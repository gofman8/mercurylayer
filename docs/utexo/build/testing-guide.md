# Testing guide

All suites live in `clients/tests/rust` and run against the local regtest stack. The dispatch is a
flat numeric switch on an environment variable — one flow per process, no test harness — so every
suite below is a single `cargo run` with `SDK_E2E=n` or `RGB_E2E=n` set.

## Stack

```bash
cd rgb-lightning-node && ./regtest.sh start                       # bitcoind + electrs + RGB proxy
cd mercurylayer && docker compose -f docker-compose-lockbox.yml up -d   # SE + lockbox + token server
```

Environment (from `clients/tests/rust`):

```bash
export ML_NETWORK=regtest
export RLN_BITCOIND_CONTAINER=rgb-lightning-node-bitcoind-1   # the test faucet
# Lightning flows only (sdk19/20/21/23/24/25/63-68) — they shell out to the RLN stack:
export RLN_REGTEST=/path/to/rgb-lightning-node/regtest.sh
export COMPOSE_FILE=/path/to/rgb-lightning-node/compose.yaml
export COMPOSE_PROJECT_NAME=rgb-lightning-node
```

> **Toolchain.** The tests crate pins 1.83 in `rust-toolchain`; run with `cargo +stable run`
> (rgb-lib is edition-2024). `rgb-lib` is a **path** dependency on the local fork checkout
> (`clients/libs/rust-rgb/Cargo.toml` → `../../../../utexo-rgb-lib`); it must be on `feat/spark`, or
> the test binary fails to build with missing `load_transfer` /
> `validate_consignment_offchain_chain`.
>
> **Working directory.** Each flow wipes `wallet.db*` and its RGB data dirs in the *current* working
> directory at start. Two runs sharing a CWD clobber each other — run parallel suites from separate
> directories.
>
> Core 28+ is required: every ladder tier is a v3/TRUC transaction with a P2A anchor.

## What the suites exercise

There is **one protocol**, and it produces two coin **shapes**. Knowing which shape a flow drives
tells you what its assertions are actually about. Full treatment in [PROTOCOL.md](../PROTOCOL.md).

### Laddered (TES-R) — every plain BTC deposit

`claim()` establishes an exit ladder over every fresh confirmed ROOT coin, unconditionally: funding
`F` → **trigger** `T` (no timelock, signed once at deposit) → **extension** `X_m` (relative CSV
`E_m`) → **state** `S_k` (relative CSV `Δ_k`). All three tiers are v3/TRUC with a P2A anchor, and all
three stay **un-broadcast**. There is no `deposit_protocol_version` and no `UTEXO_PROTOCOL_DEFAULT`
env — those are deleted, and no test pins any other lane.

BIP-68 relative timelocks only start counting once the *parent* confirms, and `T` carries no
timelock at all, so **nothing matures until someone broadcasts `T`**. An idle coin never ages: no
calendar deadline, 0 vB of rent. What that means when you write or read a test:

* **No flow can "wait out" a deadline.** Time is driven by broadcasting a trigger and then mining
  past each relative CSV (sdk40, sdk50). Mining at an idle coin changes nothing at all — sdk30 part
  (a) mines 300 blocks and asserts the exit chain is byte-identical afterwards, `F` still unspent.
* **A transfer co-signs a fresh state one δ LOWER** than the one it replaces
  (replace-by-lower-timelock), so the new owner's state always matures first; the replaced state is
  disclosed as superseded and counted by the receiver's census (sdk41, sdk49, sdk54).
* **Renewal (a lower-CSV extension) and rollover (a fresh level) are off-chain and unbounded** — a
  coin can live off-chain forever (sdk43, sdk44). `refresh` is the **re-anchor** primitive: one
  on-chain tx that moves the coin to a fresh funding outpoint and mints a new ladder, killing every
  exit right rooted at the old `F`. It is not a deadline reset (sdk30).
* **A unilateral exit walks the pre-signed chain tier by tier**, waiting out each relative timelock
  (sdk50) — not a single backup broadcast. A keyless watch bundle lets a delegated tower do the same
  for an offline owner (sdk45, sdk51).

### Un-laddered — RGB carriers and un-broadcast split sub-coins

Not every coin is laddered, by design. An RGB **carrier** is deliberately never laddered — a plain
tier spend would destroy the allocation (terminal-freeze) — and a split sub-coin whose funding is
un-broadcast cannot root a trigger. These keep the **signed-once backup** and transfer by
backup-chain handover, with **decrementing absolute nLocktimes**: each hop's backup is built one
interval below the previous, so the current owner's backup is always the first to become final, and
a superseded backup is non-final (its locktime sits above the tip) whenever a fresh one is spendable.

This shape is current and load-bearing, not deprecated. Every RGB flow rides it (sdk52, sdk02,
sdk29, sdk31, sdk34, sdk39), and so does the upstream Mercury suite, which sits below the SDK and
never calls `claim()`. Its receiver-side guarantee is `transfer_receiver::verify_terminal_parents` —
one terminal ancestor per structural input.

### Non-exact payments — the in-ladder split

A payment that is not an exact subset of the sender's laddered coins runs the **in-ladder split**: a
state tier `SP` spending `X_m.out[0]` — a *descendant* of the trigger, never a rival for `F` — paying
a piece child and a change child, each with its own two-tier ladder. Admission floor is
`min_child_value` = **1306 sat at 2 sat/vB** (a child funds its own extension + state and clears
dust); the parent is terminalized and its old owner state disclosed as superseded (sdk58, sdk59).

Received children are **first-class**: the claim completes the standard SE key handover, so the
receiver co-owns `A_child` (invariant across the rotation, which is what keeps the pre-signed child
tiers valid) and the sender is permanently locked out. A child pays onward off-chain whole
(`child_retransfer`) or split (`child_in_ladder_pay`, a depth-2 `ancestors` chain) — one
co-signature and exactly one disclosed superseded state per hop (sdk60, sdk17). See
[CHILDREN.md](../CHILDREN.md).

## Suites

### SDK E2E (`SDK_E2E=n cargo +stable run`)

The dispatch lives in `clients/tests/rust/src/main.rs` and runs `SDK_E2E=1..68` with gaps where
tests were retired — see [Retired numbers](#retired-numbers). The tables below group by theme; the
numbers themselves are historical, not ordered by topic.

#### Wallet, parity, guard rails

| n | Flow | Proves |
|---|---|---|
| 1 | `sdk01_wallet_flow` | deposit → exact-subset transfer → auto-claim → non-exact transfer (routes to the in-ladder split, sender keeps the change) → auto-claim → cooperative withdraw to L1. No sats/UTXO management surfaced to the app |
| 4 | `sdk04_adversarial` | SDK guard rails: typed `InsufficientBalance`; a split parent is terminalized by `SP` at the SE and booked WITHDRAWN, so a second full-value spend is refused twice over; claim idempotence; double-withdraw refusal |
| 11 | `sdk11_parity_methods` | parity API: identity message signing, multi-recipient sats transfer, Utexo invoices (create + fulfill), query/history |
| 12 | `sdk12_adversarial` | security-review regressions on the laddered default: in-ladder value flow lands the exact amount; MuSig2 secnonce reuse refused on the second `/sign/second`; unauthenticated `/transfer/unlock` refused (403, real and unknown ids). Parts C and D are protocol-agnostic SE/lockbox guards tested **nowhere else** — they must survive every migration |
| 15 | `sdk15_fresh_doublesign` | the honest trust floor: a *malicious SE* co-signing a RIVAL trigger over the same `F` is exactly as final as the owner's — `T` is locktime-free, so the CSV tiers break no tie and the contest degrades to a plain on-chain race. The ladder shape changed; this floor did not |
| 16 | `sdk16_onboarding` | enter with nothing: a wallet with no deposit and zero balance receives BTC *and* an RGB asset, then exits unilaterally |
| 17 | `sdk17_oor_chain` | out-of-round transferability: alice → bob → carol where hop 2 is a **partial** re-spend of bob's received child (a child-level in-ladder split; the child becomes an intermediate `ancestors` segment and carol's exit walks a depth-2 chain). `F` stays unspent throughout |

#### TES-R ladder — consensus, lifecycle, exit, defence

| n | Flow | Proves |
|---|---|---|
| 40 | `sdk40_tesr_consensus` | the consensus core against real bitcoind: un-broadcast immunity; `X` REJECTED before `E` confirmations of `T` and `S` REJECTED before `Δ` confirmations of `X`; a full unilateral exit with no operator cooperation. **PART 2**: cooperative de-trigger defeats a hostile trigger. The blind SE is unchanged — it blind-signs v3 + relative-timelock + P2A sighashes |
| 41 | `sdk41_tesr_transfer` | a transfer really moves control: `A` and `F` are invariant (no on-chain tx), only the shares rotate; Bob co-signs and exits a full ladder over the same `F`, and Alice's later co-sign attempt is refused — she is cryptographically out |
| 42 | `sdk42_tesr_lifecycle` | wallet-level lifecycle: establish → renew off-chain → persist to the wallet DB → reload as a fresh session would → unilateral exit **from the reloaded bundle** |
| 43 | `sdk43_tesr_rollover` | "off-chain forever": when the extension-CSV budget is exhausted the ladder rolls over **off-chain** to a fresh level (zero on-chain bytes), renewal keeps working at the new level, and the whole deep chain still exits |
| 44 | `sdk44_tesr_params` | the canonical `TesrParams` schedule drives `establish_auto` / `renew_auto` / `rollover_auto` — the cadence a real wallet runs — and the resulting ladder still exits (decrement + floor + `m_max` math) |
| 45 | `sdk45_tesr_watchtower` | a **keyless** WatchBundle (pre-signed tiers only, no key material — every tier pays the owner) lets a delegated tower drive an offline owner's exit after a griefer broadcasts the trigger; a second independent tower pass is idempotent |
| 46 | `sdk46_tesr_rprime` | the census formula `se_num_sigs == v1_backups + tiers` checked against the **real** SE sig count (not a mock): `verify_bundle` accepts the true count and rejects a hidden extra signature |
| 47 | `sdk47_tesr_rprime_transfer` | a pre-established ladder carried across a transfer message and accepted by the receiver's R′ verifier |
| 48 | `sdk48_v2_native_deposit` | `claim()` auto-establishes **and persists** the ladder for each fresh confirmed deposit, exiting to the wallet's seed-derived backup address (recoverable, not an out-of-wallet key); `num_sigs == 4` (deposit backup + `T` + `X` + `S`); a second `claim()` does not double-establish |
| 49 | `sdk49_model_a_transfer` | Model A: the sender pre-signs the receiver-paying state `S'` one δ lower; the receiver verifies it exits to **its own** key, adopts the ladder, and unilaterally exits it — the end-to-end proof that the receiver gets a complete self-custodial exit chain |
| 50 | `sdk50_v2_unilateral_exit` | the public `wallet.unilateral_exit()` walks the chain (trigger → extension → state) as each relative CSV matures, reporting `wait_blocks` between tiers, until the funds land at the wallet's own backup address. No absolute-locktime backup is broadcast |
| 51 | `sdk51_v2_watchtower` | the contested case: *someone else* spends `F`, starting the CSV clock; the owner only runs `wallet.defend_ladders()` and wins because the adopted current state carries the strictly-lowest CSV. Also asserts the pass is a **no-op while the coin is idle** — an un-broadcast coin never ages, so there is nothing to defend |
| 52 | `sdk52_v2_rgb_carrier` | terminal-freeze: in one wallet the plain deposit carries a ladder and the RGB carrier carries **none**, and an off-chain token transfer still settles 750/250 — the two shapes coexist |
| 53 | `sdk53_v2_latch_guard` | the old refusal of a Lightning-latched transfer of a laddered coin is **lifted** — the latch opens. (The happy path is sdk63; the guard's replacement is the SSP's pre-pay census.) |
| 54 | `sdk54_verify_bundle_adversarial` | the anti-theft count cannot be padded: `expected = v1_backups + tiers + superseded_states + superseded_extensions`, with superseded entries parsed, ladder-linked and signature-checked, and `csv: None` no longer skipping the race check. Each attack that inflated `expected` to hide a low-CSV self-paying state is now REJECTED while the honest bundle verifies |
| 55 | `sdk55_backup_chain_adversarial` | the conveyed backup chain cannot be **padded** (duplicate `tx1` inflating `expected`) or **inverted** (sender keeps the lower locktime so their stale backup matures first). `validate_backup_chain_v2`'s INV-5 `ladder_decrements_by_interval` rejects both, using the coin's real validly-signed backups |
| 56 | `sdk56_keystone_retry_idempotent` | the signing round is idempotent under retry: re-sending the exact same `/sign/second` returns the **identical** partial signature from cache and does **not** advance `sig_count` — so a lost response cannot leave the count ahead of the disclosed tier set and brick the coin's census |
| 57 | `sdk57_owner_share_binding` | the server records an **authoritative** aggregate per `statechain_id` (owner share + enclave share) and `/info/statechain` returns it equal to the coin's own aggregate x-only — the anchor that stops a rogue-key decoy defeating the child census |

#### In-ladder split and first-class children

| n | Flow | Proves |
|---|---|---|
| 58 | `sdk58_inladder_split` | `verify_child_bundle` ACCEPTS a real split child — `SP` is a state tier spending `X_m.out[0]` (a descendant of the trigger, not a rival for `F`), the parent is terminalized and `S_0` disclosed as superseded, and the child's two-aggregate bundle (ancestors under `A_parent`, child tiers under `A_child`) checks out against chain + `/info/statechain`. Eleven adversarial census cases are REJECTED, including a non-terminal parent and a hidden lower-CSV state |
| 59 | `sdk59_inladder_pay` | the split is a usable **payment** through `transfer()` / `claim()` / `unilateral_exit()`: `transfer()` auto-routes to the in-ladder split, the piece child pays Bob (Model A) and is conveyed to his mailbox, the change child pays Alice back; Bob adopts via `verify_child_bundle` and exits the child to his own key |
| 60 | `sdk60_child_firstclass` | a **received** child is re-transferred WHOLE off-chain (alice → bob → carol): the claim completes the key handover so `A_child` is invariant and Alice is locked out; `child_retransfer` co-signs a fresh state at a strictly lower CSV and discloses the replaced one; Carol's census counts the child-superseded segment (`child_num_sigs == 0 + 2 + 1`) and she exits. Two payments, zero on-chain footprint — `F` unspent until the final exit |

#### Lightning

Both directions run on the ladder through a **HODL-invoice latch** — see
[LIGHTNING.md](../LIGHTNING.md). The LN-latched piece is the one case that stays terminalized (it
sits unclaimed past the pending-transfer lock's window).

| n | Flow | Proves |
|---|---|---|
| 19 | `sdk19_receive_failure` | RECEIVE that is never paid: no LN payment ⟹ the SE does not reveal the preimage, the receiver cannot claim, the SSP keeps its (reclaimable) coin |
| 20 | `sdk20_adversarial_gate` | the SSP pre-payment gate over the live SE + live RLN: a coin latched to a **third party** and an **undersized** coin are both refused; no LN payment goes out and the merchant invoice does not settle |
| 21 | `sdk21_remote_sspclient` | the same `pay_lightning_invoice` / `create_lightning_invoice` calls against a **deployed** `mercury-ssp` HTTP server — exercises serialization, the `{error:..}` contract, background settle spawn and DB isolation |
| 23 | `sdk23_rgb_ln_swap` | RGB assets over Lightning: issue → colored channel → asset invoice → decode → pay; asset balances shift by the exact amount |
| 24 | `sdk24_receive_cancel` | the HODL **cancel** leg: the payer pays (HTLC parks HELD, status `Claimable`), the SSP aborts *before* confirming the latch → `/cancelhodlinvoice` fails the HTLC back and the payer is refunded immediately |
| 25 | `sdk25_receive_delayed_claim` | the receiver who stalls past the SE latch window gets **nothing**: the claim gate and the SSP's `get_preimage` are bound to the same expiry, set shorter than the HODL HTLC, so the coin stays with the SSP and the payer is refunded |
| 63 | `sdk63_v2_lightning_pay` | **exact PAY** from a laddered coin: the SSP's pre-pay census (`verify_bundle` over the conveyed ladder, `num_sigs` read from the enclave sig-count) runs before `send_payment`. Alice deposits the exact invoice amount, so no split is involved |
| 64 | `sdk64_v2_lightning_receive` | **exact RECEIVE** into a laddered coin: the SSP fronts its own coin under a HODL invoice and the SE reveals the preimage only once the payee's coin is claimable — the SSP owns the coin throughout its risk window, so the receive direction needs no operator trust |
| 65 | `sdk65_inladder_lightning_pay` | **non-exact PAY** via a latched in-ladder split: the piece pays the SSP and is latched to the invoice hash, the change stays Alice's, and the SSP runs `verify_conveyed_child` on the CHILD bundle before `send_payment` |
| 66 | `sdk66_inladder_pay_failure` | non-exact PAY failure → clean **rollback**: an unroutable invoice after the split + conveyance restores the parent as exitable and drops the piece plus the optimistic change, so the user recovers the whole parent |
| 67 | `sdk67_inladder_lightning_receive` | **non-exact RECEIVE**: the SSP holds only a large laddered coin, so `create_receive` falls back to an in-ladder split and conveys a piece worth the invoiced amount under an SE-minted preimage; `settle_receive` releases the piece and claims the HTLC |
| 68 | `sdk68_v2_pay_failure_reclaim` | exact whole-coin PAY failure → clean **reclaim**: the orphan `S'` co-sign inflates `sig_count`, so `reclaim_lightning_payment` restores the coin locally as exitable instead of self-transferring; the value is fully recoverable and re-transfer is unblocked by a `refresh()` |

#### RGB tokens

| n | Flow | Proves |
|---|---|---|
| 2 | `sdk02_token_flow` | issue 1000 TKN (RGB NIA) onto a statechain coin → pay bob 250 off-chain: colored split + branch-carrying key handover with the consignment riding the transfer message; bob validates it off-chain (un-broadcast witness chain) and books under the **verified** contract id (750/250); bob then exits and his token coin materializes |
| 9 | `sdk09_ifa_batch` | IFA (inflatable) issuance, on-chain mint bound to a new statechain coin, and a batch multi-recipient transfer in **one** colored split, each receiver validating its own consignment amount |
| 29 | `sdk29_granularity_tokens` | raw-unit precision (down to 1 raw unit; `precision` is contract metadata the SDK never scales), depth-2 token exit, spent-carrier change → plain BTC, the one-carrier-per-transfer limit, and idempotent double-receive of the same asset |
| 31 | `sdk31_token_combine` | multi-carrier combine: an amount spanning several carriers is paid by one SE-co-signed colored combine (N inputs → piece + change); the receiver requires **one terminal ancestor per structural input** |
| 32 | `sdk32_token_over_time` | tokens are never lost by inactivity. A carrier is terminal-frozen (`tesr::load` → `None`) and carries only the signed-once backup; issued tokens stay sendable forever via colored split (a path that never touches a ladder), received tokens stay materializable, and a plain unilateral exit of a carrier is refused because it would destroy the allocation |
| 34 | `sdk34_token_watchtower` | `auto_exit_due` materializes a **received** carrier nearing its clawback deadline — branch only, no sats-sweeping backup — so a malicious sender cannot claw the tokens back; an issued flat carrier (no ancestor, no clawback risk) is skipped at any margin |
| 39 | `sdk39_depth2_token_exit` | a token piece **two** colored splits deep exits on-chain end-to-end (branch broadcast root-first, on-chain root spent) with the allocation preserved and no SE involvement |
| 52 | `sdk52_v2_rgb_carrier` | (also above) a carrier is never laddered — RGB and the ladder coexist in one wallet |

#### Operations: re-anchor, fees, sponsorship

| n | Flow | Proves |
|---|---|---|
| 30 | `sdk30_refresh` | **(a) idle coins never age** — mine 300 blocks and the exit chain is byte-identical (same txids, same relative CSVs), `F` still unspent, balance untouched, 0 vB of rent. **(b) re-anchor** — `refresh` cooperatively spends `F` into a brand-new aggregate, minting a new statechain id with its own fresh ladder and permanently killing every exit right rooted at the old `F`. User-pays mode (fee deducted from the coin) |
| 36 | `sdk36_derived_tokens` | split / combine / refresh slots are **free derived slots** (`POST /deposit/get_derived_token`, gated on the parent's owner auth with a single-use nonce and a per-parent lifetime cap) — they never consume a paid onboarding token, and a derived-slot coin is an ordinary transferable coin |
| 37 | `sdk37_ssp_value_gate` | the SSP's pre-payment value gate reads the **true** value, never an attacker-supplied hint: a child bundle has no on-chain-rooted branch, so `peek_pending_transfers` proves it with `verify_conveyed_child` and reports `child_state.out_value` — the value the ladder cryptographically commits to. Fails closed on any tamper |
| 38 | `sdk38_sponsor_stiff` | bounded loss on sponsored refresh: a sponsor that stiffs the user after the on-chain re-anchor costs only the fee — the user keeps the refreshed `amount − fee` coin and gets an explicit error |

#### Chaos

`SDK_E2E=22` is the concurrent chaos / property test — see [its own
section](#concurrent-chaos--property-test-sdk_e2e22).

### Retired numbers

These ids no longer exist. **Never cite them as live**; use the column on the right. Nothing lost
coverage silently — where a claim genuinely became meaningless it is marked *obsolete* with the
reason, and there is no repoint.

| Retired | Was | Its claim now lives in |
|---|---|---|
| 3 | latch swap legs | **sdk63** (pay), **sdk64** (receive), **sdk67** (non-exact receive) |
| 5 | Mercury → Lightning pay | **sdk63**, plus **sdk65** (non-exact) |
| 6 | Lightning → Mercury receive | **sdk64**, plus **sdk67** (non-exact) |
| 7 | practical unilateral exit | **sdk50** — the SDK walks the tier chain as each relative CSV matures; **sdk39** for a depth-2 token exit |
| 8 | terminal-node enforcement | **sdk58** — after an in-ladder split the parent IS terminal (budget consumed by `SP`) and a bundle naming a non-terminal parent is rejected; read directly in sdk04, sdk31, sdk32, sdk60 |
| 10 | receiver terminal-parent verify | **sdk58** + **sdk54**/**sdk55**; the un-laddered ancestor guard rides along in every colored flow (sdk02, sdk29, sdk31, sdk39, sdk52) |
| 13 | stale-state broadcast defeated | **sdk51** (the lowest-CSV state wins a contested exit), **sdk40 PART 2** (a stale state dies at consensus; cooperative de-trigger), **sdk45** |
| 14 | watcher race | **sdk51**, **sdk45** (keyless tower), **sdk40 PART 2** |
| 18 | LN pay failure + reclaim | **sdk68** (exact whole-coin reclaim) + **sdk66** (non-exact rollback) |
| 26, 27 | invalidation at scale / over time | *obsolete under TES-R* — idle coins never age and rent is 0 vB, so there is no aging left to invalidate; the ladder plus terminality subsume the finite-ladder model. **No repoint exists, because the phenomenon does not** |
| 28 | granularity (plain sats) | *obsolete* — the in-ladder split carries its own fee model (`tier_out_total` / `committed_fee_for_outputs` / `min_child_value`); the old backup-fee floor no longer governs a split |
| 33 | auto-refresh | *obsolete* — there is no ladder floor to approach; unbounded **off-chain** renewal is **sdk43** |
| 35 | trust boundaries | **sdk45** (the watch bundle carries NO key material; a second independent tower is idempotent — both back-filled before sdk35 was retired), **sdk52** (a carrier is never laddered), **sdk51**, **sdk46**/**sdk47**/**sdk54** (the R′ census) |
| 61, 62 | — | never existed; the child work shipped as **sdk60** (+ **sdk17**) |

### Off-chain DAG primitives (`RGB_E2E=1..14`)

The low-level suite under the SDK, driving the un-laddered off-chain DAG directly: off-chain split
(1), 2-input combine (2), 2-deep un-broadcast chain (3), SE single-use refusal (4), 3-input combine
(5), 3-level DAG (6), epoch deadline (7), wide combine (8), blinded/witness send-receive (9), history
+ self-transfer (10), UDA/CFA schemas (11), validate-offchain negative (12), consignment integrity
(13), metadata + IFA supply (14).

### Upstream Mercury suite (default `cargo +stable run`)

With no `SDK_E2E` / `RGB_E2E` / `LN_SMOKE` set, the binary runs the vanilla protocol tests
(tb01–tb05, tm01, ta01–ta03, tv01): simple transfer, address reuse, atomic transfer, **lightning
latch**, timelock, sender double-spend, deposit edge cases. These call `mercuryrustlib` directly and
never run the SDK's `claim()`, so they exercise the **un-laddered** shape — the signed-once backup
chain with decrementing absolute locktimes. Run this after any change to transfer/receiver code: the
branch-transfer extension must stay regression-clean against it.

### Lightning harness smoke (`LN_SMOKE=1`)

Two `rgb-lightning-node` daemons, a funded channel, a real BOLT11 paid end to end. Use it to prove
the RLN half of the stack is healthy before blaming an LN E2E.

### Unit tests

```bash
cargo +stable test -p mercury-utexo-sdk   # coin selection, config, invoices, SSP, watchtower, doctest
cargo +stable test -p mercurylib          # the TES-R primitives (lib/src/tesr.rs)
```

`mercurylib`'s `tesr` tests are pure, stack-free consensus math: a split state conserves value and
scales its fee with output count (and rejects mint/burn), a child's tiers root at `SP.out[j]` for
arbitrary `j`, the P2A script is `OP_1 <0x4e73>`, `csv_blocks` sets a relative *block* lock (disable
and type bits clear), the **trigger's sequence disables the relative lock entirely**, tier value
decrements by fee + anchor, and the `TesrParams` schedule decrements and clamps at its floors with
correct renewal / rollover thresholds.

The SDK crate also carries two executable **models** — pure companions to the specs, calling the
real production functions wherever a callable pure one exists:

* `invalidation_model.rs` ↔ [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md) — the un-laddered shape's
  backup-locktime ladder (`mercurylib::transaction::calculate_block_height`), split fee reserve and
  admission guard, payment planning, deposit-anchored deadlines.
* `granularity_model.rs` ↔ [GRANULARITY-SPEC.md](../GRANULARITY-SPEC.md) — exact subsets, the split
  floor, the fixed 1500-sat token piece, ceil fee arithmetic.

## Adversarial coverage map (mirrors Spark)

| Spark test theme | Covered by |
|---|---|
| double-claim / duplicate leaf | ta02, ta03 (duplicate deposits), tm01 (sender double-spend), sdk04 (claim idempotence) |
| conflicting off-chain spend | RGB_E2E=4 (SE single-use refusal), sdk04 (a terminalized split parent refused twice over), sdk12 Part C (secnonce reuse) |
| wrong preimage / locked claim | tb04 (the latch itself), sdk64 (the SE reveals the preimage only once the payee's coin is claimable), sdk19 (never paid ⟹ no preimage), sdk25 (claim past the latch window is refused) |
| transfer interrupt / resume | tb01+tb02 paths; `claim()` is idempotent per message (sdk04); sdk56 (a replayed `/sign/second` returns the cached signature and does not advance the count) |
| exit-race ordering | sdk41 and sdk49 (the receiver's state carries the strictly lower CSV and matures first), sdk51 (that ordering wins a *contested* exit), sdk54 (a hidden lower-CSV state cannot be smuggled past the census), sdk55 (an inverted backup ladder is rejected), tb05 for the un-laddered locktime ladder |
| griefing / forced exit | sdk45 (a keyless tower defends an offline owner after a hostile trigger), sdk40 PART 2 (cooperative de-trigger) |
| invalid consignment | receiver hook rejects (`validate_offchain_chain`) — sdk02 asserts the valid path, RGB_E2E=12 the negative, RGB_E2E=13 consignment integrity |
| value inflation at the operator boundary | sdk37 (the SSP gate reads the ladder-committed value, not a sender hint), sdk20 (wrong-recipient + undersized refused), sdk58 (11 `verify_child_bundle` census attacks) |

## Concurrent chaos / property test (SDK_E2E=22)

A soak test for the bugs that only appear under real parallel usage. `CHAOS_USERS` wallets (one
sqlite db each) run weighted-random actions CONCURRENTLY against the live SE + lockbox — enter
(deposit), send, claim, respend (deepen the DAG by another hop), split, unilateral exit (including
at a DAG point), cooperative withdraw — plus a low-probability **cheat**. There are two cheats, both
"broadcast an old state" claw-backs that must be refused:

1. **steal-after-send** — capture a coin's pre-signed backup, legitimately send the coin away, then
   broadcast the now-stale backup to claw it back.
2. **steal-after-split** — capture the backup, then split the coin (its value moves into fresh
   sub-coins), then broadcast the stale pre-split backup.

A background miner confirms deposits and matures exits; a semaphore caps concurrent SE co-signing;
all bitcoin-core shell-outs serialise through one mutex. Every attempt and result is traced to
`{run_dir}/chaos.jsonl`.

After a quiescent settle a **spec-invariant oracle** (`chaos22_oracle`) audits the trace + final live
state:

- **No value created** (INV-1/13/25): Σ SE-side balances + Σ exited-on-chain ≤ Σ deposited (tight:
  the residual is realised fees).
- **No cheat succeeded** (INV-5/18/19): every stale-state broadcast was refused, and on-chain the
  funding outpoint was never spent by the cheater's stale tx (`spender_of` backstop).
- **Single custody per `statechain_id`** (INV-18/19) and non-negative balances (INV-9).
- **All outcomes expected**: `classify()` separates spec-sanctioned contention from unclassified
  errors, and any unclassified error is a breach. Recognised classes include insufficient balance,
  no-coin / no-exact-coin, terminal (single-use / spend budget / already spent), epoch deadline,
  nonce guard, batch lock, mempool conflict, non-final, input-spent, dust and split-fit — plus the
  laddered-shape limits it legitimately hits in a long run: **`csv-floor`** (replace-by-lower-timelock
  has finite depth — at the floor a coin must be exited or re-anchored rather than re-sent) and a
  child too small to split into a viable piece + change. The "coin has a ladder and cannot be split
  as plain BTC" refusal is deliberately left UNCLASSIFIED so a real routing regression still shows up
  as a breach.

```bash
# smoke (fast): 5 users, 20s
SDK_E2E=22 CHAOS_USERS=5 CHAOS_SECS=20 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo +stable run
# full: 100 users, 120s, 8 whales
SDK_E2E=22 CHAOS_USERS=100 CHAOS_SECS=120 CHAOS_WHALES=8 CHAOS_INFLIGHT=24 ... cargo +stable run
```

Other knobs: `CHAOS_DEPOSIT_SATS` (default 2_000_000), `CHAOS_CHEAT_PROB` (0.06), `CHAOS_SEED` (42).

It never runs on the default `cargo run` path — `SDK_E2E=22` must be set explicitly — so ordinary CI
stays fast; the full-matrix runner picks it up at the small default size. **This test found real
robustness bugs**: SE worker panics on DB-pool exhaustion (P2-1 — request-path `unwrap()`s in
`endpoints/utils.rs` and `database/utils.rs` replaced with graceful failures, SE pool raised 10 → 50)
and a client panic on an unexpected SE error body. The protocol stayed *safe* throughout (value
conserved, cheats refused); only liveness failed. RGB-over-chaos is a follow-up — the harness runs
pure sats today.

## Running the whole matrix

`clients/tests/run_all_suites.sh` runs the unit tests, then **every** `SDK_E2E` and `RGB_E2E` index
it discovers by grepping the dispatch in `main.rs` (so it tracks the live set automatically), then
the LN smoke, then the upstream suite. It captures per-test stdout/stderr plus time-sliced docker
logs (mercury-server, lockbox, electrs) into `$LOGDIR` (default `/tmp/utexo_suite_logs`) with a
`summary.txt` of PASS/FAIL and durations.

```bash
./run_all_suites.sh                                # everything
ONLY="SDK_E2E=59 SDK_E2E=60" ./run_all_suites.sh   # a subset
TRACE=1 ./run_all_suites.sh                        # RUST_LOG debug for the client
SKIP_LN=1 ./run_all_suites.sh                      # skip the RLN-backed smoke
```

Prerequisites are the two stacks above plus the RLN binary built (`cd rgb-lightning-node && git
submodule update --init && cargo build`).
