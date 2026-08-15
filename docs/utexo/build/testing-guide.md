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
export UTEXO_ATTESTATION_IDENTITY=0x…                         # see below — without it, claim() refuses to ladder
export RLN_BITCOIND_CONTAINER=rgb-lightning-node-bitcoind-1   # the test faucet (this is also the default)
# Flows that shell out to the RLN stack (the Lightning group, and chaos22's miner):
export RLN_REGTEST=/path/to/rgb-lightning-node/regtest.sh
export COMPOSE_FILE=/path/to/rgb-lightning-node/compose.yaml
export COMPOSE_PROJECT_NAME=rgb-lightning-node
export RLN_BIN=/path/to/rgb-lightning-node/target/debug/rgb-lightning-node   # LN_SMOKE and the LN flows
```

> **`UTEXO_ATTESTATION_IDENTITY` is required for the SDK lane.** The client verifies the enclave's
> `utexo/sig_count/v2` attestation over `(statechain_id, num_sigs, sig_budget, nonce)` against a
> pinned identity, and resolution is compiled-in pin → config → **refuse**; it never falls back to
> the key the coordinator serves. No network has a compiled-in pin
> (`TesrParams::attestation_identity_const` returns `None` everywhere), so the pin has to be
> supplied. `regtest.Settings.toml` carries an `attestation_identity` line, but that file is read by
> `ClientConfig::load` — the `mercuryrustlib` lane. An SDK wallet is built through
> `ClientConfig::from_params` from `SdkConfig`, which never reads a Settings file and falls back to
> the environment variable. So the harness must export it (or each test must set
> `SdkConfig::attestation_identity`). Read the running lockbox's value from
> `GET /attestation_identity`; a differently seeded lockbox refuses rather than passes.
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

There is **one protocol**, and it produces two coin **shapes**. Knowing which shape a flow drives
tells you what its assertions are about. Full treatment in [PROTOCOL.md](../spec/PROTOCOL.md).

### Laddered — every plain BTC deposit

`claim()` establishes an exit ladder over every fresh confirmed root coin, unconditionally: funding
`F` → **trigger** `T` (no timelock, signed once at deposit) → **extension** `X_m` (relative CSV
`E_m`) → **state** `S_k` (relative CSV `Δ_k`). All three tiers are v3/TRUC with a P2A anchor, and all
three stay **un-broadcast**. There is no protocol-version field and no escape-hatch environment
variable; no flow pins any other lane.

BIP-68 relative timelocks only start counting once the *parent* confirms, and `T` carries no timelock
at all, so **nothing matures until someone broadcasts `T`**. What that means when you write or read a
flow:

* **No flow can "wait out" a CSV deadline.** Time is driven by broadcasting a trigger and then mining
  past each relative timelock (sdk40, sdk50). Mining at an idle coin does not move the CSV clock —
  sdk30 (a) mines 300 blocks and asserts the exit chain is byte-identical afterwards, `F` still
  unspent.
* **The calendar clock is a different clock.** A laddered coin also carries the flat backup chain of
  absolute locktimes, and `min(L_k)` is a real deadline held by prior owners: it loses a block to
  every block mined and `interval` to every whole-coin hop. sdk86 reads both clocks on a *received*
  coin across two hops, which is the shape a deposit-only flow structurally cannot witness. Cite it,
  not sdk30 (a), for the scope of "idle coins never age".
* **A transfer co-signs a fresh state one δ LOWER** than the one it replaces
  (replace-by-lower-timelock), so the new owner's state always matures first; the replaced state is
  disclosed as superseded and counted by the receiver's census (sdk41, sdk49, sdk54).
* **Renewal (a lower-CSV extension) and rollover (a fresh level) are off-chain and unbounded**
  (sdk43, sdk44). `refresh` is the **re-anchor** primitive: one on-chain tx that moves the coin to a
  fresh funding outpoint and mints a new ladder, killing every exit right rooted at the old `F`
  (sdk30 (b)).
* **A unilateral exit walks the pre-signed chain tier by tier**, waiting out each relative timelock
  (sdk50) — not a single backup broadcast. A keyless watch bundle lets a delegated tower do the same
  for an offline owner (sdk45, sdk51, sdk72 part B).

### Flat — RGB carriers and un-broadcast split sub-coins

Not every coin is laddered, by design. `SdkConfig::colored_ladder` ships **false**, so an RGB
**carrier** takes the flat **signed-once backup** shape: an uncoloured tier spend of `F` would
destroy the allocation (terminal freeze). A split sub-coin whose funding is un-broadcast likewise
cannot root a trigger. Both transfer by backup-chain handover with **decrementing absolute
nLocktimes** — each hop's backup is built one `interval` below the previous, so the current owner's
backup is always the first to become final and a superseded one is non-final whenever a fresh one is
spendable.

This shape is current and load-bearing. The RGB flows that inherit the shipped default ride it
(sdk09, sdk16, sdk39, sdk52, sdk73), and so does the upstream Mercury suite, which sits below the SDK
and never calls `claim()`. Its receiver-side guarantee is
`transfer_receiver::verify_terminal_parents` — one terminal ancestor per structural input.

**Which lane a flow is on is a property of its own config, not of the test's subject matter.** The
coloured ladder (CTES-R) is built and test-covered behind `colored_ladder = true`, and every flow on
that lane sets the flag by name on every wallet it builds: sdk02, sdk29, sdk31, sdk32, sdk34, sdk74,
sdk75, sdk77, sdk78, sdk79, sdk87, sdk88, plus RGB_E2E=15/16. A grep for `colored_ladder = true`
under `clients/tests/rust/src/` is the authoritative list. sdk74 additionally pins the default in the
direction it actually has — `SdkConfig::regtest(..).colored_ladder` must be false. Refusing the
combination of a coloured ladder and the legacy coloured split lane is `refuse_if_colored_ladder`.

### Non-exact payments — the in-ladder split

A payment that is not an exact subset of the sender's laddered coins runs the **in-ladder split**: a
state tier `SP` spending `X_m.out[0]` — a *descendant* of the trigger, never a rival for `F` —
paying a piece child and a change child, each with its own two-tier ladder. The admission floor is
`mercurylib::tesr::min_child_value` = `2·(committed_fee(rate) + P2A_VALUE) + dust`; at the shipped
`committed_fee_rate = 3.0` sat/vB (`TIER_VBYTES` 125, `P2A_VALUE` 240, `DUST_LIMIT` 330) that is
**1 560 sat**. The floor is a *function of the rate*, not a constant — a change leg on a spine uses
the strictly lower `min_spine_tip_value` (945 sat at the same rate) because it funds one rung, not
two. The parent is terminalized and its old owner state disclosed as superseded (sdk58, sdk59).

Received children are **first-class**: the claim completes the standard SE key handover, so the
receiver co-owns `A_child` (invariant across the rotation, which is what keeps the pre-signed child
tiers valid) and the sender is permanently locked out. A child pays onward off-chain whole
(`child_retransfer`) or split (`child_in_ladder_pay`, a depth-2 `ancestors` chain), gets its transfer
budget back in place via `renew_child`, and — once `SP` confirms — can be swept with its siblings by
`mercuryrustlib::combine::combine_leaves`. That last one is a **primitive with no caller outside a
test**: sdk83 drives it end to end, but nothing in the product reaches it, which is why the sweep
economics below are marked design. See [CHILDREN.md](../spec/CHILDREN.md).

## SDK end-to-end flows (`SDK_E2E=n cargo +stable run`)

74 flows plus the `chaos22` fuzzer. Numbers run to 91 and are **not** contiguous; the full-matrix
runner discovers the live set by grepping the dispatch, so there is no list to keep in step.

### Wallet, parity, guard rails

| n | Flow | Proves |
|---|---|---|
| 1 | `sdk01_wallet_flow` | deposit → exact-subset transfer → auto-claim → non-exact transfer (routes to the in-ladder split, sender keeps the change) → auto-claim → cooperative withdraw to L1. No sats/UTXO management surfaced to the app |
| 4 | `sdk04_adversarial` | SDK guard rails: typed `InsufficientBalance`; a split parent is terminalized by `SP` at the SE and booked WITHDRAWN, so a second full-value spend is refused twice over; claim idempotence; double-withdraw refusal |
| 11 | `sdk11_parity_methods` | parity API: identity message signing, multi-recipient sats transfer, Utexo invoices (create + fulfill), query/history |
| 12 | `sdk12_adversarial` | three independent parts on the laddered default. B: a non-exact payment lands the exact amount through `verify_child_bundle`. C: MuSig2 secnonce reuse — one `/sign/first`, two `/sign/second` — refused on the second. D: `/transfer/unlock` with a bad signature and a NULL `auth_pub_key` refused with 403 (real and unknown ids). C and D are SE/lockbox guards tested **nowhere else** |
| 15 | `sdk15_fresh_doublesign` | the honest trust floor: a *malicious SE* co-signing a RIVAL trigger over the same `F` is exactly as final as the owner's — `T` is locktime-free, so the CSV tiers break no tie and the contest degrades to a plain on-chain race |
| 16 | `sdk16_onboarding` | enter with nothing: a wallet with no deposit and zero balance receives BTC *and* an RGB asset, then exits unilaterally |
| 17 | `sdk17_oor_chain` | out-of-round transferability: alice → bob → carol where hop 2 is a **partial** re-spend of bob's received child (a child-level in-ladder split; the child becomes an intermediate `ancestors` segment and carol's exit walks a depth-2 chain). `F` stays unspent throughout |
| 71 | `sdk71_unconditional_ladder` | `claim()` ladders every coin it can with no opt-in, and the conveyance path refuses the flat lane to any coin that is not legitimately flat: an unreadable ladder record is refused rather than degraded, a bindable on-chain non-carrier with no ladder is refused as a bug, and a coin whose recorded reason is the *transient* `rgb-state-unavailable` is refused. Every skip is surfaced as a `WalletEvent::LadderSkipped` carrying a `LadderSkipReason` (`RgbCarrier`, `LadderUnreadable`, …) instead of being silent |
| 85 | `sdk85_transfer_cancel` | all four rows of `mercurylib::transfer::cancel`'s authorization table against a live coordinator: opened-but-never-conveyed (sender alone), conveyed-and-unclaimed (sender-only REFUSED by name, then released cross-wallet with the receiver's single-use consent), claimed (terminal), batched (governed by the latch, never the sender). Then the safety step: after a consented cancellation bob can never claim and the coin lands with carol — exactly one of them is paid. **Needs a coordinator rebuilt with the transfer-cancel migrations** (`0010_transfer_cancel.sql`, `0011_transfer_cancel_sender_key.sql`), which `sqlx::migrate!` embeds at compile time; against an older container every step fails at the first `cancel_transfer` |

### Ladder consensus, lifecycle, exit, defence

| n | Flow | Proves |
|---|---|---|
| 40 | `sdk40_tesr_consensus` | the consensus core against real bitcoind: un-broadcast immunity; `X` REJECTED before `E` confirmations of `T` and `S` REJECTED before `Δ` confirmations of `X`; a full unilateral exit with no operator cooperation. **PART 2**: cooperative de-trigger defeats a hostile trigger. The blind SE is unchanged — it blind-signs v3 + relative-timelock + P2A sighashes |
| 41 | `sdk41_tesr_transfer` | a transfer really moves control: `A` and `F` are invariant (no on-chain tx), only the shares rotate; Bob co-signs and exits a full ladder over the same `F`, and Alice's later co-sign attempt is refused — she is cryptographically out |
| 42 | `sdk42_tesr_lifecycle` | wallet-level lifecycle: establish → renew off-chain → persist to the wallet DB → reload as a fresh session would → unilateral exit **from the reloaded bundle** |
| 43 | `sdk43_tesr_rollover` | when the extension-CSV budget is exhausted the ladder rolls over **off-chain** to a fresh level (zero on-chain bytes), renewal keeps working at the new level, and the deep chain still exits |
| 44 | `sdk44_tesr_params` | the canonical `TesrParams` schedule drives `establish_auto` / `renew_auto` / `rollover_auto` — the cadence a real wallet runs — and the resulting ladder still exits (decrement + floor + `m_max` math) |
| 45 | `sdk45_tesr_watchtower` | a **keyless** WatchBundle (pre-signed tiers only, no key material — every tier pays the owner) lets a delegated tower drive an offline owner's exit after a griefer broadcasts the trigger; a second independent tower pass is idempotent |
| 46 | `sdk46_tesr_rprime` | the census `se_num_sigs == flat_backups + tier count` checked against the **real** SE sig count, not a mock: the count is read before and after establishing, the increment must equal the number of tier co-signs, and `verify_bundle` accepts the true count while rejecting a hidden extra signature |
| 47 | `sdk47_tesr_rprime_transfer` | a pre-established ladder carried across a transfer message and accepted by the receiver's verifier |
| 48 | `sdk48_v2_native_deposit` | `claim()` auto-establishes **and persists** the ladder for each fresh confirmed deposit, exiting to the wallet's seed-derived backup address; `num_sigs == 4` (deposit backup + `T` + `X` + `S`); a second `claim()` does not double-establish |
| 49 | `sdk49_model_a_transfer` | Model A: the sender pre-signs the receiver-paying state `S'` one δ lower; the receiver verifies it exits to **its own** key, adopts the ladder, and unilaterally exits it — the end-to-end proof that the receiver gets a complete self-custodial exit chain |
| 50 | `sdk50_v2_unilateral_exit` | the public `wallet.unilateral_exit()` walks trigger → extension → state as each relative CSV matures, reporting `wait_blocks` between tiers, until the funds land at the wallet's own backup address. No absolute-locktime backup is broadcast |
| 51 | `sdk51_v2_watchtower` | the contested case: *someone else* spends `F`, starting the CSV clock; the owner only runs `wallet.defend_ladders()` and wins because the adopted current state carries the strictly-lowest CSV. Also asserts the pass is a **no-op while the coin is idle** |
| 53 | `sdk53_v2_latch_guard` | a Lightning-latched transfer of a laddered coin OPENS — the SSP's pre-pay census (`peek_pending_transfers` → `ssp::execute_pay`) is what stands in the way of a rogue SSP, not a blanket refusal. The happy path is sdk63 |
| 54 | `sdk54_verify_bundle_adversarial` | the anti-theft count cannot be padded: `expected = flat_backups + tiers + superseded_states + superseded_extensions`, with superseded entries parsed, ladder-linked and signature-checked, and a `csv: None` not skipping the race check. Each attack that inflates `expected` to hide a low-CSV self-paying state is REJECTED while the honest bundle verifies |
| 55 | `sdk55_backup_chain_adversarial` | the conveyed backup chain cannot be **padded** (duplicate `tx1` inflating `expected`) or **inverted** (sender keeps the lower locktime so their stale backup matures first). `validate_backup_chain_v2`'s INV-5 `ladder_decrements_by_interval` rejects both, using the coin's real validly-signed backups |
| 56 | `sdk56_keystone_retry_idempotent` | the signing round is idempotent under retry: re-sending the exact same `/sign/second` returns the **identical** partial signature from cache and does **not** advance `sig_count` — a lost response cannot leave the count ahead of the disclosed tier set and brick the census |
| 57 | `sdk57_owner_share_binding` | the server records an **authoritative** aggregate per `statechain_id` (owner share + enclave share) and `/info/statechain` returns it equal to the coin's own aggregate x-only — the anchor that stops a rogue-key decoy defeating the child census |
| 70 | `sdk70_verifier_binding_adversarial` | three properties against real co-signed ladders. **A**: every chaining site reads an explicit `payload_vout` accessor, and a bundle declaring the wrong one is REJECTED with a named error — never accepted, never a panic, never a silent fall back to `output[0]`. **B**: a genuine, fully co-signed DECOY ladder over an attacker-owned outpoint that exits to the victim's key and balances the victim's `num_sigs` is accepted by `verify_bundle` and must be rejected by `verify_bundle_bound`. **C**: one co-sign, one census slot — a repeated genuine disclosed tier cannot be double-counted |
| 72 | `sdk72_watchtower_failloud` | the watchtower is never silently idle. **A**: a real enumeration failure (the RGB data dir is a regular file) makes `auto_exit_due` return `Err` with a `WatchtowerBlind` event and a retained, pollable `WatchtowerFault` that clears itself once repaired — instead of an empty carrier set that reads as "nothing to protect". **B**: sdk51's attack with the manual pass deleted — the owner only calls `start_background()` and the coin must still exit to the owner's key |
| 86 | `sdk86_received_coin_ages` | both clocks on a RECEIVED laddered coin over two hops. **A**: the CSV clock does not tick — 300 blocks idle and the exit chain is byte-identical, `F` unspent. **B**: the calendar clock does — `min(L_k)` loses those 300 blocks and one `interval` per whole-coin hop. This is the evidence for INV-27's real scope |
| 89 | `sdk89_plain_detrigger` | the PLAIN de-trigger through the wallet API: a griefer broadcasts alice's un-timelocked `T`, and `detrigger_to_owner` spends `T.out[0]` with **no** relative timelock, so it confirms ahead of every pre-signed extension. The coloured precondition is checked before anything is broadcast, so a coloured coin can never take this path |

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
| 58 | `sdk58_inladder_split` | `verify_child_bundle` ACCEPTS a real split child — `SP` is a state tier spending `X_m.out[0]`, the parent is terminalized and `S_0` disclosed as superseded, and the child's two-aggregate bundle (ancestors under `A_parent`, child tiers under `A_child`) checks out against chain + `/info/statechain`. Eleven adversarial census cases are REJECTED, including a non-terminal parent and a hidden lower-CSV state |
| 59 | `sdk59_inladder_pay` | the split is a usable **payment** through `transfer()` / `claim()` / `unilateral_exit()`: `transfer()` auto-routes to it, the piece child pays Bob (Model A) and is conveyed to his mailbox, the change child pays Alice back; Bob adopts via `verify_child_bundle` and exits the child to his own key |
| 60 | `sdk60_child_firstclass` | a **received** child re-transferred WHOLE off-chain (alice → bob → carol): the claim completes the key handover so `A_child` is invariant and Alice is locked out; `child_retransfer` co-signs a fresh state at a strictly lower CSV and discloses the replaced one; Carol's census counts the child-superseded segment and she exits. Two payments, zero on-chain footprint |
| 69 | `sdk69_transfer_many_inladder` | `transfer_many` on a LADDERED parent: a plain split is refused, then one `SP` over `X_m.out[0]` carries two recipient children + change + P2A with `F` untouched. The trigger-race attack is then executed for real — alice broadcasts her retained, un-timelocked trigger and spends `F` — and both recipients still exit unilaterally for their exact amounts, because `SP` descends from that trigger instead of racing it |
| 76 | `sdk76_received_parent_split` | splitting a RECEIVED laddered coin. The ancestor census's `flat_backups` term must be the parent's real count, not a constant: every whole-coin hop co-signs one more flat backup, so a parent received `k` times carries `1 + k`. Bob's count is asserted from his own backup rows, then he pays carol non-exactly and carol's child must be adoptable. sdk58/59/69 all deposit the parent, so `k = 0` and are blind to this |
| 80 | `sdk80_plain_child_split_watchtower` | `child_in_ladder_pay_many` conveys every grandchild before writing the child's durable status, and the child's own record is never rewritten — so this wallet's watchtower loop is admitted to drive a superseded state while strangers already hold the bundles that supersede it. The ordering must close that window |
| 81 | `sdk81_inladder_split_recovery` | an in-ladder split killed by SIGABRT (`UTEXO_CRASH_POINT=after_inladder_sp_sign`) the instant `SP`'s co-signature is journalled — before either child ladder exists — leaves the parent PERMANENTLY terminal at the SE. `recover_in_ladder_splits` replays the write-ahead journal, completes both children, and the original payment still lands |
| 82 | `sdk82_exit_headroom_gate` | the exit-headroom admission gate: a child conveyed so near the end of its funding epoch that its unilateral exit provably cannot finish before the sender's flat backup can spend `F` is REFUSED by the receiver, naming the shortfall; the same payment in a fresh epoch is adopted. The gate computes from the SIGNED `nSequence` of the actual chain, not from a serde field on the conveyed bundle |
| 83 | `sdk83_leaf_combine` | `mercuryrustlib::combine::combine_leaves`: one spine batch carves five leaves, four of them the recipient's; the shared prefix `T → X_m → SP` is walked on chain until `SP` CONFIRMS, then THREE of those four are swept into ONE consolidated UTXO by a single 3-input transaction carrying no timelock, and the fourth — deliberately left out — still exits on its own pre-signed tiers. The refusals are exercised for real: over an un-broadcast `SP` (blind), over a mempool `SP` (replaceable), and over a coloured leaf (allocation-destroying), each side-effect free |
| 84 | `sdk84_leaf_renewal` | a leaf's transfer budget is replenishable: `renew_child` rebuilds both leaf tiers IN PLACE over the same `SP.out[j]` for zero on-chain bytes and no depth. The flow walks a leaf through every hop of every epoch on the regtest schedule, renewing between them, and settles the safety property on chain — the transaction that finally takes `SP.out[j]` is the RENEWED extension, and every superseded one loses the maturity race. The refusals must name the right remedy, and a renewal that does not strictly lower the extension rung is refused with no co-signature burned |

### Lightning

Both directions run on the ladder through a **HODL-invoice latch** — see
[LIGHTNING.md](../spec/LIGHTNING.md). The LN-latched piece is the one case that stays terminalized
(it sits unclaimed past the pending-transfer lock's window).

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
| 9 | `sdk09_ifa_batch` | IFA (inflatable) issuance, on-chain mint bound to a new statechain coin, and a batch multi-recipient transfer in **one** colored split, each receiver validating its own consignment amount |
| 29 | `sdk29_granularity_tokens` | raw-unit precision on the coloured lane: three wallets paid in ONE in-ladder split down to 1 raw unit (`precision` is contract metadata the SDK never scales), each booking what its own consignment assigns. A received coloured child **cannot be subdivided at all** — `ChildTesrBundle::colored_child_seals` refuses it structurally, not arithmetically. A fully-spent carrier leaves **no plain BTC change**: `F` is wholly consumed by `T`, and `colored_in_ladder_pay` carves no change child when the allocation is fully paid out, so conservation is asserted as `Σ children == colored_tier_out_total`. Paying more than any single carrier holds SUCCEEDS via `colored_multi_carrier_transfer` (one split per carrier, N pieces). Plus idempotent double-receive, and a five-tier coloured child exit measured with the read-only `color_psbt` stock probe |
| 31 | `sdk31_token_combine` | an amount spanning several carriers, on the coloured lane — and **there is no combine transaction**. Each carrier's `F` is already spent by its own trigger, so the payment is one in-ladder split per carrier, each conveying a coloured child; the recipient's declared shares sum to the amount paid (bob 100 / alice 10) from two DIFFERENT carriers. The invalidation property the old N-terminal-ancestor rule enforced is asserted **per leg**: every `SP` spends its parent's `X_m` payload output and never `F`, and every source carrier is terminal at the SE. A leg that skipped terminalisation fails |
| 32 | `sdk32_token_over_time` | tokens are never lost by inactivity, on the coloured lane. The carrier IS laddered and every rung carries a valid RGB state transition (`bundle.is_colored()`, `colored_ladder_health` validating the full allocation against the ladder's own un-broadcast txids). An idle coloured ladder never ages — no tier reaches the chain, `F` stays unspent. The invariant is proved positively rather than by absence: the three RGB-unaware routes to a carrier are each refused by name — plain-BTC coin selection, the uncoloured in-ladder split (`refuse_uncolored_over_colored`), and the flat ladder conveyance |
| 34 | `sdk34_token_watchtower` | `auto_exit_due` materializes a **received** carrier nearing its clawback deadline so a malicious sender cannot claw the tokens back. On the coloured lane a received piece has **no `branch-` row at all** — its exit material is the five-tier `ctesr-` chain `T → X_m → SP → ext_child → state_child`, and the watchtower drives that through `unilateral_exit` against a deadline head-started by the walk's own Σcsv. An ISSUED carrier has no ancestor (no `branch-`, no `ctesr-` bundle of its own), so no stale backup can race it and it is left untouched at any margin |
| 39 | `sdk39_depth2_token_exit` | a token piece **two** colored splits deep exits on-chain end-to-end (branch broadcast root-first, on-chain root spent) with the allocation preserved and no SE involvement |
| 52 | `sdk52_v2_rgb_carrier` | terminal-freeze at the shipped default: in one wallet the plain deposit carries a ladder and the RGB carrier carries none, and an off-chain token transfer still settles 750/250 — the two shapes coexist |
| 73 | `sdk73_structural_recovery` | a colored split killed by SIGABRT (`UTEXO_CRASH_POINT=after_structural_sign`) the instant the signed material is journalled — the window in which the carrier is already terminal at the SE and its one spending transaction lives only in process memory. The parent restarts from the same database, runs the recovery reader, and completes the ORIGINAL payment |
| 74 | `sdk74_colored_ladder` | with `colored_ladder` ON, `claim()` builds a ladder whose every tier carries a valid RGB state transition: one `OP_RETURN` per tier at vout 0, payload at vout 1, P2A at vout 2, `payload_vout` threaded from the builder's returned index rather than assumed, each tier chaining through its parent's DECLARED payload vout, and the committed fee exactly `committed_fee_for_outputs(n + 1, rate)`. The plain deposit in the same wallet stays byte-identical |
| 75 | `sdk75_colored_exit` | the unilateral exit of an RGB allocation: `T → X_0 → S_0` all CONFIRMED (not merely mempool-accepted), each spending its parent's declared payload vout, with the allocation intact at the end — no SE, no counterparty, only blocks |
| 77 | `sdk77_colored_inladder_split` | a coloured carrier pays PART of its allocation: a coloured `SP` over `X_m`'s payload output (a descendant of `T`, not a rival of it) with a headless coloured ladder per child, so five coloured tiers stand between `F` and the recipient's key |
| 78 | `sdk78_uncolourable_carrier` | on a wallet with the flip ON, a carrier that can never be coloured must be neither stranded nor rescued by weakening anything. The floors are read from `mercuryrustlib::tesr` rather than copied, and repeated `claim()` passes must not colour a carrier below the coloured root floor |
| 79 | `sdk79_split_watchtower` | the sender's own watchtower must not destroy the recipient's allocation. `colored_in_ladder_pay` stores the terminalized parent segment back to the sender's own record BEFORE conveying anything, so the sender's row names `SP` — byte-identical to the state in the recipient's conveyed child — and the replaced `S_0` appears among the superseded states rather than in the exit chain |
| 87 | `sdk87_carrier_deadline` | the CARRIER variant of the deadline pass: `deadline_safety_due`'s cooperative route excludes carriers permanently (a re-anchor destroys the allocation), but the unilateral route must cover them. The forced action is the coin's own pre-signed `T`, which does not re-aggregate — and the allocation must survive it |
| 88 | `sdk88_carrier_headroom` | the CARRIER variant of sdk82's headroom bound. The gate is colour-blind by design (it reads the signed `nSequence` of the actual chain), but a coloured tier is dearer — 168 vB against 125 — so the same sats buy different headroom, and the refusal on this lane must leave NOTHING booked |

### Operations: re-anchor, fees, sponsorship

| n | Flow | Proves |
|---|---|---|
| 30 | `sdk30_refresh` | **(a)** mine 300 blocks at an idle k=0 deposit and the exit chain is byte-identical (same txids, same relative CSVs), `F` still unspent, balance untouched — the CSV half of "idle coins never age" (sdk86 carries the received-coin case and the calendar half). **(b) re-anchor** — `refresh` cooperatively spends `F` into a brand-new aggregate, minting a new statechain id with its own fresh ladder and permanently killing every exit right rooted at the old `F`. User-pays mode (fee deducted from the coin) |
| 36 | `sdk36_derived_tokens` | split / combine / refresh slots are **free derived slots** (`POST /deposit/get_derived_token`, gated on the parent's owner auth with a single-use nonce and a per-parent lifetime cap) — they never consume a paid onboarding token, and a derived-slot coin is an ordinary transferable coin |
| 37 | `sdk37_ssp_value_gate` | the SSP's pre-payment value gate reads the **true** value, never an attacker-supplied hint: a child bundle has no on-chain-rooted branch, so `peek_pending_transfers` proves it with `verify_conveyed_child` and reports `child_state.out_value` — the value the ladder cryptographically commits to. Fails closed on any tamper |
| 38 | `sdk38_sponsor_stiff` | bounded loss on sponsored refresh: a sponsor that stiffs the user after the on-chain re-anchor costs only the fee — the user keeps the refreshed `amount − fee` coin and gets an explicit error |

### Concurrent chaos / property test (`SDK_E2E=22`)

A soak test for the bugs that only appear under real parallel usage. `CHAOS_USERS` wallets (one
sqlite db each) run weighted-random actions CONCURRENTLY against the live SE + lockbox — enter
(deposit), send, claim, respend (deepen the DAG by another hop), split, unilateral exit (including at
a DAG point), cooperative withdraw — plus a low-probability **cheat**. There are two cheats, both
"broadcast an old state" claw-backs that must be refused:

1. **steal-after-send** — capture a coin's pre-signed backup, legitimately send the coin away, then
   broadcast the now-stale backup to claw it back.
2. **steal-after-split** — capture the backup, then split the coin (its value moves into fresh
   sub-coins), then broadcast the stale pre-split backup.

A background miner confirms deposits and matures exits; a semaphore caps concurrent SE co-signing;
all bitcoin-core shell-outs serialise through one mutex. Every attempt and result is traced to
`{run_dir}/chaos.jsonl`.

After a quiescent settle a spec-invariant oracle (`chaos22_oracle`) audits the trace + final live
state:

- **No value created** (INV-1/13/25): Σ SE-side balances + Σ exited-on-chain ≤ Σ deposited (tight:
  the residual is realised fees).
- **No cheat succeeded** (INV-5/18/19): every stale-state broadcast was refused, and on-chain the
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
    `d_floor`); at the floor a coin must be exited, renewed (`renew_child`, sdk84) or re-anchored
    rather than re-sent. Plus a child too small to split into a viable piece + change.
  - **`tip-not-conveyable`** — a spine TIP cannot be handed over whole, because the `spinetip-`
    conveyance builder **is not landed**. This is classified only because the refusal comes by name
    from `execute_ex`; a tip reaching the flat lane and dying on the ABSENCE of backup rows must stay
    a BREACH. The tip is not stranded by it — pay FROM it with a spine batch, or exit it unilaterally.

  The "coin has a ladder and cannot be split as plain BTC" refusal is deliberately left UNCLASSIFIED
  so a real routing regression still shows up as a breach.

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

## Off-chain DAG primitives (`RGB_E2E=1..16`)

The low-level suite under the SDK, driving the flat off-chain DAG directly.

| n | Covers |
|---|---|
| 1 | off-chain split |
| 2 | 2-input combine |
| 3 | 2-deep un-broadcast chain, validated over both un-broadcast witnesses |
| 4 | SE single-use refusal — a second conflicting spend of a node |
| 5 | 3-input combine |
| 6 | 3-level DAG (split → combine → split), 3-witness chain |
| 7 | epoch deadline: the SE co-signs inside the active period, refuses past it, and a unilateral exit needs no SE call |
| 8 | wide combine — N manufactured sub-coins into one payment + change |
| 9 | blinded and witness send/receive |
| 10 | history and self-transfer semantics |
| 11 | UDA / CFA issuance schemas |
| 12 | `validate_offchain_chain` negative |
| 13 | consignment integrity |
| 14 | metadata + IFA supply |
| 15 | the coloured tier builder and per-tier seal blinding: coloured 1-payload and N-payload tiers at exactly 2.000 sat/vB, off-chain validation against an un-broadcast txid, and the ≥3-rival / non-minimum-internal-txid test. Needs bitcoind + electrs + the RGB proxy only — no coordinator, no lockbox |
| 16 | why a legacy-lane carrier cannot be coloured: reproduces sdk78's `Invalid coloring info` over an above-the-floor piece and names the cause (the legacy receive path never accepts the transfer into the RGB stock); the control accepts the same consignment through `accept_ladder` and the same piece colours. Same reduced stack as 15 |

## Upstream Mercury suite (default `cargo +stable run`)

With no `SDK_E2E` / `RGB_E2E` / `LN_SMOKE` set, the binary runs the vanilla protocol tests in order:
`tb01_simple_transfer`, `tb02_transfer_address_reuse`, `tb03_simple_atomic_transfer`,
`tb04_simple_lightning_latch`, `tb05_timelock`, `tm01_sender_double_spends`,
`ta01_sign_second_not_called`, `ta02_duplicate_deposits`, `ta03_multiple_deposits`, `tv01`. These
call `mercuryrustlib` directly and never run the SDK's `claim()`, so they exercise the **flat**
shape — the signed-once backup chain with decrementing absolute locktimes. Run this after any change
to transfer/receiver code.

## Lightning harness smoke (`LN_SMOKE=1`)

Two `rgb-lightning-node` daemons, a funded channel, a real BOLT11 paid end to end (the flow asserts
`invoice_status == "Succeeded"`). Use it to prove the RLN half of the stack is healthy before blaming
a Lightning flow. Honours `RLN_BIN`.

## Repo invariants (`cargo test -p ci-guards`)

`ci-guards` is a dependency-free crate whose "tests" are source scans over the repo: they read files
relative to the crate manifest, so they need no stack, no network and no toolchain pin, and they are
always cheap to run. Each guard pins a property that per-site fixing failed to hold. A source scan
establishes presence, absence and ordering — it does **not** establish reachability, binding or
behaviour, which is what the E2E flows are for.

| Guard | Property |
|---|---|
| `deny_armed_tower_during_conveyance` | every lane that hands value away must move the coin out of `CONFIRMED` **before** the superseding co-sign, so this wallet's own tower can never be armed against its own recipient |
| `deny_chain_anchored_token_balance` | a token balance may not depend on the SHAPE an allocation arrived in |
| `deny_colored_backup_on_a_colored_ladder` | on a coloured ladder a flat backup is a hop backup over `F` that an ancestor may hold, so it must be plain — and both acceptance paths must say so |
| `deny_flat_ladder_config_drift` | every deployment config must agree with `TesrParams::flat_ladder_params`; `interval` is the yardstick INV-5 measures each flat-backup hop against, so clients compile it in and refuse a coordinator that disagrees |
| `deny_line_number_citations_in_normative_docs` | a normative document may not cite code by line number. The normative set is DERIVED from `docs/utexo/spec/README.md`'s `*Normative*` labels, not hand-listed |
| `deny_optional_deadline_safety` | the deadline defence is unconditional; routine re-anchoring is not |
| `deny_relative_budget_mirror` | the coordinator and the enclave must be handed the SAME spend-budget quantity — one is relative, one absolute, and mixing them is how the two enforcers disagree |
| `deny_rgb_witness_apis` | `update_witnesses` / `upsert_witness` stay unreachable from this tree: one call with the plain blockchain resolver silently archives every rung of a deliberately un-broadcast coloured ladder |
| `deny_selection_without_exit_material` | a coin may not be offered to a payment unless this wallet holds material to EXIT it (a `tesr-` / `ctesr-` / `spinetip-` bundle, or a flat backup chain) |
| `deny_sender_declared_ladder_gate` | the JS clients' laddered gate must key on coordinator-served evidence, not on fields the sender fills in |
| `deny_sender_declared_margin` | the supersession margin keys on the live rival's STRUCTURAL kind (`δ` for a state, `δE` for an extension), not on a one-block lead |
| `deny_silent_degradation` / `deny_swallowed_backup_reads` | the silent-degradation class: a failure that presents as a benign empty or idle result. An empty carrier set, an empty branch-witness set, a spend generation of zero, an absent deadline — each is one line turning `Err` into a default, and the default always stands down |
| `deny_stale_depth_cap` | a document may not publish a superseded split-depth cap. The admitting rule is `check_exit_headroom_with_margin`, which adds `exit_slack_margin` |
| `deny_unattested_num_sigs_reader` | `num_sigs` enters the client only through the attested reader — unattested, a coordinator under-reporting it by `k` hides `k` co-signed rival states and the census still balances |
| `deny_unattested_terminality` | terminality comes from the enclave's signature, not from the coordinator's Postgres |
| `deny_uncoloured_legs_under_a_coloured_sp` | a coloured spine batch may not build plain legs under its coloured `SP` |
| `deny_unconsumed_slot_vouchers` | a derived-slot voucher becomes a deposit address only through `create_child_slot_addr`, so the SE-side spend and the on-disk pool move together |
| `deny_uncovered_carrier_deadline` | the carrier lane must not be the one lane with no automatic deadline coverage |
| `deny_unpinned_wire_error_codes` | `TransferReceiverError`'s variants are the interface; every client profile must carry all of them |
| `deny_unqualified_keyless_rescue` | the keyless tower's stated INCAPABILITY stays stated — it broadcasts pre-signed tiers at their committed fee and cannot fee-bump them |
| `deny_unstated_census_shape_obligation` | the shape rules that discharge the census's distinctness premise must say that they do |

## Unit tests

```bash
cargo +stable test -p mercurylib          # TES-R primitives, transfer cancellation, the P2A fee child
cargo +stable test -p mercury-utexo-sdk   # coin selection, config, invoices, watchtower, refresh, doctests
cargo +stable test -p mercuryrustlib      # core RPC, tower float
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
config → refuse, with a test asserting that no network has a compiled-in pin until an enclave is
provisioned.

The SDK crate also carries two `#[cfg(test)]` **models** — pure companions to the cost and
granularity write-ups in [learn/](../learn/), calling the real production functions wherever a
callable pure one exists:

* `invalidation_model.rs` — the flat shape's backup-locktime ladder
  (`mercurylib::transaction::calculate_block_height`), `transfer::split_fee_reserve`,
  `transfer::split_amounts`, `select::plan`, `wallet::deposit_anchored_deadline`,
  `types::ExitCostEstimate::fee_sats_at`, `types::is_terminal`, and the exit model in
  `config::tesr_exit_vbytes` / `tesr_exit_txs` / `tesr_exit_wait_blocks` (production code, because
  `SdkConfig::auto_exit_margin_blocks` derives from it).
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
| `clients/libs/rust/tests/live_info_config.rs` | a coordinator at `STATECHAIN_ENTITY` (default `http://127.0.0.1:8000`) | the compiled-in `initlock` / `interval` table matches the coordinator actually deployed. A unit test can only check the table against itself |
| `clients/libs/rust/tests/live_p2a_package_rescue.rs` | `CORE_RPC_URL` / `CORE_RPC_USER` / `CORE_RPC_PASS`, a node whose `minrelaytxfee` exceeds the tier's committed rate, `ELECTRUM_URL` for the seam test; `REQUIRE_LIVE_NODE=1` turns a skip into a failure | an under-paying v3 tier is refused alone and rescued **through this repo's own code path** (`mercurylib::wallet::p2a_fee_child::build_p2a_fee_child`), not a hand-run `bitcoin-cli`. Also covers anchor-squatting and the broadcast seam |
| `clients/libs/rust/tests/live_tower_float.rs` | `CORE_RPC_URL` and friends | what actually bounds a funded tower: under TRUC a v3 fee child may have one unconfirmed ancestor, and the tier is already it — so simultaneous-rescue capacity is the number of CONFIRMED fee UTXOs held, not the number of sats |

## Adversarial coverage map

| Theme | Covered by |
|---|---|
| double-claim / duplicate leaf | ta02, ta03 (duplicate deposits), tm01 (sender double-spend), sdk04 (claim idempotence) |
| conflicting off-chain spend | RGB_E2E=4 (SE single-use refusal), sdk04 (a terminalized split parent refused twice over), sdk12 Part C (secnonce reuse) |
| wrong preimage / locked claim | tb04 (the latch itself), sdk64 (the SE reveals the preimage only once the payee's coin is claimable), sdk19 (never paid ⟹ no preimage), sdk25 (claim past the latch window refused) |
| transfer interrupt / resume | tb01+tb02 paths; `claim()` idempotent per message (sdk04); sdk56 (a replayed `/sign/second` returns the cached signature and does not advance the count); sdk73 and sdk81 (SIGABRT in the signed-but-unpersisted window, recovered from the journal) |
| exit-race ordering | sdk41 and sdk49 (the receiver's state carries the strictly lower CSV and matures first), sdk51 (that ordering wins a *contested* exit), sdk54 (a hidden lower-CSV state cannot be smuggled past the census), sdk55 (an inverted backup ladder is rejected), sdk84 (the renewed extension beats every superseded one on chain), tb05 for the flat locktime ladder |
| griefing / forced exit | sdk45 (a keyless tower defends an offline owner after a hostile trigger), sdk40 PART 2 and sdk89 (cooperative de-trigger, plain lane), sdk72 part B (the defence runs from `start_background` alone) |
| the conveyance window | sdk90 (the payer's local gates), sdk91 (the coordinator's one-hour gate, probed directly with a genuine credential) |
| bundle binding / decoy ladders | sdk70 (`verify_bundle_bound` against a genuine decoy ladder over an attacker-owned outpoint; wrong `payload_vout` fails closed; a disclosed tier cannot be double-counted), sdk57 (the authoritative sid → aggregate binding) |
| invalid consignment | the receiver hook rejects: `mercury_rgb`'s `validate_offchain_chain` on the flat lane (RGB_E2E=12 the negative, RGB_E2E=13 consignment integrity), and on the coloured lane `UtexoWallet::colored_child_health` / `colored_ladder_health`, which book what the CONSIGNMENT assigns and never the sender's declared field (sdk02, sdk29, sdk32) |
| value inflation at the operator boundary | sdk37 (the SSP gate reads the ladder-committed value, not a sender hint), sdk20 (wrong-recipient + undersized refused), sdk58 (11 `verify_child_bundle` census attacks), sdk76 (the census's `flat_backups` term is the parent's real count) |
| a payee handed an unexitable coin | sdk82 (plain child, exit-headroom gate), sdk88 (the same bound on the coloured lane, where the loss is an asset), sdk78 (a carrier that can never be coloured is neither stranded nor rescued by weakening a floor) |
| a wallet racing its own recipient | sdk79 (the coloured sender's tower), sdk80 (the plain child-split lane's conveyance ordering) |

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
is set at the top of the script; export `UTEXO_ATTESTATION_IDENTITY` before invoking it.

```bash
./run_all_suites.sh                                # everything
ONLY="SDK_E2E=59 SDK_E2E=60" ./run_all_suites.sh   # a subset (UNIT and UPSTREAM are also labels)
TRACE=1 ./run_all_suites.sh                        # RUST_LOG debug for the client
SKIP_LN=1 ./run_all_suites.sh                      # skip the RLN-backed smoke
```

Prerequisites are the two stacks above plus the RLN binary built (`cd rgb-lightning-node && git
submodule update --init && cargo build`).
