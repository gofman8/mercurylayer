# Testing guide

Every end-to-end flow lives in `clients/tests/rust` and runs against the local regtest stack. The
dispatch in `clients/tests/rust/src/main.rs` is a flat numeric switch on an environment variable —
one flow per process, no test harness — so each flow below is a single `cargo run` with `SDK_E2E=n`,
`RGB_E2E=n` or `LN_SMOKE=1` set. Two suites do **not** need the stack: the `ci-guards` crate (repo
invariants) and the pure unit tests.

**An unknown id is an error, not a fall-through.** The dispatch is a chain of
`if VAR == Ok("N") { …; return }`, so a number with no branch would otherwise drop into the default
upstream sequence and print "completed successfully" for a test that does not exist. `main.rs` ends
that chain with an explicit refusal: any `SDK_E2E` / `RGB_E2E` value that matched no branch returns
an error naming the id.

## Stack

```bash
cd rgb-lightning-node && ./regtest.sh start                              # bitcoind + electrs + RGB proxy
cd mercurylayer && docker compose -f docker-compose-lockbox.yml up -d    # SE + lockbox + Postgres + vault
```

`docker-compose-lockbox.yml` brings up `mercury-server`, `lockbox`, `db_server`, `db_lockbox`,
`vault` and `web`. The coordinator answers on `http://127.0.0.1:8000`, the lockbox on
`http://127.0.0.1:18080`, electrs on `tcp://localhost:50001`, the RGB proxy on port 3000.

Environment, from `clients/tests/rust`:

```bash
export ML_NETWORK=regtest                                     # selects regtest.Settings.toml
# UTEXO_ATTESTATION_IDENTITY is no longer required on regtest — the pin is compiled in. See below.
export RLN_BITCOIND_CONTAINER=rgb-lightning-node-bitcoind-1   # the test faucet (this is also the default)
# Flows that shell out to the RLN stack (the Lightning group, and chaos22's miner):
export RLN_REGTEST=/path/to/rgb-lightning-node/regtest.sh
export COMPOSE_FILE=/path/to/rgb-lightning-node/compose.yaml
export COMPOSE_PROJECT_NAME=rgb-lightning-node
export RLN_BIN=/path/to/rgb-lightning-node/target/debug/rgb-lightning-node   # LN_SMOKE and the LN flows
```

> **The regtest attestation identity is now COMPILED IN.** The client verifies the enclave's
> `utexo/sig_count/v2` attestation over `(statechain_id, num_sigs, sig_budget, nonce)` against a
> pinned identity, and resolution is compiled-in pin → config → **refuse**; it never falls back to
> the key the coordinator serves. `TesrParams::attestation_identity_const("regtest")` now returns
> `REGTEST_ATTESTATION_IDENTITY` — the identity derived from the dev seed this repo commits for its
> own stack — so the harness no longer has to supply one, and a stack running the committed seed
> passes without configuration. It still returns `None` for mainnet, testnet, testnet3, testnet4 and
> signet, where no enclave is provisioned.
>
> **A compiled-in pin is not overridable.** Exporting `UTEXO_ATTESTATION_IDENTITY` with a value that
> disagrees with it is an ERROR, not an override — so a stale export left over from before this
> landed will fail every laddering claim rather than being ignored. Unset it, or set it to the value
> `GET /attestation_identity` reports. A differently seeded lockbox still refuses rather than passes,
> which is the property the pin exists for. (`regtest.Settings.toml` carries an
> `attestation_identity` line, but that file is read by `ClientConfig::load` — the `mercuryrustlib`
> lane; an SDK wallet is built through `ClientConfig::from_params` from `SdkConfig`, which never
> reads a Settings file.)
>
> **The pin also decides a carrier's colour.** `SdkConfig::colored_ladder` READS
> `attestation_identity_const` rather than stating a bool, so pinning regtest turned the coloured
> ladder ON for every regtest wallet that does not set the flag itself — see below.
>
> **Toolchain.** `clients/tests/rust/rust-toolchain.toml` pins 1.83.0; run with `cargo +stable run`,
> because `rgb-lib` is edition 2024. `rgb-lib` is a **git** dependency pinned by revision in
> `clients/libs/rust-rgb/Cargo.toml` — a clean clone builds without any sibling checkout. To develop
> the fork, add a `[patch."https://github.com/gofman8/rgb-lib"]` entry to a git-ignored
> `.cargo/config.toml` rather than editing the manifest.
>
> **Working directory.** Each flow wipes `wallet.db*` and its RGB data dirs in the *current* working
> directory at start. Two runs sharing a CWD clobber each other — run parallel suites from separate
> directories.
>
> **Core 28+ is required**: every ladder tier is a v3/TRUC transaction with a P2A anchor.
>
> Flows that read or age coordinator state shell out to `docker exec mercurylayer-db_server-1 psql`;
> flows that mine or fund shell out through `RLN_REGTEST`.

## What the flows exercise

There is **one protocol** and **one lane**: every coin's exit material is its TES-R ladder, and there
is no flat backup lane for a flow to be on instead. What a flow's assertions are about is therefore
*where* in a ladder its coins sit (root, split child, spine tip) and what colour the rungs are — and
since `colored_ladder` reads the network's pin, the colour is decided by the wallet's config rather
than by the flow's subject matter. Full treatment in [PROTOCOL.md](../spec/PROTOCOL.md).

### Laddered at first sight — every deposit

The deposit watcher establishes the exit ladder at the **first mempool sighting** of the funding
transaction, before it confirms: funding `F` → **trigger** `T` (no timelock, signed once at sight) →
**extension** `X_m` (relative CSV `E_m`) → **state** `S_k` (relative CSV `Δ_k`). Under
`coin_status::check_deposit`'s own lane (`LadderAtSight::Plain` — the `update_coins` default, which
the `mercuryrustlib`-direct flows and the upstream suite drive) the watcher ladders the coin itself;
under the SDK's `claim()` the watcher runs as `LadderAtSight::Defer` and `claim()`'s establish pass
ladders every un-laddered `IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED` coin in the same pass, plain or
coloured. All three tiers are v3/TRUC with a P2A anchor, and all three stay **un-broadcast**. There is
no protocol-version field and no escape-hatch environment variable; no flow pins any other lane.

What that means when you write or read a flow:

* **The enclave count after a deposit is 3** (`T`, `X_0`, `S_0`) and there is no fourth co-sign: no
  flat `tx1` is signed at deposit (`create_tx1` is deleted), the coin holds zero backup rows and
  `coin.locktime` is `None`. Confirmation adds nothing — the same trigger is on disk before and after
  the block (sdk48, tb01, sdk74; all re-derived, pending run). A flow that used to `establish` after
  `claim()` must **load** the deposit's ladder instead (`tesr::load`): a second establishment is a
  rival trigger over `F` and three co-signs no census can balance.
* **No flow can "wait out" a CSV deadline.** Time is driven by broadcasting a trigger and then mining
  past each relative timelock (sdk40, sdk50). Mining at an idle coin does not move the CSV clock —
  sdk30 (a) mines 300 blocks and asserts the exit chain is byte-identical afterwards, `F` still
  unspent.
* **There is no second clock to measure.** Nothing on a coin matures on its own: no absolute
  locktime exists anywhere in its material, INV-27 is unconditional, and a *received* coin sits on
  nothing either — `min(L_k)`, the epoch, and "`interval` per hop" describe material that is no
  longer built. sdk86 is re-derived to exactly that: a received coin over two hops and 300 idle
  blocks carries no flat row and `locktime == None` at k = 0, 1, 2, and its ladder is byte-identical
  (pending run); sdk30 (a) proves the CSV half at k = 0 today.
* **A transfer co-signs a fresh state one δ LOWER** than the one it replaces
  (replace-by-lower-timelock), so the new owner's state always matures first; the replaced state is
  disclosed as superseded and counted by the receiver's census (sdk41, sdk49, sdk54). The message
  conveys **no** `backup_transactions` — the receiver requires the vector to be empty
  (`verify_flat_backup_lane`), and the census is `se_num_sigs == tiers + superseded` with the flat
  term pinned to 0 (`PARENT_V2_BASELINE = 0`).
* **Renewal (a lower-CSV extension) and rollover (a fresh level) are off-chain and unbounded**
  (sdk43, sdk44), driven through the library calls `renew_auto` / `rollover_auto` — nothing on the
  transfer path invokes them yet, so a flow that wants a renewal calls it by hand. `refresh` is the
  **re-anchor** primitive: one on-chain tx that moves the coin to a fresh funding outpoint, where a
  new ladder is established at first sight through the same deposit path, killing every exit right
  rooted at the old `F` (sdk30 (b)).
* **A unilateral exit walks the pre-signed chain tier by tier**, waiting out each relative timelock
  (sdk50) — not a single backup broadcast, and there is no flat fallback arm: a coin with no ladder
  row is refused by name. A keyless watch bundle lets a delegated tower do the same for an offline
  owner (sdk45, sdk51, sdk72 part B); every laddered entry it exports is event-driven
  (`deadline_block: u32::MAX`, a trigger on `F`).
* **Liveness is `is_live_for_defence`** — `IN_MEMPOOL | UNCONFIRMED | CONFIRMED` — in
  `defend_ladders`, `unilateral_exit` and `export_watch_bundle`, so a ladder is defended from the
  block its deposit is first seen in (sdk79, sdk80; re-derived, pending run).

### Retired 2026-09-06: the flat lane, and what happened to its flows

The flat signed-once backup with decrementing absolute nLockTimes is gone: `deposit::create_tx1` is
deleted and no conveyance writes a backup row. So is the flat conveyance lane's licence classifier —
`assert_flat_conveyance_is_legitimate` no longer exists as a symbol at all, and `PermanentLicence`
survives only in comments; `is_legitimate_flat_reason` is a function that returns `false`
unconditionally. `unilateral_exit`'s flat fallback arm is deleted rather than guarded. What still
**refuses by name** is: a conveyance of a coin with no ladder (`transfer_sender::execute_ex`, before
any SE co-sign), a conveyed non-empty `backup_transactions` vector
(`tesr::verify_flat_backup_lane` / `refuse_conveyed_flat_backups`), the off-chain branch
split/combine (`register_split_subcoins_n`, `register_combine_subcoins`, and
`refuse_legacy_colored_split_lane` ahead of both on either setting of `colored_ladder`),
`unilateral_exit` on a coin with no ladder row, and `broadcast_backup_tx` on a laddered coin. A
carrier that cannot be coloured (below the coloured floor, RGB state unavailable, no pin) is not "on
the flat lane" any more: it has **no exit material** until a later pass colours it
(`LadderSkipReason::RgbCarrier`), and it cannot be conveyed. `migration_hatch_verdict` still
classifies such carriers read-only, but the hatch is CLOSED with the lane — the classifier's verdict
is now only used to word the refusal.

Every flow whose *subject* was that lane is therefore in one of two states: DELETED, or re-derived.
**A re-derived flow is not evidence until it has been run**, and a partial run has now happened.
Commit `dd03ab2` records **`SDK_E2E` 15, 37, 40, 42, 44, 45, 48, 49, 50, 58, 59, 60, 71, 74, 76 and
85 passing on the live regtest stack**, alongside the unit suites (mercuryrustlib 387, the SDK 152,
ci-guards 32). Every other re-derived flow — and every re-derived upstream and `RGB_E2E` flow — is
still **pending run**, and is marked so below. A `git diff` of `clients/tests/rust/src/` against the
pre-rule tree is the authoritative list of what has been re-derived; the commit trailers are the
authoritative list of what has been RUN.

* **DELETED — the subject no longer exists, and neither does the flow.** `RGB_E2E=1, 2, 3, 5, 6, 8,
  9, 10` (the branch lane: un-broadcast coloured split/combine over `F`, exited by broadcasting the
  branch), plus `SDK_E2E=73` and `SDK_E2E=78`. Their source files are gone from
  `clients/tests/rust/src/` and their dispatch arms are gone from `main.rs`, so **those ids no longer
  exist**: running one now hits the unknown-id refusal at the end of the chain. Do not put them in a
  run list. What they used to prove is stated here rather than left implied: the branch lane refuses
  (`register_split_subcoins_n` / `register_combine_subcoins`); `sdk73`'s crash-and-replay had nothing
  left to crash, and the live coloured in-ladder split has **no structural-spend journal and no crash
  point** — a stated KNOWN GAP in the code, recoverable by hand, not a loss of funds, and now with no
  test of any kind against it; `sdk78`'s subject (an un-colourable carrier's migration hatch) closed
  with the lane, which leaves `materialise_carrier` and the un-colourable-carrier refusals
  **UNPROVEN end to end**.
* **Re-derived to the new shape.** The flows whose former subject was the flat lane
  itself. **RUN and passing** (`dd03ab2`): `sdk48` (`num_sigs == 3`, laddered while still
  `IN_MEMPOOL`), `sdk71` (every conveyance licence refuses; skip reasons are diagnostic only),
  `sdk74` (both coins laddered at first mempool sight, coloured where the allocation is booked) and
  `sdk76` (a received parent conveys an EMPTY parent chain). **Still pending run**: `sdk46` (the
  census at first sight, flat term 0), `sdk55` (the flat term is identically zero and cannot be padded — a conveyed
  backup is refused before the census runs), `sdk82` / `sdk88` (a conveyed child has no epoch
  to run out of — the headroom gate is not consulted, and the bound is the fixed-window depth cap),
  `sdk86` (no calendar clock on a received coin over two hops), `sdk34` (a received coloured child
  has no clawback deadline; its defence is event-driven), `sdk87` (the deadline pass leaves a
  laddered carrier alone at any margin), `sdk39` (a depth-2 token piece is walked out on the
  coloured lane, not materialized), `tb01`, `tb05` and `ta03` (the upstream suite on the one coin
  shape: three co-signs at every status; the stale-state defence by relative timelock; duplicates
  refused at conveyance and recovered by withdrawal), `RGB_E2E=4` and `7` (re-derived over
  `single_use` deposits — the one deposit shape that gets no ladder, because the SE refuses a second
  co-sign on it — so they pin the coordinator's single-use and epoch gates without any exit material
  at all), and the chaos cheats (`chaos22_cheats`: the captured stale material is now the coin's
  current ladder state, since there is no backup to capture). Plus the flows touched only to assert
  the three-co-sign shape beside what they already proved — `sdk17`, `sdk30`, `sdk32`, `sdk40`–`sdk50`,
  `sdk53`, `sdk54`, `sdk56`–`sdk60`, `sdk63`, `sdk64`, `sdk68`, `sdk70`, `sdk72`, `sdk77`,
  `sdk79`, `sdk80`, `sdk84`, `ta01`, `tv01`, `RGB_E2E=11–13`; of those, `dd03ab2` ran and passed
  `sdk15`, `sdk37`, `sdk40`, `sdk42`, `sdk44`, `sdk45`, `sdk49`, `sdk50`, `sdk58`, `sdk59`, `sdk60`
  and `sdk85`, and the rest are pending.

  **The leaf bands (REQ-83) are a separate pending item.** A payee's leg can now be a one-rung
  `ThinPiece`, a `Ladderless` stub or a sub-dust `Tail`, and the plain root lane builds all three —
  but no E2E flow in this suite exercises a payment below `min_child_value`. The bands' arithmetic is
  covered by `mercurylib`'s pure tests (including the exhaustive
  `every_value_gets_a_role_that_can_afford_its_own_floor` sweep); their end-to-end behaviour is
  **UNPROVEN**.

**Which colour a flow's carrier gets is a property of its own config, not of the test's subject
matter.** `SdkConfig::regtest(..).colored_ladder` reads the compiled-in regtest pin, so it is
**true**, and a flow that does not set the flag inherits the coloured ladder. Flows that ask for it
by name — sdk02, sdk29, sdk31, sdk32, sdk34, sdk74, sdk75, sdk77, sdk79, sdk87, sdk88 — are
unaffected; a grep for `colored_ladder = true` under `clients/tests/rust/src/` is the authoritative
list. (RGB_E2E=15/16 exercise the coloured tier builder directly on a reduced stack and never build
an `SdkConfig`, so the flag does not reach them.) The RGB flows that *inherit* the default (sdk09,
sdk16, sdk39, sdk52) inherit the coloured one. Refusing the combination of a coloured ladder
and the legacy coloured split lane is `refuse_if_colored_ladder`; under the rule
`refuse_legacy_colored_split_lane` refuses the legacy lane on **both** settings of the flag, before
that check is ever reached.

### Non-exact payments — the in-ladder split

A payment that is not an exact subset of the sender's laddered coins runs the **in-ladder split**: a
state tier `SP` spending `X_m.out[0]` — a *descendant* of the trigger, never a rival for `F` —
paying a piece leg and a change child. **The payee's admission floor is 1 satoshi** (REQ-83,
`SplitLegRole::Tail.min_value`): `mercurylib::tesr::LeafShape::for_value` picks the leg's shape from
its value, and the SDK admits at the cheapest band. At the shipped `committed_fee_rate = 3.0` sat/vB
(`TIER_VBYTES` 125, `P2A_VALUE` 240, `DUST_LIMIT` 330) the band boundaries are `min_child_value`
= **1 560** (two rungs, `Piece`), `min_spine_tip_value` = **945** (one rung, `ThinPiece`),
`DUST_LIMIT` = **330** (`Ladderless` stub, no rung and no SE slot) and 1 (`Tail`, sub-dust). Both
boundaries are *functions of the rate*, not constants. The sender's change leg still carries a
ladder: `max(min_split_output(backup_rate), 945)` on the root and spine lanes, `min_child_value` on
the child lane. **Only the plain in-ladder ROOT lane actually builds the two lower bands.**
`spine_batch_split` refuses a `Stub`- or `Tail`-band leg by name before any co-sign;
`child_in_ladder_split` does neither — it hard-codes `SplitLegRole::Piece` and never consults
`LeafShape`, so on that lane admission (1 sat) and construction (two rungs) disagree. Reported as a
code defect; no flow covers it. The parent is
terminalized and its old owner state disclosed as superseded (sdk58, sdk59). Nothing in the E2E set
has been run against the leaf bands since they landed. The
split-depth cap (`enforce_split_depth_cap`) measures the leaf's exit walk against `initlock` as a
**fixed window** — there is no epoch deadline to read off a parent chain — so a grandchild split on
a received child succeeds and conveys an empty `parent_flat_backups` (sdk17, re-derived, pending
run).

Received children are **first-class**: the claim completes the standard SE key handover, so the
receiver co-owns `A_child` (invariant across the rotation, which is what keeps the pre-signed child
tiers valid) and the sender is permanently locked out. A child pays onward off-chain whole
(`child_retransfer`) or split (`child_in_ladder_pay`, a depth-2 `ancestors` chain), gets its transfer
budget back in place via `renew_child`, and — once `SP` confirms — can be swept with its siblings by
`mercuryrustlib::combine::combine_leaves`. That last one is a **primitive with no caller outside a
test**: sdk83 drives it end to end, but nothing in the product reaches it, which is why the sweep
economics below are marked design. See [CHILDREN.md](../spec/CHILDREN.md).

## SDK end-to-end flows (`SDK_E2E=n cargo +stable run`)

**75 flows plus the `chaos22` fuzzer** — 76 live `SDK_E2E` ids in `main.rs` today, one of which
(`SDK_E2E=22`) is the fuzzer. Numbers run to 94 and are **not** contiguous, and two of the gaps are
new: `73` and `78` were DELETED with the retired lane. The full-matrix runner discovers the live set
by grepping the dispatch, so there is no list to keep in step. Rows marked *re-derived, pending run*
are explained in the section above; rows struck through are gone.

### Wallet, parity, guard rails

| n | Flow | Proves |
|---|---|---|
| 1 | `sdk01_wallet_flow` | deposit → exact-subset transfer → auto-claim → non-exact transfer (routes to the in-ladder split, sender keeps the change) → auto-claim → cooperative withdraw to L1. No sats/UTXO management surfaced to the app |
| 4 | `sdk04_adversarial` | SDK guard rails: typed `InsufficientBalance`; a split parent is terminalized by `SP` at the SE and booked WITHDRAWN, so a second full-value spend is refused twice over; claim idempotence; double-withdraw refusal |
| 11 | `sdk11_parity_methods` | parity API: identity message signing, multi-recipient sats transfer, Utexo invoices (create + fulfill), query/history |
| 12 | `sdk12_adversarial` | three independent parts on the laddered default. B: a non-exact payment lands the exact amount through `verify_child_bundle`. C: MuSig2 secnonce reuse — one `/sign/first`, two `/sign/second` — refused on the second. D: `/transfer/unlock` with a bad signature and a NULL `auth_pub_key` refused with 403 (real and unknown ids). C and D are SE/lockbox guards tested **nowhere else** |
| 15 | `sdk15_fresh_doublesign` | the honest trust floor: a *malicious SE* co-signing a RIVAL trigger over the same `F` is exactly as final as the owner's — `T` is locktime-free, so the CSV tiers break no tie and the contest degrades to a plain on-chain race |
| 16 | `sdk16_onboarding` | enter with nothing: a wallet with no deposit and zero balance receives BTC *and* an RGB asset, then exits unilaterally |
| 17 | `sdk17_oor_chain` | out-of-round transferability: alice → bob → carol where hop 2 is a **partial** re-spend of bob's received child (a child-level in-ladder split; the child becomes an intermediate `ancestors` segment and carol's exit walks a depth-2 chain). `F` stays unspent throughout. *Re-derived, pending run*: the root is asserted to carry exactly three co-signs, zero backup rows and `locktime == None`, and the grandchild bundle an EMPTY `parent_flat_backups` — the depth cap measures against the fixed `initlock` window, so the grandchild split must SUCCEED |
| 71 | `sdk71_unconditional_ladder` | *re-derived, pending run.* `claim()` ladders every coin it can with no opt-in, and there is NO lane for a coin without a ladder: every conveyance licence refuses, an unreadable ladder record is refused rather than degraded, a bindable on-chain non-carrier with no ladder is refused as a bug, and the `ladderskip-<sid>` records the pass writes are DIAGNOSTIC ONLY (`is_legitimate_flat_reason` always `false`). Every skip is surfaced as a `WalletEvent::LadderSkipped` carrying a `LadderSkipReason` (`RgbCarrier`, `LadderUnreadable`, …) instead of being silent |
| 85 | `sdk85_transfer_cancel` | all four rows of `mercurylib::transfer::cancel`'s authorization table against a live coordinator: opened-but-never-conveyed (sender alone), conveyed-and-unclaimed (sender-only REFUSED by name, then released cross-wallet with the receiver's single-use consent), claimed (terminal), batched (governed by the latch, never the sender). Then the safety step: after a consented cancellation bob can never claim and the coin lands with carol — exactly one of them is paid. **Needs a coordinator rebuilt with the transfer-cancel migrations** (`0010_transfer_cancel.sql`, `0011_transfer_cancel_sender_key.sql`), which `sqlx::migrate!` embeds at compile time; against an older container every step fails at the first `cancel_transfer` |

### Ladder consensus, lifecycle, exit, defence

| n | Flow | Proves |
|---|---|---|
| 40 | `sdk40_tesr_consensus` | the consensus core against real bitcoind: un-broadcast immunity; `X` REJECTED before `E` confirmations of `T` and `S` REJECTED before `Δ` confirmations of `X`; a full unilateral exit with no operator cooperation. **PART 2**: cooperative de-trigger defeats a hostile trigger. The blind SE is unchanged — it blind-signs v3 + relative-timelock + P2A sighashes. *Re-derived, pending run*: the ladder under test is the one the DEPOSIT establishes at first mempool sight (`LadderAtSight::Plain`), and the shared `deposit_coin` helper (used by sdk41–47/53–57/70) measures that shape on every deposit — three co-signs, no `tx1` |
| 41 | `sdk41_tesr_transfer` | a transfer really moves control: `A` and `F` are invariant (no on-chain tx), only the shares rotate; Bob co-signs and exits a full ladder over the same `F`, and Alice's later co-sign attempt is refused — she is cryptographically out |
| 42 | `sdk42_tesr_lifecycle` | wallet-level lifecycle: the ladder the deposit established → renew off-chain (`renew_auto`; the enclave count becomes 5 = 3 live tiers + 2 superseded, no flat term) → persist to the wallet DB → reload as a fresh session would → unilateral exit **from the reloaded bundle**. *Re-derived, pending run* |
| 43 | `sdk43_tesr_rollover` | when the extension-CSV budget is exhausted the ladder rolls over **off-chain** to a fresh level (zero on-chain bytes), renewal keeps working at the new level, and the deep chain still exits |
| 44 | `sdk44_tesr_params` | the canonical `TesrParams` schedule drives the deposit's own first-sight establishment (`check_deposit` → `establish_auto`, at E0/D0), then `renew_auto` / `rollover_auto` — the cadence a real wallet runs — and the resulting ladder still exits (decrement + floor + `m_max` math). *Re-derived, pending run* |
| 45 | `sdk45_tesr_watchtower` | a **keyless** WatchBundle (pre-signed tiers only, no key material — every tier pays the owner) lets a delegated tower drive an offline owner's exit after a griefer broadcasts the trigger; a second independent tower pass is idempotent |
| 46 | `sdk46_tesr_rprime` | the census `se_num_sigs == tiers + superseded`, with **no flat term**, checked against the **real** SE sig count at first mempool sight: the three co-signs on a fresh deposit are `T`, `X_0`, `S_0` and nothing else, and `verify_bundle` accepts the true count while rejecting a hidden extra signature. *Re-derived, pending run* |
| 47 | `sdk47_tesr_rprime_transfer` | a pre-established ladder carried across a transfer message and accepted by the receiver's verifier |
| 48 | `sdk48_v2_native_deposit` | *re-derived, pending run.* A fresh deposit is laddered at FIRST MEMPOOL SIGHT: `claim()` books it as `IN_MEMPOOL` and, in the same pass, establishes and persists its plain ladder (`tesr-<id>`), exiting to the wallet's seed-derived backup address; `num_sigs == 3` (`T` + `X` + `S`, no deposit backup); nothing exit-related waits for a confirmation; a second `claim()` does not double-establish |
| 49 | `sdk49_model_a_transfer` | Model A: the sender pre-signs the receiver-paying state `S'` one δ lower; the receiver verifies it exits to **its own** key, adopts the ladder, and unilaterally exits it — the end-to-end proof that the receiver gets a complete self-custodial exit chain |
| 50 | `sdk50_v2_unilateral_exit` | the public `wallet.unilateral_exit()` walks trigger → extension → state as each relative CSV matures, reporting `wait_blocks` between tiers, until the funds land at the wallet's own backup address. No absolute-locktime backup is broadcast — and none exists |
| 51 | `sdk51_v2_watchtower` | the contested case: *someone else* spends `F`, starting the CSV clock; the owner only runs `wallet.defend_ladders()` and wins because the adopted current state carries the strictly-lowest CSV. Also asserts the pass is a **no-op while the coin is idle** |
| 53 | `sdk53_v2_latch_guard` | a Lightning-latched transfer of a laddered coin OPENS — the SSP's pre-pay census (`peek_pending_transfers` → `ssp::execute_pay`) is what stands in the way of a rogue SSP, not a blanket refusal. The happy path is sdk63 |
| 54 | `sdk54_verify_bundle_adversarial` | the anti-theft count cannot be padded: `expected = tiers + superseded_states + superseded_extensions` (flat term 0), with superseded entries parsed, ladder-linked and signature-checked, and a `csv: None` not skipping the race check. Each attack that inflates `expected` to hide a low-CSV self-paying state is REJECTED while the honest bundle verifies |
| 55 | `sdk55_backup_chain_adversarial` | *re-derived, pending run.* It used to pad (duplicate `tx1`) and invert (sender keeps the lower locktime) a conveyed flat backup chain and rely on `validate_backup_chain_v2`'s INV-5. Now, against the REAL conveyed bundle of a real hop: (a) **padding** — a flat backup conveyed beside the ladder is refused BY NAME before the census runs (`verify_flat_backup_lane` on the root lane, `refuse_conveyed_flat_backups` on the child/tail/stub lanes) and the census does not balance with a flat term of one; (b) **inversion** — a disclosed rival cannot be placed below the live state |
| 56 | `sdk56_keystone_retry_idempotent` | the signing round is idempotent under retry: re-sending the exact same `/sign/second` returns the **identical** partial signature from cache and does **not** advance `sig_count` — a lost response cannot leave the count ahead of the disclosed tier set and brick the census |
| 57 | `sdk57_owner_share_binding` | the server records an **authoritative** aggregate per `statechain_id` (owner share + enclave share) and `/info/statechain` returns it equal to the coin's own aggregate x-only — the anchor that stops a rogue-key decoy defeating the child census |
| 70 | `sdk70_verifier_binding_adversarial` | three properties against real co-signed ladders. **A**: every chaining site reads an explicit `payload_vout` accessor, and a bundle declaring the wrong one is REJECTED with a named error — never accepted, never a panic, never a silent fall back to `output[0]`. **B**: a genuine, fully co-signed DECOY ladder over an attacker-owned outpoint that exits to the victim's key and balances the victim's `num_sigs` is accepted by `verify_bundle` and must be rejected by `verify_bundle_bound`. **C**: one co-sign, one census slot — a repeated genuine disclosed tier cannot be double-counted |
| 72 | `sdk72_watchtower_failloud` | the watchtower is never silently idle. **A**: a real enumeration failure (the RGB data dir is a regular file) makes `auto_exit_due` return `Err` with a `WatchtowerBlind` event and a retained, pollable `WatchtowerFault` that clears itself once repaired — instead of an empty carrier set that reads as "nothing to protect". **B**: sdk51's attack with the manual pass deleted — the owner only calls `start_background()` and the coin must still exit to the owner's key |
| 86 | `sdk86_received_coin_ages` | *re-derived, pending run.* INV-27 on a RECEIVED coin, the shape the deposit-only flows (sdk30 (a), sdk48) structurally cannot reach: two hops and 300 idle blocks change nothing about when, or whether, the coin can exit — the exit chain is byte-identical, `F` unspent, and at k = 0, 1, 2 the coin carries no flat row and `locktime == None`. It used to measure the other answer (`L1 == L0 − interval`, `left_after + 300 ≤ left_before`) off flat backup rows that no longer exist |
| 89 | `sdk89_plain_detrigger` | the PLAIN de-trigger through the wallet API: a griefer broadcasts alice's un-timelocked `T`, and `detrigger_to_owner` spends `T.out[0]` with **no** relative timelock, so it confirms ahead of every pre-signed extension. The coloured precondition is checked before anything is broadcast, so a coloured coin can never take this path |
| 92 | `sdk92_witness_binding` | REQ-57 witness binding, live: a real laddering claim with the client's disclosure attached signs, and a tampered disclosure is refused by the running lockbox — a binding that accepted everything would be indistinguishable from no binding |

### The conveyance window

| n | Flow | Proves |
|---|---|---|
| 90 | `sdk90_transfer_window_lapse` | the payer's own software stops them twice — the wallet's coin lookup and the sender-side outstanding-conveyance refusal (`refuse_outstanding_conveyance`). Both are LOCAL gates, so this flow reaches no conclusion about the server. It also asserts the safety invariant that matters regardless: the payee's conveyed leaf stays claimable and worth what it was worth |
| 91 | `sdk91_malicious_payer_window` | the payer who skips their own client and POSTs `/sign/first` with their own genuine `signed_statechain_id`. **Inside** the coordinator's one-hour window (`OPEN_TRANSFER_WINDOW_SQL`, `server/src/database/transfer_sender.rs`, non-batch branch) the probe is asserted to get **HTTP 409**; **outside** it — after the transfer row is aged past `updated_at` — the coordinator issues a session, **HTTP 200 with a `server_pubnonce`**, which is RECORDED rather than pinned. That is the only server-side gate on this path. Setting `EXPECT_LATCH=1` converts the recording into an assertion the day REQ-61's owner latch ships |

Both flows age the transfer row through `docker exec … psql`; the one-hour non-batch branch is
hard-coded, so it cannot be waited out in-process. See
[TRUST-MODEL.md](../spec/TRUST-MODEL.md) for the scope of what a lapsed window does and does not
mean — a `sign/first` session is the first link of a chain, not a completed theft.

### In-ladder split, children, leaves

| n | Flow | Proves |
|---|---|---|
| 58 | `sdk58_inladder_split` | `verify_child_bundle` ACCEPTS a real split child — `SP` is a state tier spending `X_m.out[0]`, the parent is terminalized and `S_0` disclosed as superseded, and the child's two-aggregate bundle (ancestors under `A_parent`, child tiers under `A_child`) checks out against chain + `/info/statechain`. Eleven adversarial census cases are REJECTED, including a non-terminal parent and a hidden lower-CSV state. *Re-derived, pending run*: the parent is the deposit's own first-sight ladder (`num_sigs == 3`, flat term 0), loaded rather than re-established |
| 59 | `sdk59_inladder_pay` | the split is a usable **payment** through `transfer()` / `claim()` / `unilateral_exit()`: `transfer()` auto-routes to it, the piece child pays Bob (Model A) and is conveyed to his mailbox, the change child pays Alice back; Bob adopts via `verify_child_bundle` (parent `F` on chain, parent terminal, an EMPTY conveyed parent chain) and exits the child to his own key. *Re-derived, pending run* |
| 60 | `sdk60_child_firstclass` | a **received** child re-transferred WHOLE off-chain (alice → bob → carol): the claim completes the key handover so `A_child` is invariant and Alice is locked out; `child_retransfer` co-signs a fresh state at a strictly lower CSV and discloses the replaced one; Carol's census counts the child-superseded segment and she exits. Two payments, zero on-chain footprint. *Re-derived, pending run*: the root is asserted at exactly three co-signs and zero backup rows |
| 69 | `sdk69_transfer_many_inladder` | `transfer_many` on a LADDERED parent. It used to call `split_coin` and assert the refusal; with that route DELETED the guarantee became **structural** — a deleted route cannot be taken by a caller who forgets to check — so the flow now proves the positive half: one `SP` over `X_m.out[0]` carries two recipient children + change + P2A with `F` untouched. The trigger-race attack is then executed for real — alice broadcasts her retained, un-timelocked trigger and spends `F` — and both recipients still exit unilaterally for their exact amounts, because `SP` descends from that trigger instead of racing it. That trigger is exactly the [B1] weapon the plain split had no answer to |
| 76 | `sdk76_received_parent_split` | splitting a RECEIVED laddered coin. *Re-derived, pending run*: a laddered coin carries NO flat backup at deposit or at any hop, so a parent received `k` times conveys an EMPTY parent chain and the ancestor census's flat term is 0 for every `k` — not `1 + k`, as the retired flat chain made it. Bob's count is asserted from his own rows (zero), then he pays carol non-exactly and carol's child must be adoptable. sdk58/59/69 all deposit the parent, so `k = 0` and are blind to this |
| 80 | `sdk80_plain_child_split_watchtower` | `child_in_ladder_pay_many` conveys every grandchild before writing the child's durable status, and the child's own record is never rewritten — so this wallet's watchtower loop is admitted to drive a superseded state while strangers already hold the bundles that supersede it. The ordering must close that window. *Re-derived, pending run*: the loop's one filter is L1 = `is_live_for_defence` |
| 81 | `sdk81_inladder_split_recovery` | an in-ladder split killed by SIGABRT (`UTEXO_CRASH_POINT=after_inladder_sp_sign`) the instant `SP`'s co-signature is journalled — before either child ladder exists — leaves the parent PERMANENTLY terminal at the SE. `recover_in_ladder_splits` replays the write-ahead journal, completes both children, and the original payment still lands |
| 82 | `sdk82_exit_headroom_gate` | *re-derived, pending run.* A conveyed child has NO EPOCH TO RUN OUT OF: the flat backup that used to mature at `H_deposit + initlock`, spend `F` and void the tree does not exist, so a payment from a coin aged past `initlock` is admitted exactly like a fresh one (a control payment and an aged one both adopted), `check_exit_headroom_with_margin` is not consulted, and the admission bound is the split-depth cap against `initlock` as a FIXED window — a property of the child's SHAPE, read from the SIGNED `nSequence` of every tier, not of the calendar |
| 83 | `sdk83_leaf_combine` | `mercuryrustlib::combine::combine_leaves`: one spine batch carves five leaves, four of them the recipient's; the shared prefix `T → X_m → SP` is walked on chain until `SP` CONFIRMS, then THREE of those four are swept into ONE consolidated UTXO by a single 3-input transaction carrying no timelock, and the fourth — deliberately left out — still exits on its own pre-signed tiers. The refusals are exercised for real: over an un-broadcast `SP` (blind), over a mempool `SP` (replaceable), and over a coloured leaf (allocation-destroying), each side-effect free |
| 84 | `sdk84_leaf_renewal` | a leaf's transfer budget is replenishable: `renew_child` rebuilds both leaf tiers IN PLACE over the same `SP.out[j]` for zero on-chain bytes and no depth. The flow walks a leaf through every hop of every epoch on the regtest schedule, renewing between them, and settles the safety property on chain — the transaction that finally takes `SP.out[j]` is the RENEWED extension, and every superseded one loses the maturity race. The refusals must name the right remedy, and a renewal that does not strictly lower the extension rung is refused with no co-signature burned. *Re-derived, pending run*: the leaf's census is `2 + superseded` and it conveys an empty parent chain |

### Lightning

Both directions run on the ladder through a **HODL-invoice latch** — see
[LIGHTNING.md](../spec/LIGHTNING.md). The LN-latched piece is the one case that stays terminalized
(it sits unclaimed past the pending-transfer lock's window). The latch no longer requires the coin to
carry a `locktime`.

| n | Flow | Proves |
|---|---|---|
| 19 | `sdk19_receive_failure` | RECEIVE that is never paid: no LN payment ⟹ the SE does not reveal the preimage, the receiver cannot claim, the SSP keeps its (reclaimable) coin |
| 20 | `sdk20_adversarial_gate` | the SSP pre-payment gate over the live SE + live RLN: a coin latched to a **third party** and an **undersized** coin are both refused; no LN payment goes out and the merchant invoice does not settle |
| 21 | `sdk21_remote_sspclient` | the same `pay_lightning_invoice` / `create_lightning_invoice` calls against a **deployed** `mercury-ssp` HTTP server — serialization, the `{error:..}` contract, background settle spawn, DB isolation |
| 23 | `sdk23_rgb_ln_swap` | RGB assets over Lightning: issue → colored channel → asset invoice → decode → pay; asset balances shift by the exact amount |
| 24 | `sdk24_receive_cancel` | the HODL **cancel** leg: the payer pays (HTLC parks HELD, status `Claimable`), the SSP aborts *before* confirming the latch → `/cancelhodlinvoice` fails the HTLC back and the payer is refunded immediately |
| 25 | `sdk25_receive_delayed_claim` | the receiver who stalls past the SE latch window gets **nothing**: the claim gate and the SSP's `get_preimage` are bound to the same expiry, set shorter than the HODL HTLC, so the coin stays with the SSP and the payer is refunded |
| 63 | `sdk63_v2_lightning_pay` | **exact PAY** from a laddered coin: the SSP's pre-pay census (`verify_bundle` over the conveyed ladder, `num_sigs` from the attested enclave sig-count) runs before `send_payment`. Alice deposits the exact invoice amount, so no split is involved |
| 64 | `sdk64_v2_lightning_receive` | **exact RECEIVE** into a laddered coin: the SSP fronts its own coin under a HODL invoice and the SE reveals the preimage only once the payee's coin is claimable — the SSP owns the coin throughout its risk window, so the receive direction needs no operator trust |
| 65 | `sdk65_inladder_lightning_pay` | **non-exact PAY** via a latched in-ladder split: the piece pays the SSP and is latched to the invoice hash, the change stays Alice's, and the SSP runs `verify_conveyed_child` on the CHILD bundle before `send_payment` |
| 66 | `sdk66_inladder_pay_failure` | non-exact PAY failure → clean **rollback**: an unroutable invoice after the split + conveyance restores the parent as exitable and drops the piece plus the optimistic change |
| 67 | `sdk67_inladder_lightning_receive` | **non-exact RECEIVE**: the SSP holds only a large laddered coin, so `create_receive` falls back to an in-ladder split and conveys a piece worth the invoiced amount under an SE-minted preimage; `settle_receive` releases the piece and claims the HTLC |
| 68 | `sdk68_v2_pay_failure_reclaim` | exact whole-coin PAY failure → clean **reclaim**: the orphan `S'` co-sign inflates `sig_count`, so `reclaim_lightning_payment` restores the coin locally as exitable instead of self-transferring; the value is fully recoverable and re-transfer is unblocked by a `refresh()` |

### RGB tokens and the coloured lane

| n | Flow | Proves |
|---|---|---|
| 2 | `sdk02_token_flow` | issue 1000 TKN (RGB NIA) onto a statechain coin → pay bob 250 off-chain over the **coloured in-ladder split**: a coloured child carved out of `SP`, conveyed with the consignment riding the transfer message; bob validates it off-chain (un-broadcast witness chain) and books under the **verified** contract id (750/250). It closes on the negative: bob's cooperative sweep correctly sweeps **zero** coins, leaving the token coin alone. Zero on-chain cost per payment |
| 9 | `sdk09_ifa_batch` | IFA (inflatable) issuance, on-chain mint bound to a new statechain coin, and a batch multi-recipient transfer in **one** coloured split, each receiver validating its own consignment amount |
| 29 | `sdk29_granularity_tokens` | raw-unit precision on the coloured lane: three wallets paid in ONE in-ladder split down to 1 raw unit (`precision` is contract metadata the SDK never scales), each booking what its own consignment assigns. A received coloured child **cannot be subdivided at all** — `ChildTesrBundle::colored_child_seals` refuses it structurally, not arithmetically. A fully-spent carrier leaves **no plain BTC change**: `F` is wholly consumed by `T`, and `colored_in_ladder_pay` carves no change child when the allocation is fully paid out, so conservation is asserted as `Σ children == colored_tier_out_total`. Paying more than any single carrier holds SUCCEEDS via `colored_multi_carrier_transfer` (one split per carrier, N pieces). Plus idempotent double-receive, and a five-tier coloured child exit measured with the read-only `color_psbt` stock probe |
| 31 | `sdk31_token_combine` | an amount spanning several carriers, on the coloured lane — and **there is no combine transaction**. Each carrier's `F` is already spent by its own trigger, so the payment is one in-ladder split per carrier, each conveying a coloured child; the recipient's declared shares sum to the amount paid (bob 100 / alice 10) from two DIFFERENT carriers. The invalidation property the old N-terminal-ancestor rule enforced is asserted **per leg**: every `SP` spends its parent's `X_m` payload output and never `F`, and every source carrier is terminal at the SE. A leg that skipped terminalisation fails |
| 32 | `sdk32_token_over_time` | tokens are never lost by inactivity, on the coloured lane. The carrier IS laddered and every rung carries a valid RGB state transition (`bundle.is_colored()`, `colored_ladder_health` validating the full allocation against the ladder's own un-broadcast txids). An idle coloured ladder never ages — no tier reaches the chain, `F` stays unspent. The invariant is proved positively rather than by absence: the RGB-unaware routes to a carrier are each refused by name — plain-BTC coin selection, the uncoloured in-ladder split (`refuse_uncolored_over_colored`), and the flat conveyance (which now refuses every coin) |
| 34 | `sdk34_token_watchtower` | *re-derived, pending run.* A RECEIVED token piece has NO calendar deadline, and its defence is EVENT-DRIVEN: `auto_exit_due` leaves it alone at any margin (the leaf near-deadline loop is deleted), and `defend_ladders`' per-block child loop answers a hostile trigger on the shared funding output `F` — nothing else ever needs to happen. Two earlier shapes are recorded in its header: the flat-lane branch broadcast before `L0`, then the coloured child walked out before `L0 − Σcsv`, both against a sender's retained flat backup that no longer exists |
| 39 | `sdk39_depth2_token_exit` | *re-derived onto the coloured lane, pending run.* A token piece TWO coloured splits deep is exited (walked) on chain end to end with its allocation preserved. Depth 2 now arises as in-ladder splits — alice's first `transfer_tokens` carves bob a coloured child (depth 1) off `SP_1` and leaves alice a spine tip; the second pays from that tip — and the exit is the walk, not a branch broadcast root-first (the legacy branch lane `register_split_subcoins_n` refuses) |
| 52 | `sdk52_v2_rgb_carrier` | terminal-freeze honoured by COLOUR: in one wallet the plain deposit carries a PLAIN ladder, the RGB carrier carries a COLOURED one, and an off-chain token transfer still settles 750/250 — the two ladders coexist. It sets no `colored_ladder`, so it inherits the regtest default, which the pin turned ON. (Its earlier shape — "the carrier carries no ladder" — is the shape a network without an enclave has, and under the rule that shape has no exit material rather than a flat one) |
| ~~73~~ | `sdk73_structural_recovery` | **DELETED — `SDK_E2E=73` no longer dispatches.** It killed the legacy coloured **branch** split (`create_colored_split_tx` over `F`, journalled as `lane = "colored_split"`) at `UTEXO_CRASH_POINT=after_structural_sign` and replayed it through `recover_structural_spends` → `register_split_subcoins_n`, which now refuses. The live coloured in-ladder split (`colored_in_ladder_transfer`) has **no structural-spend journal and no crash point** — a stated KNOWN GAP in the code, recoverable by hand, not a loss of funds — so the F7 property cannot be measured against it and now has no test at all; a replacement is owed the moment that lane journals. The plain in-ladder counterpart with a journal is sdk81 |
| 74 | `sdk74_colored_ladder` | *re-derived, pending run.* With `colored_ladder` ON, BOTH coins are laddered AT FIRST MEMPOOL SIGHT, before any block is mined — a PLAIN `tesr-` row behind the deposit, a COLOURED one behind the issuance's carrier (an issuance books its allocation at broadcast, so the coloured `T` is built over an UNCONFIRMED `F`). At that instant each coin's `num_sigs` is exactly 3, with zero backup rows and `locktime == None`; confirmation changes nothing. Every coloured tier carries a valid RGB state transition: one `OP_RETURN` per tier at vout 0, payload at vout 1, P2A at vout 2, `payload_vout` threaded from the builder's returned index, each tier chaining through its parent's DECLARED payload vout, and the committed fee exactly `committed_fee_for_outputs(n + 1, rate)`. The census balances at `3 == flat_backups(0) + tiers(3) + superseded(0)` and is refused with a flat term of 1 |
| 75 | `sdk75_colored_exit` | the unilateral exit of an RGB allocation: `T → X_0 → S_0` all CONFIRMED (not merely mempool-accepted), each spending its parent's declared payload vout, with the allocation intact at the end — no SE, no counterparty, only blocks |
| 77 | `sdk77_colored_inladder_split` | a coloured carrier pays PART of its allocation: a coloured `SP` over `X_m`'s payload output (a descendant of `T`, not a rival of it) with a headless coloured ladder per child, so five coloured tiers stand between `F` and the recipient's key |
| ~~78~~ | `sdk78_uncolourable_carrier` | **DELETED — `SDK_E2E=78` no longer dispatches.** Its subject (an un-colourable carrier's migration hatch onto the legacy lane) closed with the lane itself. Nothing now measures the un-colourable class end to end: `materialise_carrier`, the by-name refusals in `unilateral_exit`, and "repeated `claim()` passes must not colour a carrier below the coloured root floor" are all **UNPROVEN**. `RGB_E2E=16` still reproduces why such a carrier cannot be coloured, on a reduced stack |
| 79 | `sdk79_split_watchtower` | the sender's own watchtower must not destroy the recipient's allocation. `colored_in_ladder_pay` stores the terminalized parent segment back to the sender's own record BEFORE conveying anything, so the sender's row names `SP` — byte-identical to the state in the recipient's conveyed child — and the replaced `S_0` appears among the superseded states rather than in the exit chain. *Re-derived, pending run*: `defend_ladders` keys on L1 = `is_live_for_defence`, and that event-driven pass is the ONLY thing defending a laddered coin |
| 87 | `sdk87_carrier_deadline` | *re-derived, pending run.* The CARRIER variant of the deadline pass leaves a laddered carrier ALONE at any margin: `coin.locktime` is `None`, so `coin_near_final` — the predicate both routes of `deadline_safety_due` select on — is never true and "near its deadline" is not a state a laddered coin can be in. The RGB-safe sever it used to reach for (the coin's own pre-signed `T`, which does not re-aggregate) is still there when the OWNER asks for it by name (`sever_from_f`), with the allocation intact |
| 88 | `sdk88_carrier_headroom` | *re-derived, pending run.* The CARRIER variant of sdk82: a coloured child has NO EPOCH to fit inside, so a payment from a carrier aged past `initlock` is admitted like a fresh one. The rule is colour-blind — nothing on any coin carries an absolute calendar — and the lanes differ only in what a calendar would have cost (an asset, not sats). The earlier claim that a coloured chain is LONGER is retracted in its header |

### Operations: re-anchor, fees, sponsorship

| n | Flow | Proves |
|---|---|---|
| 30 | `sdk30_refresh` | **(a)** mine 300 blocks at an idle k=0 deposit and the exit chain is byte-identical (same txids, same relative CSVs), `F` still unspent, balance untouched — "idle coins never age", which under the rule is the whole statement: there is no calendar half. **(b) re-anchor** — `refresh` cooperatively spends `F` into a brand-new aggregate, minting a new statechain id with its own fresh ladder (established at first sight of the new funding tx) and permanently killing every exit right rooted at the old `F`. User-pays mode (fee deducted from the coin) |
| 36 | `sdk36_derived_tokens` | split / refresh slots are **free derived slots** (`POST /deposit/get_derived_token`, gated on the parent's owner auth with a single-use nonce and a per-parent lifetime cap) — they never consume a paid onboarding token, and a derived-slot coin is an ordinary transferable coin |
| 37 | `sdk37_ssp_value_gate` | the SSP's pre-payment value gate reads the **true** value, never an attacker-supplied hint: a child bundle has no on-chain-rooted branch, so `peek_pending_transfers` proves it with `verify_conveyed_child` and reports `child_state.out_value` — the value the ladder cryptographically commits to. Fails closed on any tamper |
| 38 | `sdk38_sponsor_stiff` | bounded loss on sponsored refresh: a sponsor that stiffs the user after the on-chain re-anchor costs only the fee — the user keeps the refreshed `amount − fee` coin and gets an explicit error |

### Token economics and tree closure

| n | Flow | Proves |
|---|---|---|
| 93 | `sdk93_token_payment_keeps_sats` | a token payment must not cost the sender a carrier: `transfer_tokens_onto` assigns the allocation to an outpoint the RECEIVER already owns (a revealed foreign seal) while the single bitcoin output pays the SENDER — the allocation moves, the satoshis do not |
| 94 | `sdk94_collapse_grant` | the collapse's accept path, run end to end: alice deposits, pays bob non-exactly (a real tree, root terminalized), then closes the tree through `collapse_obligations` / `collapse_grant` and the tree lands on chain |

### Concurrent chaos / property test (`SDK_E2E=22`)

A soak test for the bugs that only appear under real parallel usage. `CHAOS_USERS` wallets (one
sqlite db each) run weighted-random actions CONCURRENTLY against the live SE + lockbox — enter
(deposit), send, claim, respend (deepen the DAG by another hop), split, unilateral exit (including at
a DAG point), cooperative withdraw — plus a low-probability **cheat**. There are two cheats, both
"broadcast an old state" claw-backs that must be refused. There is no flat backup to capture any
more, so both capture the coin's **current ladder state** (`tesr::load(..).current().state`) before
the honest move and broadcast it afterwards:

1. **steal-after-send** — capture the state, legitimately send the coin away, then broadcast the
   now-stale state to claw it back.
2. **steal-after-split** — capture the state, then split the coin (its value moves into fresh
   sub-coins), then broadcast the stale pre-split state.

A background miner confirms deposits and matures exits; a semaphore caps concurrent SE co-signing;
all bitcoin-core shell-outs serialise through one mutex. Every attempt and result is traced to
`{run_dir}/chaos.jsonl`.

After a quiescent settle a spec-invariant oracle (`chaos22_oracle`) audits the trace + final live
state:

- **No value created** (INV-1/13/25): Σ SE-side balances + Σ exited-on-chain ≤ Σ deposited (tight:
  the residual is realised fees).
- **No cheat succeeded** (INV-18/19): every stale-state broadcast was refused, and on-chain the
  funding outpoint was never spent by the cheater's stale tx (`spender_of` backstop).
- **Single custody per `statechain_id`** (INV-18/19) and non-negative balances (INV-9).
- **All outcomes expected**: `classify()` separates spec-sanctioned contention from unclassified
  errors, and any unclassified error is a breach. Recognised classes include insufficient balance,
  no-coin / no-exact-coin, terminal (single-use / spend budget / already spent), epoch deadline,
  nonce guard, batch lock, mempool conflict, non-final, input-spent, confirm-lag, dust and split-fit;
  the concurrency refusals `raced-spend` and `raced-handover` (a concurrent handover rotated the auth
  key, which IS the permanent lockout first-class children rely on); and the infra load-shedding
  classes `pool`, `db-lock`, `conn`, `timeout`, `se-5xx`, `se-parse`. Two entries are protocol limits
  rather than contention, and both are worth reading as status:
  - **`csv-floor`** — replace-by-lower-timelock has finite depth (regtest `d0` 24 → 18 → 12 → 6 =
    `d_floor`); at the floor a coin must be exited, renewed (`renew_child`, sdk84 — by hand, since
    nothing on the transfer path renews yet) or re-anchored rather than re-sent. Plus a child too
    small to split into a viable piece + change.
  - **`tip-not-conveyable`** — a spine TIP cannot be handed over whole, because there is no
    `spinetip-` conveyance builder. This is classified only because the refusal comes by name from
    `execute_ex`; a tip dying anywhere else must stay a BREACH. The tip is not stranded by it — pay
    FROM it with a spine batch, or exit it unilaterally.

  The "coin has a ladder and cannot be split as plain BTC" refusal is deliberately left UNCLASSIFIED
  so a real routing regression still shows up as a breach. Its producer, `split_coin`, is now
  deleted, so the string can no longer be emitted at all — leaving it unclassified still costs
  nothing and still means "if this ever appears, it is a breach". The harness's `split` action was
  never a plain split anyway: it calls `transfer`, which dispatches on the coin's shape.

```bash
# smoke (fast): 5 users, 20s
SDK_E2E=22 CHAOS_USERS=5 CHAOS_SECS=20 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo +stable run
# full: 100 users, 120s, 8 whales
SDK_E2E=22 CHAOS_USERS=100 CHAOS_SECS=120 CHAOS_WHALES=8 CHAOS_INFLIGHT=24 ... cargo +stable run
```

Knobs and defaults, read in `chaos22_concurrent_users`: `CHAOS_USERS` 5, `CHAOS_SECS` 20,
`CHAOS_WHALES` 2, `CHAOS_DEPOSIT_SATS` 2 000 000, `CHAOS_INFLIGHT` 12 (caps concurrent signing to
respect the SE pool), `CHAOS_CHEAT_PROB` 0.06, `CHAOS_SEED` 42, `CHAOS_RUN_DIR`.

It never runs on the default `cargo run` path — `SDK_E2E=22` must be set explicitly — so ordinary
runs stay fast; the full-matrix runner picks it up at the small default size. RGB-over-chaos is not
implemented; the harness runs pure sats today.

## RGB primitives (`RGB_E2E`)

The low-level suite under the SDK. It used to drive the off-chain branch DAG directly; that lane is
retired, and eight of its sixteen ids went with it.

**The live set is `4, 7, 11, 12, 13, 14, 15, 16`** — that, and only that, is what `main.rs`
dispatches. `RGB_E2E=1, 2, 3, 5, 6, 8, 9, 10` are **DELETED**: source files gone, dispatch arms gone,
so those ids now hit the unknown-id refusal. They are listed below only so a reader who remembers
them can see where they went.

| n | Covers | Status |
|---|---|---|
| 1 | off-chain split | **DELETED** — the lane it drove refuses (`register_split_subcoins_n`); a partial payment is the in-ladder split |
| 2 | 2-input combine | **DELETED** — the combine refuses (`register_combine_subcoins`) |
| 3 | 2-deep un-broadcast chain | **DELETED** |
| 4 | SE single-use refusal — a second conflicting spend of a node | **re-derived, pending run**: a `single_use` deposit is the one deposit shape that gets no ladder at first sight (the SE refuses a second co-sign on it) and, with `create_tx1` gone, no flat backup either; the probe pins that shape (CONFIRMED, no `tesr-` row, no backup row) and then the guard |
| 5 | 3-input combine | **DELETED** |
| 6 | 3-level DAG (split → combine → split) | **DELETED** |
| 7 | epoch deadline: the SE co-signs inside the active period, refuses past it | **re-derived, pending run**: the coordinator's `epoch_deadline` gate on `sign/first`, pinned over `single_use` deposits, independent of how a coin exits |
| 8 | wide combine | **DELETED** |
| 9 | blinded and witness send/receive | **DELETED** — both legs co-signed un-broadcast coloured transactions over `F` |
| 10 | history and self-transfer semantics over an un-broadcast split | **DELETED** |
| 11 | UDA / CFA issuance schemas | unchanged |
| 12 | `validate_offchain_chain` negative | unchanged |
| 13 | consignment integrity | unchanged |
| 14 | metadata + IFA supply | unchanged |
| 15 | the coloured tier builder and per-tier seal blinding: coloured 1-payload and N-payload tiers at exactly 2.000 sat/vB, off-chain validation against an un-broadcast txid, and the ≥3-rival / non-minimum-internal-txid test. Needs bitcoind + electrs + the RGB proxy only — no coordinator, no lockbox | unchanged |
| 16 | why a legacy-lane carrier cannot be coloured: reproduces the `Invalid coloring info` failure the deleted `sdk78` used to raise, over an above-the-floor piece, and names the cause (the legacy receive path never accepts the transfer into the RGB stock); the control accepts the same consignment through `accept_ladder` and the same piece colours. Same reduced stack as 15 | unchanged — and now the ONLY surviving evidence about the un-colourable class |

## Upstream Mercury suite (default `cargo +stable run`)

With no `SDK_E2E` / `RGB_E2E` / `LN_SMOKE` set, the binary runs the vanilla protocol tests in order:
`tb01_simple_transfer`, `tb02_transfer_address_reuse`, `tb03_simple_atomic_transfer`,
`tb04_simple_lightning_latch`, `tb05_timelock`, `tm01_sender_double_spends`,
`ta01_sign_second_not_called`, `ta02_duplicate_deposits`, `ta03_multiple_deposits`, `tv01`. These
call `mercuryrustlib` directly and never run the SDK's `claim()` — which under the rule means they
run `coin_status::update_coins` under `LadderAtSight::Plain`, the lane in which the deposit watcher
ladders the coin itself at first sight. So they exercise the **same** shape as everything else: three
co-signs at `IN_MEMPOOL`, no flat backup, `locktime == None`. **Four** of them are re-derived (all
pending run): `tb01` measures that shape at every status; `tb05` — which used to broadcast a previous owner's
flat backup, watch the node refuse it as `non-final` and then accept it once the calendar ran out —
now pins the pending-transfer lock, cancellation, and the stale-state defence by RELATIVE timelock
(every replaced state is disclosed at a strictly higher CSV than the live one; `broadcast_backup_tx`
refuses the coin by name); `ta03` — which used to force-send a coin with its duplicate deposits and
validate the INV-5 chain — now pins that four deposits to one address book ONE laddered coin plus
three `DUPLICATED` coins with no calendar, that `transfer_sender::execute` refuses a coin with
duplicates BY NAME whatever `force_send` says, and that duplicates are recovered by cooperative
withdrawal. **`ta02` is the fourth**: a duplicate is refused at conveyance by name with `force_send`
both set and unset (the flag is INERT — `execute_ex` discards it with `let _ = force_send`), nothing
is conveyed, nothing is INVALIDATED, and both flows end by recovering the duplicate through a
cooperative withdrawal. `ta01` and `tv01` are touched to assert the shape. **No flow in this suite
has been run under the rule** — the sixteen-flow run in `dd03ab2` was `SDK_E2E` only. Run this after
any change to transfer/receiver code.

## Lightning harness smoke (`LN_SMOKE=1`)

Two `rgb-lightning-node` daemons, a funded channel, a real BOLT11 paid end to end (the flow asserts
`invoice_status == "Succeeded"`). Use it to prove the RLN half of the stack is healthy before blaming
a Lightning flow. Honours `RLN_BIN`.

## Repo invariants (`cargo test -p ci-guards`)

`ci-guards` is a dependency-free crate whose "tests" are source scans over the repo: they read files
relative to the crate manifest, so they need no stack, no network and no toolchain pin, and they are
always cheap to run. Each guard pins a property that per-site fixing failed to hold. A source scan
establishes presence, absence and ordering — it does **not** establish reachability, binding or
behaviour, which is what the E2E flows are for. Four of them were re-derived with the rule
(`deny_armed_tower_during_conveyance`, `deny_colored_backup_on_a_colored_ladder`,
`deny_selection_without_exit_material`, `deny_unqualified_keyless_rescue`).

| Guard | Property |
|---|---|
| `deny_armed_tower_during_conveyance` | every lane that hands value away must move the coin out of the live set — L1 is `is_live_for_defence`: `IN_MEMPOOL`, `UNCONFIRMED` or `CONFIRMED`, three statuses because a ladder exists and is defended from first sight — **before** the superseding co-sign, so this wallet's own tower can never be armed against its own recipient |
| `deny_chain_anchored_token_balance` | a token balance may not depend on the SHAPE an allocation arrived in |
| `deny_colored_backup_on_a_colored_ladder` | a laddered coin conveys NO flat backup, and both acceptance paths must refuse one — plain or coloured, on either lane (`verify_flat_backup_lane` over an empty vector is the only thing that passes) |
| `deny_flat_ladder_config_drift` | every deployment config must agree with `TesrParams::flat_ladder_params`. `initlock`/`interval` survive as compatibility constants — `initlock` is the fixed exit window the split-depth cap measures against — so a coordinator that disagrees is still refused rather than trusted |
| `deny_line_number_citations_in_normative_docs` | a normative document may not cite code by line number. The normative set is DERIVED from `docs/utexo/spec/README.md`'s `*Normative*` labels, not hand-listed |
| `deny_optional_deadline_safety` | the deadline defence stays unconditional in `maintenance_plan`; routine re-anchoring is not. (On a laddered wallet the pass has no subject — it is kept unconditional, not kept busy) |
| `deny_relative_budget_mirror` | the coordinator and the enclave must be handed the SAME spend-budget quantity — one is relative, one absolute, and mixing them is how the two enforcers disagree |
| `deny_rgb_witness_apis` | `update_witnesses` / `upsert_witness` stay unreachable from this tree: one call with the plain blockchain resolver silently archives every rung of a deliberately un-broadcast coloured ladder |
| `deny_selection_without_exit_material` | a coin may not be offered to a payment unless this wallet holds material to EXIT it — a `tesr-` / `ctesr-` / `spinetip-` bundle, which under one coin shape is the ONLY exit material there is |
| `deny_sender_declared_ladder_gate` | the JS clients' laddered gate must key on coordinator-served evidence, not on fields the sender fills in |
| `deny_sender_declared_margin` | the supersession margin keys on the live rival's STRUCTURAL kind (`δ` for a state, `δE` for an extension), not on a one-block lead |
| `deny_silent_degradation` / `deny_swallowed_backup_reads` | the silent-degradation class: a failure that presents as a benign empty or idle result. An empty carrier set, an empty branch-witness set, a spend generation of zero, an absent deadline — each is one line turning `Err` into a default, and the default always stands down |
| `deny_stale_depth_cap` | a document may not publish a superseded split-depth cap. The caps are **8 / 19** (mainnet) and **54 / 111** (regtest): `exit_wait_blocks + exit_slack_margin` against the fixed `initlock` window |
| `deny_unattested_num_sigs_reader` | `num_sigs` enters the client only through the attested reader — unattested, a coordinator under-reporting it by `k` hides `k` co-signed rival states and the census still balances |
| `deny_unattested_terminality` | terminality comes from the enclave's signature, not from the coordinator's Postgres |
| `deny_uncoloured_legs_under_a_coloured_sp` | a coloured spine batch may not build plain legs under its coloured `SP` |
| `deny_unconsumed_slot_vouchers` | a derived-slot voucher becomes a deposit address only through `create_child_slot_addr`, so the SE-side spend and the on-disk pool move together |
| `deny_uncovered_carrier_deadline` | `deadline_safety_due`'s unilateral route must not exclude carriers while its cooperative route (rightly) does. Under the rule neither route has a laddered subject; the guard pins the shape of the pass, not a deadline |
| `deny_unpinned_wire_error_codes` | `TransferReceiverError`'s variants are the interface; every client profile must carry all of them |
| `deny_unqualified_keyless_rescue` | the keyless tower's stated INCAPABILITY stays stated — it broadcasts pre-signed tiers at their committed fee and cannot fee-bump them |
| `deny_unstated_census_shape_obligation` | the shape rules that discharge the census's distinctness premise must say that they do — under the rule the counted categories are live tiers and disclosed superseded tiers, and the flat term is 0 by emptiness |

## Unit tests

```bash
cargo +stable test -p mercurylib          # TES-R primitives, transfer cancellation, the P2A fee child
cargo +stable test -p mercury-utexo-sdk   # coin selection, config, invoices, watchtower, refresh, doctests
cargo +stable test -p mercuryrustlib      # core RPC, tower float, the empty-vector census refusals
cargo test -p ci-guards                   # repo invariants (no stack, no pinned toolchain)
```

`mercurylib`'s `tesr` tests are pure, stack-free consensus math: a split state conserves value and
scales its fee with output count (and rejects mint/burn), a child's tiers root at `SP.out[j]` for
arbitrary `j`, encoding reads the payload output and fails closed out of range, the P2A script is
`OP_1 <0x4e73>` at 240 sats, `csv_blocks` sets a relative *block* lock (disable and type bits clear),
the **trigger's sequence disables the relative lock entirely**, tier value decrements by fee +
anchor, the uncoloured fee matches a measured signed tier, the coloured surcharge is exactly one
`OP_RETURN` output, the derived floors track `TIER_VBYTES`, the spine-tip floor is strictly below the
child floor at every shipped rate, and the `TesrParams` schedule decrements and clamps at its floors
with correct renewal / rollover thresholds. `attestation_identity` resolution is pinned as pin →
config → refuse, and `only_regtest_has_a_pinned_identity_until_more_enclaves_are_provisioned` is the
tripwire on the pin set itself: regtest must equal `REGTEST_ATTESTATION_IDENTITY`, every other
network must still be `None`. It is kept rather than deleted because pinning a key changes the
security posture of every client build — and, since `SdkConfig` reads it to decide `colored_ladder`,
pinning a network also turns the coloured ladder on for it. The SDK crate's own
`colored_ladder_is_never_on_without_a_pinned_attestation_identity` asserts the other end of that
coupling: each constructor's flag must equal whether a pin exists, so the two can never disagree.

`mercuryrustlib`'s `tesr` module carries the census's emptiness refusals as unit tests —
`an_empty_vector_passes_on_both_lanes`,
`plain_backups_are_refused_on_both_lanes_and_the_refusal_names_the_lane`,
`rgb_material_on_a_flat_backup_is_refused_like_any_other_flat_backup`,
`an_unparseable_backup_is_refused_without_being_read` — and the SDK's `watchtower` module pins that
a leaf entry is exported at `deadline_block: u32::MAX` with a trigger on `F`, exactly like a
laddered parent.

The SDK crate also carries two `#[cfg(test)]` **models** — pure companions to the cost and
granularity write-ups in [learn/](../learn/), calling the real production functions wherever a
callable pure one exists:

* `invalidation_model.rs` — the exit model in `config::tesr_exit_vbytes` / `tesr_exit_txs` /
  `tesr_exit_wait_blocks` (production code, because `SdkConfig::auto_exit_margin_blocks` derives
  from it), `transfer::split_fee_reserve`, `transfer::split_amounts`, `select::plan`,
  `types::ExitCostEstimate::fee_sats_at` and `types::is_terminal`. It also still models the
  **retired** flat shape's arithmetic — `mercurylib::transaction::calculate_block_height` and
  `wallet::deposit_anchored_deadline` — which now describe only the legacy `branch-` deadline
  `auto_exit_due` computes for coins that predate the rule; nothing minted today has such a
  deadline.
* `granularity_model.rs` — exact subsets and whole-coins-then-split (`select::plan`,
  `select::exact_subset`), the split floor, `tokens::TOKEN_PIECE_SATS` (4 074 sats — the coloured
  root-ladder floor at twice the committed tier fee rate, derived rather than chosen), and ceil fee
  arithmetic.

## Live integration tests

These live in workspace `tests/` directories rather than the E2E dispatch. Each **skips loudly**
when its dependency is absent — printing that nothing was verified rather than passing quietly.

| Test | Needs | Proves |
|---|---|---|
| `lib/tests/live_sig_count_attestation.rs` | a lockbox at `LOCKBOX_URL` (default `http://127.0.0.1:18080`), `LIVE_STATECHAIN_ID` | a REAL attestation fetched from the running lockbox and checked by the shipping verifier. It fetches rather than embedding a vector, so it either exercises the live SE or says it did not |
| `lib/tests/d7_network_profiles.rs` | nothing | every network's schedule is named explicitly — `for_network_checked` refuses an unrecognised name instead of falling through to the toy regtest profile |
| `lib/tests/p2a_script_shape.rs` | nothing | `p2a_script()` is exactly `51024e73` and `P2A_VALUE` is 240 |
| `clients/libs/rust/tests/live_info_config.rs` | a coordinator at `STATECHAIN_ENTITY` (default `http://127.0.0.1:8000`) | the compiled-in `initlock` / `interval` table matches the coordinator actually deployed — compatibility constants under the rule, but a disagreeing coordinator is still refused. A unit test can only check the table against itself |
| `clients/libs/rust/tests/live_p2a_package_rescue.rs` | `CORE_RPC_URL` / `CORE_RPC_USER` / `CORE_RPC_PASS`, a node whose `minrelaytxfee` exceeds the tier's committed rate, `ELECTRUM_URL` for the seam test; `REQUIRE_LIVE_NODE=1` turns a skip into a failure | an under-paying v3 tier is refused alone and rescued **through this repo's own code path** (`mercurylib::wallet::p2a_fee_child::build_p2a_fee_child`), not a hand-run `bitcoin-cli`. Also covers anchor-squatting and the broadcast seam |
| `clients/libs/rust/tests/live_tower_float.rs` | `CORE_RPC_URL` and friends | what actually bounds a funded tower: under TRUC a v3 fee child may have one unconfirmed ancestor, and the tier is already it — so simultaneous-rescue capacity is the number of CONFIRMED fee UTXOs held, not the number of sats |

## Adversarial coverage map

| Theme | Covered by |
|---|---|
| double-claim / duplicate leaf | ta02, ta03 (duplicate deposits refused at conveyance by name and recovered by withdrawal — re-derived, pending run), tm01 (sender double-spend), sdk04 (claim idempotence) |
| conflicting off-chain spend | RGB_E2E=4 (SE single-use refusal, re-derived), sdk04 (a terminalized split parent refused twice over), sdk12 Part C (secnonce reuse) |
| wrong preimage / locked claim | tb04 (the latch itself), sdk64 (the SE reveals the preimage only once the payee's coin is claimable), sdk19 (never paid ⟹ no preimage), sdk25 (claim past the latch window refused) |
| transfer interrupt / resume | tb01+tb02 paths; `claim()` idempotent per message (sdk04); sdk56 (a replayed `/sign/second` returns the cached signature and does not advance the count); sdk81 (SIGABRT in the signed-but-unpersisted window, recovered from the journal) |
| exit-race ordering | sdk41 and sdk49 (the receiver's state carries the strictly lower CSV and matures first), sdk51 (that ordering wins a *contested* exit), sdk54 (a hidden lower-CSV state cannot be smuggled past the census), sdk84 (the renewed extension beats every superseded one on chain), tb05 (the stale-state defence on the upstream lane is the same relative ordering — re-derived, pending run). There is no absolute-locktime ladder to order any more |
| a flat backup or branch material smuggled beside a ladder | the `mercuryrustlib::tesr` unit refusals (`plain_backups_are_refused_on_both_lanes_…`, `rgb_material_on_a_flat_backup_is_refused_…`), sdk55 (a conveyed backup is refused before the census; a disclosed rival cannot be inverted), sdk74 (a flat term of 1 is refused) — all re-derived, pending run |
| griefing / forced exit | sdk45 (a keyless tower defends an offline owner after a hostile trigger), sdk40 PART 2 and sdk89 (cooperative de-trigger, plain lane), sdk72 part B (the defence runs from `start_background` alone) |
| the conveyance window | sdk90 (the payer's local gates), sdk91 (the coordinator's one-hour gate, probed directly with a genuine credential) |
| bundle binding / decoy ladders | sdk70 (`verify_bundle_bound` against a genuine decoy ladder over an attacker-owned outpoint; wrong `payload_vout` fails closed; a disclosed tier cannot be double-counted), sdk57 (the authoritative sid → aggregate binding), sdk92 (REQ-57 witness binding, live) |
| invalid consignment | the receiver hook rejects: `mercury_rgb`'s `validate_offchain_chain` (RGB_E2E=12 the negative, RGB_E2E=13 consignment integrity), and on the coloured lane `UtexoWallet::colored_child_health` / `colored_ladder_health`, which book what the CONSIGNMENT assigns and never the sender's declared field (sdk02, sdk29, sdk32) |
| value inflation at the operator boundary | sdk37 (the SSP gate reads the ladder-committed value, not a sender hint), sdk20 (wrong-recipient + undersized refused), sdk58 (11 `verify_child_bundle` census attacks), sdk76 (a received parent's census flat term is 0 for every `k` — re-derived, pending run) |
| a payee handed an unexitable coin | the split-depth cap against the fixed `initlock` window (sdk17 — a deep split that must SUCCEED; sdk82 / sdk88 — a child has no epoch to run out of, plain and coloured; the ci-guard `deny_stale_depth_cap`) — re-derived, pending run. The carrier half of this theme (a carrier that can never be coloured is neither stranded silently nor rescued by weakening a floor) was `sdk78`, now DELETED: **UNPROVEN** |
| a wallet racing its own recipient | sdk79 (the coloured sender's tower), sdk80 (the plain child-split lane's conveyance ordering) — both re-derived to L1 = `is_live_for_defence`, pending run |

## What a payment costs

Numbers quoted in reviews come from [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md),
priced on the **leaf** lane — what an ordinary holder has, not what a depositor has. Per payment,
against ~154 vB for an ordinary on-chain payment: **0 vB** spent onward while it stays off-chain;
**418 vB** shipped default; **250 – 2 719 vB** walked out unilaterally. Do not quote the whole-coin
lane as the user-facing figure, and do not lead with the shipped-default number as a win — for the
population that actually exists it settles a payment for MORE block space than doing it on chain.

Two figures are **DESIGN, NOT BUILT**, and a review that quotes either must say so:

* the **sweep** (SPEC.md §5.3, economics §3) — the **~105 vB** swept-and-settled row, 1.47× better
  and the cap without the round. `combine_leaves` exists as a primitive and sdk83 drives it, but it
  has **no caller outside a test**: there is no absorption predicate, no `claim()`-time swap and no
  settlement scheduler in the tree.
* the **discharge round** (SPEC.md §5.4) — its SE enforcement point is empty, so no flow exercises
  it at all.

## Running the whole matrix

`clients/tests/run_all_suites.sh` runs the `mercury-utexo-sdk` unit tests, then **every** `SDK_E2E`
and `RGB_E2E` index it discovers by grepping the dispatch in `main.rs` (so it tracks the live set
automatically), then the LN smoke, then the upstream suite. It exports `ML_NETWORK`, `COMPOSE_FILE`,
`COMPOSE_PROJECT_NAME`, `RLN_REGTEST` and `RLN_BITCOIND_CONTAINER` with defaults, nudges electrs if
its port is closed, and captures per-test stdout/stderr plus time-sliced docker logs
(`mercurylayer-mercury-server-1`, `mercurylayer-lockbox-1`, `rgb-lightning-node-electrs-1`) into
`$LOGDIR` (default `/tmp/utexo_suite_logs`) with a `summary.txt` of PASS/FAIL and durations. `REPO`
is set at the top of the script. `UTEXO_ATTESTATION_IDENTITY` no longer has to be exported for
regtest — the pin is compiled in — and a stale export that disagrees with it now fails every
laddering claim rather than being ignored, so clear it rather than leaving it set. A full run has
**not** been made since the rule landed. What HAS been run, per `dd03ab2`, is a sixteen-flow subset —
`SDK_E2E` 15, 37, 40, 42, 44, 45, 48, 49, 50, 58, 59, 60, 71, 74, 76 and 85 — plus the three
stack-free suites; everything else above is pending. That commit also taught the runner three
pass-line spellings it did not recognise (`SDK71 - PASS:`, `SDK48 - ✓ PASS:`,
`sdk85_transfer_cancel: OK`), which it had been reporting as failures on a zero exit code; if you are
on an older `run_all_suites.sh`, expect those three false FAILs. A run list copied from an older
revision will also fail on the DELETED ids (`RGB_E2E=1, 2, 3, 5, 6, 8, 9, 10`, `SDK_E2E=73`,
`SDK_E2E=78`) — the runner greps the live dispatch, so it will not schedule them, but a hand-written
`ONLY=` will.

```bash
./run_all_suites.sh                                # everything
ONLY="SDK_E2E=59 SDK_E2E=60" ./run_all_suites.sh   # a subset (UNIT and UPSTREAM are also labels)
TRACE=1 ./run_all_suites.sh                        # RUST_LOG debug for the client
SKIP_LN=1 ./run_all_suites.sh                      # skip the RLN-backed smoke
```

Prerequisites are the two stacks above plus the RLN binary built (`cd rgb-lightning-node && git
submodule update --init && cargo build`).
