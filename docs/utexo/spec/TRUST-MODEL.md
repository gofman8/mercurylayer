# Trust model — who trusts whom, what is verified instead, and what cannot be solved

> ## ⚠️ Direction of travel: ONE COIN TYPE — arrived; and since 2026-09-06, NO FLAT BACKUP anywhere
>
> The trust boundaries below were first written for two coin shapes — *laddered* (TES-R) and
> *un-laddered* (RGB carriers and branch-funded split sub-coins) — and then, for a while, for one
> shape that still carried a flat absolute-locktime backup chain BESIDE its ladder. **Both of those
> are gone.** A coin's ONLY exit material is its TES-R ladder, established at the FIRST MEMPOOL
> SIGHTING of its funding transaction, before confirmation (`check_deposit` in
> `clients/libs/rust/src/coin_status.rs` under `LadderAtSight::Plain`; the SDK's `claim()` establish
> pass under `LadderAtSight::Defer`, which ladders every un-laddered `IN_MEMPOOL` / `UNCONFIRMED` /
> `CONFIRMED` coin in the same pass, plain or COLOURED for a carrier whose allocation is booked).
> No coin carries an absolute-locktime backup transaction — none at deposit (`create_tx1` is
> deleted), none at any hop. The receiver REQUIRES the conveyed flat-backup vector to be empty
> (`verify_flat_backup_lane` and `refuse_conveyed_flat_backups` in `clients/libs/rust/src/tesr.rs`,
> `refuse_branch_material` in `clients/libs/rust/src/transfer_receiver.rs`), so the census is exactly
> `se_num_sigs == tiers + superseded` with the flat term pinned to 0 (`PARENT_V2_BASELINE = 0`,
> `CHILD_V2_BASELINE = 0`). The enclave count after a deposit is 3: `T`, `X_0`, `S_0`.
>
> **What follows for this document.** Every statement here that rested on a calendar — the root
> epoch, `min(L_k)`, the decrementing backup chain (INV-5, RETIRED: there is no chain to decrement),
> the deposit-anchored deadline, the "two clocks" of a laddered coin — is RETIRED in place below,
> dated, rather than deleted. Nothing on a coin matures on its own (INV-27 is unconditional):
> `coin.locktime` is `None` for life; the deadline passes (`deadline_safety_due`, `auto_refresh_due`
> in `clients/libs/rust-sdk/src/refresh.rs`) have no laddered subject, because their predicate reads
> that `None`; every watch-bundle entry exports `deadline_block: u32::MAX` and is event-driven; and a
> ladder is defended from the block its deposit is first seen in (`is_live_for_defence` =
> `IN_MEMPOOL | UNCONFIRMED | CONFIRMED`, the allowlist of `defend_ladders`, `unilateral_exit` and
> `export_watch_bundle`). A coin's off-chain life is bounded by renewal and rollover only; its
> on-chain cadence is the cooperative re-anchor at the renewal/rollover cap.
>
> **Read the residual as narrow, not as absent.** An RGB carrier is laddered like any other coin,
> with every tier carrying a valid RGB state transition (CTES-R), wherever the coloured builder can
> take it. Where it cannot *this pass* — allocation not booked yet, more than one allocation on the
> outpoint, RGB state unreadable — the coin is recorded `LadderSkipReason::RgbCarrier` and retried at
> the next `claim()`; until that pass succeeds it has NO exit material and cannot be conveyed. There
> is no flat lane to fall back to: the flat conveyance lane and its licence classifier are deleted,
> `is_legitimate_flat_reason` (`clients/libs/rust/src/transfer_sender.rs`) answers `false` for every
> reason, and `execute_ex` refuses a coin with no ladder row by name. A
> PLAIN ladder found over a carrier is recorded `PlainLadderOverCarrier`, and **there is no remedy
> for it in the tree**: `colored_reanchor` (`clients/libs/rust-sdk/src/refresh.rs`) refuses a
> plain-laddered coin by name ("use `refresh`"), and `refresh`'s plain re-anchor is the RGB-unaware
> spend that would destroy the allocation. Such a coin is exitable as satoshis only, must not be
> conveyed as a carrier, and its allocation is STRANDED unless a counterparty co-operates off this
> path — which is what the variant's own doc comment says. `SdkConfig::colored_ladder`
> READS the network's compiled-in enclave attestation pin instead of stating a bool, so colouring is
> ON where an identity is pinned (regtest) and OFF where none is (mainnet, testnet, signet) — not as
> a policy choice but because without a pin nothing the census rests on can be verified. On an
> unpinned network the pin's absence now bites harder than it used to, and the sentence to get right
> is the SDK one. RECEIVING needs the attested signature count (R5). So does the SDK's own establish
> pass: its bindability check calls `get_statechain_info`, which resolves the identity pin → config →
> REFUSE (`TesrParams::attestation_identity`, `lib/src/tesr.rs`), so on such a network that call
> fails, the pass records `LadderSkipReason::AttestationIdentityUnpinned` and ladders NOTHING —
> while `check_deposit` under `LadderAtSight::Plain` (`mercuryrustlib::coin_status::update_coins`)
> calls neither and ladders a plain deposit without any pin. **The consequence for an SDK wallet is
> not "deposits work, receiving does not".** Under `LadderAtSight::Defer` the deposit IS BOOKED —
> `check_deposit` sets the chain's status (`IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED`) and only the
> establish pass that follows is skipped — so the coin exists in the wallet with **NO exit material
> at all**: it cannot be conveyed (`execute_ex` refuses by name: "has no exit ladder and cannot be
> conveyed"), and it cannot be unilaterally exited (`unilateral_exit` has nothing to walk). The one
> route out is **COOPERATIVE WITHDRAWAL** — `withdraw::execute` reads no backup rows and needs no
> ladder, and it needs the SE. **That unilateral exit is what the flat backup used to provide, and it
> needed no attestation of any kind**; removing the flat chain removed it, and nothing replaced it on
> an unpinned network. Say the deployment state plainly rather than as a live regression: mainnet has
> no enclave provisioned at all, so there is no wallet in this state today — it is a NOT-YET-DEPLOYABLE
> network, and mainnet is waiting on an ENCLAVE, not on a decision (`SPEC.md` §0.4 V-6, B12).
>
> **What is NOT retired, and must not be read as retired: un-broadcast funding.** Colouring a tier
> cannot broadcast a funding output. Every in-ladder split child and every spine-tip change leg still
> rests on an un-broadcast `SP.out[j]` — that is what the design exists to produce — so every boundary
> below that turns on "this coin's funding output is not on chain" stands unchanged, B11 among them.
>
> **B2 is RETIRED with the branch lane (2026-09-06).** The terminal-parent proofs that were checked
> by COUNT rather than cryptographically bound to the branch inputs lived on the flat BRANCH lane. An
> independent review rated that CRITICAL; the answer was deletion rather than hardening, and the
> deletion has happened: `register_split_subcoins_n` and `register_combine_subcoins`
> (`clients/libs/rust-sdk/src/transfer.rs`) refuse by name, and the receiver refuses branch material
> (`refuse_branch_material`). The ladder never had the gap (R7).
>
> Two boundaries that do NOT go away, and should not be read as transitional: the single-SE trust
> unit itself, and availability within the CSV window once someone spends `F` — the ONE clock a coin
> has, and it starts on an event, never on a date (§5, B4). The **root epoch** that used to sit here
> — 10,000 blocks ≈ 69.4 days on mainnet, 1,000 on regtest — is gone with the flat backup it
> belonged to. `initlock` and `interval` survive in `GET /info/config` and in
> `TesrParams::flat_ladder_params` (`lib/src/tesr.rs`) only as compatibility constants, cross-checked
> against the compiled-in table and refused on disagreement (`info_config`,
> `clients/libs/rust/src/utils.rs`): `initlock` now names the FIXED EXIT WINDOW the split-depth cap
> measures a leaf's exit walk against (`enforce_split_depth_cap`, `enforce_exit_chain_length`,
> `clients/libs/rust/src/tesr.rs`), and `interval` is applied to nothing. Both re-anchors still exist
> — the plain one (`refresh`, one ~112-vB tx) and the coloured one (`colored_reanchor`: broadcast the
> already-co-signed trigger, then co-sign a coloured **de-trigger** whose relative lock is disabled —
> two txs, zero CSV wait, no SE change) — but neither is forced by a date. They are the on-chain
> cadence at the renewal/rollover cap, and neither is yet invoked automatically on the transfer
> path (renewal and rollover are library calls a wallet invokes by hand; the coloured re-anchor is a
> manual call — recorded here as a dated note, not described as built).
>
> Colouring is **wired and default-ON wherever an attestation identity is pinned**
> (`SdkConfig::colored_ladder` reads `TesrParams::attestation_identity_const`, `lib/src/tesr.rs`, so
> the two can no longer disagree): the claim path builds and co-signs a coloured ladder
> (`build_colored_ladder_auto` / `cosign_colored_ladder`), the coloured in-ladder split pays from it,
> `colored_reanchor` re-anchors it, and `defend_ladders` watches it. Where the pin is absent the flag
> is off and there is nothing else to run: a carrier on such a network has no ladder and no exit
> material until an identity is pinned. Which description applies is a property of the NETWORK, not
> of a preference.

> **Read every E2E citation in this document as "pending run" (2026-09-07).** 9ddc4bb rewrote most of
> the E2E suite to the rule and DELETED ten flows; the crate compiles and `cargo test` is green for
> `mercuryrustlib` (387 passed), `mercury-utexo-sdk` (152 passed) and `ci-guards` (32 suites, no
> failures), but **no E2E flow has been executed against the regtest stack under the rule**. E2E ids
> are cited below for traceability, not as measurement (`SPEC.md` §0.2(3)). The unit and ci-guard
> suites ARE measured. The one E2E measurement quoted verbatim — the `sdk91` window probe of §3 — was
> taken BEFORE the rule landed; its own file is unchanged and so is the server SQL it probed, but it
> drives the SDK `claim()` path that 9ddc4bb rewrote, so it is a finding about the coordinator that
> has not been re-run. Per-file status is in the Evidence index legend (§7).

Every party a user interacts with — sender, receiver, the statechain entity (SE), the watchtower,
the Bitcoin indexer, the RGB proxy, operators — and, for each: what flows between them, what the
code **verifies** (with the file and the test that proves it), what is **trusted** and why, and
the residual boundaries that no protocol change can remove. Normative requirements in
[SPEC.md](SPEC.md), [PROTOCOL.md](PROTOCOL.md) (the TES-R ladder), [CHILDREN.md](CHILDREN.md)
(first-class split children), [LIGHTNING.md](LIGHTNING.md) (the HODL latch),
[PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md).

The one-line summary: **nothing here asks the user to trust a counterparty. The trust that
remains is confined to (a) one well-known statechain assumption about the SE, (b) the user's own
view of the Bitcoin chain, and (c) someone being awake within the CSV window once a coin's funding
output is spent — and (c) is delegable without custody, keyless, and is the WHOLE of the
availability duty: a coin has no calendar clock for the owner's own wallet to defend (§5, B4).**
One gap does not fit that summary and is stated in full in §3: between conveyance and claim, the
server side is held by a **wall-clock one-hour timer**, not by ownership.

### One protocol, one coin shape, one clock — and what is still un-laddered

There is exactly **one protocol**. A coin's TES-R ladder — trigger `T` → extension `X_m` → state
`S`, relative CSV, un-broadcast — is established at the first mempool sighting of its funding
transaction, before confirmation, and it is the coin's only exit material; there is no per-deposit
protocol switch and no second shape. Under that one protocol:

- **LADDERED** — every plain deposit. Exit is the pre-signed tier chain. The coin never ages: no
  absolute locktime anywhere in its material, 0 vB of on-chain rent while idle, and renewal is
  off-chain (`sdk43`; `sdk40` PART 3; `sdk50` walks the exit to completion with no absolute-locktime
  backup in sight — `sdk40` and `sdk50` re-derived to first-sight establishment, pending run).
  `coin.locktime` is `None` for life. What bounds its off-chain life is the
  renewal/rollover capacity of the schedule and the SE's signature budget — both conveyed and
  counted by the census — never a date. *(The sentence that used to follow here — "the coin still
  carries one absolute height, `min(L_k)` over its flat backup chain" — is RETIRED 2026-09-06: there
  is no flat backup chain.)*
- **LADDERED, AND COLOURED** — an RGB **carrier** is never given a *plain* ladder (a plain tier spend
  would destroy the allocation — terminal-freeze, PROTOCOL.md §5.10, `sdk52`). It gets a COLOURED one
  instead, every tier carrying a valid RGB state transition, so laddering MOVES the allocation rather
  than destroying it (`sdk74` establish/renew/convey — re-derived to the flat-term-0 census, pending
  run; `sdk75` the on-chain unilateral walk). This is the default wherever an attestation identity is
  pinned, because `colored_ladder` reads that pin. An issuance books its carrier's allocation at
  broadcast, so the coloured ladder can be established over a still-unconfirmed `F`.
- **UN-BROADCAST FUNDING is a property, not a third shape.** A split sub-coin's funding output is
  un-broadcast, so it has no on-chain outpoint to root a trigger — permanently true, and unaffected by
  colouring. Such a coin is an in-ladder CHILD carrying its own `ctesr-` bundle, or a `spinetip-` tip;
  it is not routed down a separate lane.

**One hazard is CLOSED BY CONSTRUCTION, and it is worth stating rather than deleting.** The plain
off-chain split spent the coin's funding output `F` directly. A prior owner's retained trigger `T`
spends the same `F` and carries **no timelock**, so the two were rival spends of one outpoint decided
by first-seen and fee — the prior owner could void the split and destroy the payee's sub-coin, and the
payee had no way to detect the exposure before accepting the coin. That is the hazard the code tags
`[B1]` at its split sites (**not** this document's B1, which is SE + old-owner collusion and is
untouched by any of this). It used to be answered by a refusal inside `split_coin` and by
`ParentShape`'s dispatch choosing an in-ladder route for a laddered parent. It is now answered by
absence: `split_coin`, `ParentShape::Unladdered`, `ManyRoute::PlainSplit` and `ensure_exact_coin`'s
minting fallback are deleted, so there is no call site left that can pose it. Every split is a STATE
tier `SP` over `X_m.out[0]` — a **descendant** of `T`, never a rival for `F`.

**A second hazard is REMOVED (2026-09-06), and it is the reason the flat backup went.** Until then
every past owner of a coin retained a flat absolute-locktime spend of `F` — their own `tx_k` — that
MATURED on a date. That gave the coin a 10,000-block / 100-hop calendar its current owner had to
watch, made `min(L_k)` over prior owners' backups a clock no receiver could read, and on the coloured
lane let a past owner burn the carrier's allocation after the date. None of that material exists
any more. The only spends of `F` in a past owner's hands are the retained trigger `T` — no timelock,
and the SAME transaction the current owner holds, so broadcasting it starts a race the current
owner's (or their watcher's) strictly-lowest-CSV state wins — and their superseded states, which
lose the CSV race by construction. A past owner can force the coin on chain; without the SE's
retained share (B1) they cannot take it, and nothing they hold ever ripens.

**What is still un-laddered — a fault to repair, not a lane.** Four populations can be found without
a ladder. None of them can be conveyed, and none can be unilaterally exited, until they get one;
what each of them still has is the **cooperative** route — `withdraw::execute` reads no backup rows
and no ladder, so a confirmed coin can be withdrawn with the SE's co-signature (the sender's own
refusal says so: "The coin is unaffected and still withdrawable cooperatively"):

- a carrier the coloured builder could not take *this pass* (`LadderSkipReason::RgbCarrier`;
  retried at the next `claim()`);
- a carrier over an outpoint that was already plain-laddered as a deposit
  (`PlainLadderOverCarrier`: exitable as satoshis only, must not be conveyed as a carrier, and with
  **no remedy** — `colored_reanchor` refuses a plain ladder by name and a plain `refresh` would burn
  the allocation, so it is STRANDED);
- a carrier no coloured ladder can ever be built for (the sub-floor pre-flip pieces).
  `tokens::migration_hatch_verdict` still classifies these read-only, per coin, under the lock that
  would otherwise build the ladder — but the lane it opened onto, the legacy RGB-aware split/combine,
  now ends at `register_split_subcoins_n` / `register_combine_subcoins`, which refuse. Such a carrier
  has no exit and no conveyance until a later pass can colour it;
- every SDK deposit on a network with no pinned enclave identity (`AttestationIdentityUnpinned`) —
  and here the coin is **booked** (`LadderAtSight::Defer` books the chain's status before the
  establish pass runs), not withheld, so it sits in the wallet as a balance with no exit material.
  This is the population where the removal cost something that used to work: the flat backup gave
  such a coin a unilateral exit with NO attestation at all, and there is no longer any.

Every "flat lane" / "branch lane" statement below is RETIRED in place; none describes a route a
wallet can take.

Read §2 with that in mind: the **R′ census** (`verify_bundle_bound` / `verify_child_bundle` in
`clients/libs/rust/src/tesr.rs`) is the receiver's whole anti-sender check. It proves the
handed-over state carries the strictly-lowest CSV and that every superseded state was disclosed and
provably out-raced; the backup-chain checks that used to sit beside it (R4's flat arm, R5's flat
arm) have nothing to measure.

---

## 1. The parties and what flows between them

```
                       co-sign requests (blind)              chain reads,
        ┌──────────────────────────────────────► SE          broadcasts
        │                                        ▲ │              ▲
        │              key handover,             │ │ terminal     │
        │              ladder (root or child),   │ ▼ queries      │
   SENDER ────────────────────────────────► RECEIVER ────────► BITCOIN
        │              consignment (RGB)         │            (via indexer)
        │                                        │                ▲
        │                                        │ watch bundle   │
        └── stale states / a hostile trigger ──►chain  └──────────► WATCHTOWER(s)
            (the attack)                                      (keyless)
```

- **Sender → Receiver** (via the SE's message relay): the transfer message — key-handover
  material (`t1`) plus the **TES-R bundle**: shape 2, a root ladder, or shape 4, a split child (with
  its ancestor segment and every superseded state the census must count). Those are the only two
  admissible shapes (`admissible_shape`, `SHAPE_ROOT_LADDER`, `SHAPE_CHILD` in
  `clients/libs/rust/src/transfer_receiver.rs`); the un-laddered shape 0 no longer exists and cannot
  be received. And (tokens) the RGB consignment. **No backup-tx chain, no exit branch and no
  terminal-ancestor list travel with a coin any more** — a message carrying any of them is refused
  by name (R5, R6).
- **User ↔ SE**: blind MuSig2 co-signing (`sign_first`/`sign_second`), key-share rotation on
  transfer, spend-budget/terminal state, deposit init, the encrypted message relay.
- **User ↔ Bitcoin (via an electrum indexer)**: tip height, tx/outpoint lookups, history,
  broadcasts.
- **User → Watchtower**: a keyless bundle — the `TesrBundle` (tier chain) for one laddered coin, or
  the `WatchBundle` from `export_watch_bundle`, whose entries are all EVENT-driven (a trigger on the
  coin's funding outpoint, `deadline_block: u32::MAX`, no backup tx) — root ladders and split leaves
  alike. Pre-signed exit material only, in both cases (see §5).
- **User ↔ RGB proxy**: consignment upload/download (token transfers only).
- **User ↔ operators** (optional): deposit-token server (onboarding), refresh sponsor (fee
  rebates), SSP (Lightning swaps).

---

## 2. The receiver: what is verified on receive (the heart of the model)

A receiver **verifies, and does not trust,** the sender — and verifies most of what the SE says.
On every claim (`clients/libs/rust/src/transfer_receiver.rs` — client-side, running on the
receiver's machine — and there is no kind of claim that does not run the R′ census in
`clients/libs/rust/src/tesr.rs`: `verify_bundle_bound` for a whole coin, `verify_child_bundle` for a
split child):

| # | Check | Defeats | Code | Proven by |
|---|---|---|---|---|
| R1 | Sender's Schnorr signature binding the coin's outpoint to the receiver's new pubkey (`tx0_txid ‖ vout ‖ new_user_pubkey`) | handover messages not authorized by the coin's owner | `verify_transfer_signature` (`lib/src/transfer/receiver.rs`), over the funding outpoint the receiver read FROM THE CHAIN, never the message's copy | every successful claim (`sdk01` et al.); reject paths in `unit::transfer_signature_tests` (replay-to-other-receiver, wrong-outpoint, forged-by-non-owner) |
| R2 | The receiver's NEW share + new server share combine to the coin's on-chain aggregate pubkey (else `IncorrectAggregatedPublicKey`); sender-key/t1 supporting checks | SE or sender handing over key material that doesn't control the coin | `get_new_key_info` (`lib/src/transfer/receiver.rs`); `validate_tx0_output_pubkey`, `validate_t1pub` | every claim (`sdk01` et al.) |
| R3 | Funding tx0 output pays the expected aggregate (`validate_tx0_output_pubkey`); outpoint **unspent**. Confirmation is NOT a reject: a ladder over a funding output still in the mempool is a good coin (its exit is signed and needs no confirmation), so the claim completes and the coin is booked with the chain's status (`IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED`) and walked to `CONFIRMED` by `coin_status` like any deposit. *(The stricter rule for exit-branch roots is RETIRED 2026-09-06 with the branch lane — R6.)* | fake or spent funding | `verify_tx0_output_is_unspent_and_confirmed` (`clients/libs/rust/src/transfer_receiver.rs`) — the UNSPENT half is the reject; the status half is booked, not judged | every claim (`sdk01` et al. — all over CONFIRMED funding); **no E2E asserts the mempool-`F` admission on the receive path, and the ORDINARY SEND cannot reach it**: `execute_ex`'s own coin filter (`clients/libs/rust/src/transfer_sender.rs`) admits only `CONFIRMED` or `IN_TRANSFER`, so a wallet cannot convey a coin whose `F` is still in the mempool (`tb01` records that refusal by name). The admission is therefore reachable only when `F` loses its confirmations between send and claim — i.e. a reorg — and that path is UNPROVEN. (`sdk48` ladders a mempool deposit; nothing conveys one.) The SSP's pre-pay census (`prepay_flat_census`, reached from `peek_pending_transfers`) is deliberately stricter and still REQUIRES `CONFIRMED` before it authorises an irreversible Lightning leg |
| R4 | **R′ census**: the handed-over state carries the **strictly-lowest CSV**, no hidden lower state exists, and every superseded state is disclosed and provably out-raced. *(The flat arm — backup-chain locktime in `(tip, tip + initlock]`, `LocktimeTooLow` / `LocktimeTooHigh` — is RETIRED 2026-09-06: there is no backup chain to window. The census is the whole check.)* | being handed an **already-raceable** coin, or a ladder with a retained lower-CSV state | `verify_bundle_bound` / `verify_child_bundle` (`clients/libs/rust/src/tesr.rs`) | `sdk46` (the census against the REAL SE count at first mempool sight: a hidden extra signature, an undercount and a flat term of 1 are each rejected — re-derived, pending run); `sdk47` (a pre-established ladder carried across a transfer — the R′ count equation is the only equality the receiver runs; the sentence that used to sit here about "the decrementing-ladder structural check still running on both paths" is retired with the chain. Its own file WAS re-derived in 9ddc4bb; pending run); `sdk54` (re-derived to the flat term 0 — its control and every adversarial variant now call `verify_bundle(.., 0)`; pending run); `sdk58` (11 adversarial child-bundle cases REJECT — aggregates, hidden state, Model-A, parent terminality, child-superseded race, count padding, value spoof — re-derived, pending run) |
| R5 | **Census count**: `se_num_sigs == tiers + superseded`, exact equality, with the flat term ZERO **by construction** — not "zero because the vector was empty", but zero because no flat backup is ever co-signed for a laddered coin, at deposit or at any hop. The receiver enforces that: `verify_flat_backup_lane` REFUSES any non-empty `backup_transactions` beside a ladder, `refuse_conveyed_flat_backups` refuses one on the child, tail, stub and spine-tip lanes, and `PARENT_V2_BASELINE = 0` / `CHILD_V2_BASELINE = 0` are the constants the child census starts from. Each off-chain hop costs exactly **one** co-signature and discloses exactly **one** superseded state. The child term is `child_flat_backups + cb.child_rungs().len() + superseded`, and the tier count is DERIVED from the bundle's own shape, never a literal 2 (REQ-83): a two-rung piece at depth 1 balances at `0 + 2 + 1`, a one-rung thin piece or spine tip at `0 + 1 + 1`. The enclave count after a deposit is 3. **The count is not taken on the coordinator's word**: `get_statechain_info` sends a fresh random 32-byte nonce and refuses any answer that does not carry a `utexo/sig_count/v2` Schnorr signature over (`statechain_id`, `num_sigs`, budget-presence, `sig_budget`, nonce) verified against the **PINNED enclave attestation identity** — one long-term key per enclave, compiled in or configured, resolved pin → config → REFUSE. The coin's chain-anchored `enclave_public_key` cannot serve here: a depth-≥2 in-ladder-split ancestor's funding output is deliberately un-broadcast, so it has no chain anchor (B11). A half-stated or absent budget is refused, not defaulted | hidden intermediate owners; a sender keeping extra co-signed states; a sender padding a conveyed vector to absorb one; **a coordinator under-reporting `num_sigs` by k, which hides k co-signed rival states while the exact-equality census still balances** | `verify_bundle_bound` called with `0` as the flat term (`clients/libs/rust/src/transfer_receiver.rs`); `verify_flat_backup_lane`, `refuse_conveyed_flat_backups`, `PARENT_V2_BASELINE`, `CHILD_V2_BASELINE` (`clients/libs/rust/src/tesr.rs`); `get_statechain_info` (`clients/libs/rust/src/utils.rs`) + `verify_sig_count_attestation` (`lib/src/transfer/receiver.rs`); census counts in `verify_child_bundle`. *(RETIRED 2026-09-06 from the receive path: the flat arm `statechain_info.num_sigs != backup_transactions.len()` and `ladder_decrements_by_interval` in `validate_signature_scheme`, `lib/src/transfer/receiver.rs`. That code survives in `mercurylib` only behind the non-Rust bindings' FFI (`ffi_validate_signature_scheme`), and those bindings cannot receive shape 2 or 4 at all — `SPEC.md` §0.4 V-2.)* | every claim; count-padding rejected in `sdk58`; the per-hop census arithmetic across two hops in `sdk60` (+ `sdk17`, partial second hop) — those three re-derived, pending run; a conveyed flat backup refused on both lanes — unit `an_empty_vector_passes_on_both_lanes`, `plain_backups_are_refused_on_both_lanes_and_the_refusal_names_the_lane`, `rgb_material_on_a_flat_backup_is_refused_like_any_other_flat_backup`, `an_unparseable_backup_is_refused_without_being_read` (`clients/libs/rust/src/tesr.rs`); the flat-term-0 control and its attacks: `sdk54`, `sdk70` (both re-derived to the flat term 0 — `sdk70`'s CONTROL 0 now pins that the retired flat term of 1 is REFUSED; pending run); `unit::transfer_signature_tests::ladder_interval_check_rejects_wrong_and_increasing` still runs but exercises the retired lib arm |
| R6 | **RETIRED 2026-09-06 — branch validation.** There is no exit branch to validate: `refuse_branch_material` refuses any `branch_txs` / `terminal_parents` beside a ladder, and `register_split_subcoins_n` / `register_combine_subcoins` refuse to mint a branch-funded sub-coin in the first place. Value conservation per hop (INV-25) is the ladder's own check, inside `verify_child_bundle`'s value chain | fabricated exit branches (no longer expressible) | `refuse_branch_material` (`clients/libs/rust/src/transfer_receiver.rs`) | `sdk58` (value spoof reject — re-derived, pending run) |
| R7 | **Terminal ancestors** — the ladder lane does not ask the coordinator: `attested_terminal` (`clients/libs/rust/src/tesr.rs`) derives terminality from the enclave-signed payload (budget present **and** `num_sigs ≥ budget`), for the parent and every intermediate segment, and keeps the coordinator's answer only as a cross-check that refuses on disagreement. *(RETIRED 2026-09-06 with the branch lane: the per-structural-input named-ancestor rule and its unattested `terminal` flag read from `GET /statechain/spend_budget`.)* | sender double-spending a split parent via a fresh SE co-signature | `attested_terminal`, `verify_child_bundle` (`clients/libs/rust/src/tesr.rs`) | `sdk58` (parent-terminality reject), `sdk60` — both re-derived, pending run |
| R8 | **RGB consignment** fully client-validated; amount booked = what the consignment assigns to the receiver's outpoint, under the cryptographically-derived contract id | token forgery, wrong-asset or wrong-amount claims — no proxy or issuer is trusted for token *rules or amounts* (chain anchoring resolves through the wallet's indexer and inherits §4/B3) | `accept_incoming_tokens` (REQ-21/22) | `sdk02` (file unchanged), `rgb13` (re-derived, pending run) |
| R9 | **Received split child: the key handover COMPLETES.** The claim rotates the SE share and the auth key, and `A_child` is **invariant** across the rotation (proved by passing the un-broadcast `SP` as the funding tx to `get_new_key_info`), so every pre-signed child tier stays valid and the **sender is permanently locked out**. The child is then first-class: payable onward whole (`child_retransfer`) or split (`child_in_ladder_pay`, a depth-2 `ancestors` chain) | a sender re-spending or re-transferring a child he has already paid away; a "received payment" that is only exit-able | `clients/libs/rust/src/transfer_receiver.rs` (the child claim path); `verify_child_bundle` (`tesr.rs`, child terminality deliberately NOT required — the handover, not a freeze, is what makes the census durable); the coordinator's pending-transfer lock (`locked = true` on `statechain_transfer`, `server/src/database/transfer_sender.rs`) covers the census→completion gap — **but only for as long as that lock's window stays open; see §3, "The conveyance window"** | `sdk60` (alice→bob→carol, the funding outpoint unspent throughout), `sdk17` (partial second hop) — both re-derived, pending run |

*Parameter provenance:* `initlock` and `interval` still arrive from the SE's `GET /info/config` at
claim time, but they are no longer parameters of any anti-sender check: `info_config`
(`clients/libs/rust/src/utils.rs`) compares them against the compiled-in
`TesrParams::flat_ladder_params` table and REFUSES on disagreement, `initlock` is used only as the
fixed exit window the exit-chain-length cap and the split-depth cap measure a walk against
(`enforce_exit_chain_length`, `enforce_split_depth_cap`), and `interval` is applied to nothing.
The fee-sanity baseline comes from the indexer's `estimate_fee` and is covered by the §4 trust
item, not independently verified. The **ladder's** CSV schedule is *not* SE-served: the
decrement/floor/`m_max` cadence is the canonical, per-network `TesrParams` compiled into the client
(`lib/src/tesr.rs`, PROTOCOL.md §5.2), and a conveyed bundle whose declared schedule contradicts the
receiver's preset is refused (`cap_schedule`), so a hostile SE or sender cannot widen or narrow a
coin's race window. `sdk44` drives a whole lifecycle — establish → renew to the budget → roll over →
exit — off that schedule alone (re-derived to first-sight establishment, pending run).

What the receiver **cannot** verify (the honest list):

- **R-a. Ancestor-id substitution (blind-SE caveat, SPEC §14) — RETIRED 2026-09-06.** This was a gap
  of the flat BRANCH lane only: the terminal-ancestor *ids* conveyed with an exit branch were not
  cryptographically bound to the branch's outpoints, and the Σ-inputs count check defeated
  *omission* but not *substitution*. The lane is gone (R6). **The ladder never had this gap**:
  `verify_child_bundle` never trusts a supplied id. It derives `A_parent` from the *fetched on-chain*
  `F.spk` and requires the SE's recorded aggregate for the claimed parent sid to equal it (and
  `UNIQUE(aggregate_xonly)` means only the real parent can), then walks each intermediate segment
  deriving its aggregate from the funding output it actually spends. A substituted id fails on the
  key, not on a name (`tesr.rs`, checks [1]/[2]/[4b]; the decoy-parent and Model-A variants are among
  `sdk58`'s 11 rejects).
- **R-b. SE share deletion** — see §3.

**The sender needs no trust in the receiver**: a transfer is a one-way handover; until the
receiver completes the claim the sender can effectively cancel by **re-sending the coin**
(overwriting the pending relay message — proven, including its impossibility *after* the claim,
by `tm01`; the SSP's latch-gated receive additionally has an explicit abort, `sdk24`), and after
the claim the coin is simply no longer the sender's. (Payment-for-goods atomicity is out of
protocol scope — for atomic counterparty swaps use the Lightning latch (`tb04`) or invoices.)

---

## 3. The SE: blind co-signer — what it can, cannot, and must be trusted for

The SE holds **one share of a 2-of-2** MuSig2 key per coin and co-signs blindly: it receives a
session commitment, never the transaction — no amounts, outpoints, or destinations. Blindness covers
*content*, not *traffic*: the SE does learn statechain ids, per-coin auth pubkeys, deposit-token ids,
`single_use`/`epoch_deadline`/spend-budget flags, signature counts, transfer timing (relay
polling), and the caller's network endpoint — a timing/velocity graph of the system, and it can
correlate deposits to exits by amount at the chain boundary. It can also censor its message
relay (delaying claims; the coin stays the sender's until re-sent or exited).

Physically the "SE" is an API server + database (the **coordinator**) plus **enclave(s)** (lockbox)
holding the key shares, brokered per co-sign. Two facts about that split must be stated plainly:

- **In production the lockbox and the coordinator are run by the SAME OPERATOR.** The separation is
  a software boundary inside one administrative domain, not two parties. Any argument of the form
  "the coordinator cannot do X because the enclave would have to agree" is an argument about
  software, not about incentives.
- **The SE has no trustworthy chain access.** It is not incapable of reaching the network — the
  lockbox already makes outbound HTTPS through `cpr` (`lockbox/src/hashicorp_api_key_manager.cpp`)
  — but it runs in an operator-controlled container on an operator-controlled network, so whatever
  endpoint it queried is operator-chosen. "The SE checked the chain" reduces to "the operator says
  so." No design here may rest on the SE verifying an on-chain fact. That is also why the ladder
  can be signed at first mempool sighting: the coordinator's sign endpoints never look at the chain,
  and the trigger needs nothing but the funding outpoint, its value and the aggregate key.

Be precise about what is and is not attested: there is **no enclave-residency attestation** (no
quote a client checks), so "the share lives in an enclave" remains an operational claim the user
trusts, not verifies — but the enclave key does **sign the numbers the census rests on**
(`utexo/sig_count/v2` over the statechain id, `num_sigs`, the spend budget and a client-chosen
nonce), and the client refuses an unattested or half-stated answer outright rather than recording it
(R5). Authenticity of the count, not residency of the share. Discovery note: SEs advertise their
URL/terms via signed nostr events (server `NostrInfo`); wallets take the SE URL from config —
verify it out-of-band, the relay is not a trust anchor.

### ⚠️ The conveyance window: server-side, a conveyed-but-unclaimed coin is held by a one-hour wall clock

Between the moment a payer conveys a coin and the moment the payee claims it, the payer still holds
a valid credential for that coin (`signed_statechain_id`, written at deposit and not rotated until
the claim completes). What stops the payer from obtaining a fresh co-signing session over the coin
they already paid away is, on the server side, exactly one thing: the open-transfer gate
`has_open_transfer` / `OPEN_TRANSFER_WINDOW_SQL` (`server/src/database/transfer_sender.rs`), whose
non-batch branch is a hard-coded `updated_at > NOW() - INTERVAL '1 hour'`.

**Measured, on a live regtest stack, by `SDK_E2E=91` (`clients/tests/rust/src/sdk91_malicious_payer_window.rs`) — the run PREDATES 2026-09-06; the probe file and `OPEN_TRANSFER_WINDOW_SQL` are both unchanged since, but the deposit it rides on now goes through the rewritten SDK `claim()` path, so this has not been re-run under the rule:**

```
[a] raw /sign/first INSIDE  the window -> HTTP 409  "coin has an open transfer …"
[b] transfer row aged 2h past updated_at (1 row)
[c] raw /sign/first OUTSIDE the window -> HTTP 200  {"server_pubnonce":"0x02ec1163…"}
```

Nothing is forged in that probe: the payer POSTs `/sign/first` directly to the coordinator with
their own genuine credential, skipping their own client. So:

- The gate is real and fires correctly **while the window is open**.
- **Once the hour lapses, the coordinator issues the session** — the timer expires on wall-clock
  time, whether or not the payee has claimed.
- The client-side defences on this path — the wallet's own coin lookup and the sender-side
  outstanding-conveyance refusal (`refuse_outstanding_conveyance`, `clients/libs/rust/src/tesr.rs`,
  called from `transfer_sender.rs`, `in_ladder_split`, `renew` and the coloured builders) — are the
  **payer's own software**, and a payer who wants to cheat does not run them. `sdk90`
  (`clients/tests/rust/src/sdk90_transfer_window_lapse.rs`) measures those local gates and reaches
  no conclusion about the server.

**Scope — do not report this as more than it is.** A `sign/first` session is the first link of the
chain `has_open_transfer`'s own comment describes, not a completed theft: `sign/second` and a
broadcast race against the payee's strictly-lower-CSV state still stand between it and money moving.
In the measured run the payee claimed his 120,000-sat coin intact. The remaining links are **not
tested and must not be assumed in either direction**.

**Status of the fix.** SPEC REQ-61 specifies the **owner latch** —
`latch_key := xonly(state_child.vout[0].spk)`, read from the money itself, write-once, after which
every co-signature under that sid requires a fresh BIP-340 by that key — which binds co-signing to
ownership rather than to elapsed time. **It is DESIGN; it is not built.** `EXPECT_LATCH=1` is wired
into `sdk90` and `sdk91` and converts the recording into a hard assertion the day it ships. The
batch branch of the same SQL is not the one-hour rule: a latch batch stays open until its receiver
can no longer claim (`MAX(lightning_latch.expires_at)`, grace deliberately not subtracted), a plain
batch until `batch_time + batch_timeout`. The window SQL has **no behavioural test** — there is no
test database or embedded Postgres in this repo, so `open_window_invariant_tests` models the window
arithmetic in Rust and asserts the SQL carries that shape. That is weaker than executing it.

**Cannot do alone** (verified/structural):
- Steal: it never holds a full key, and every spend needs the owner's share (`sdk01`+every E2E).
- Forge your exit material after the fact: your tiers are already signed and in your hands, from
  the block your deposit was first seen in.
- Un-terminate a node: the budget may only TIGHTEN — `POST /statechain/spend_budget` lands in
  `set_sig_budget`, which writes `min(count_finalized + remaining, existing)` server-side
  (`server/src/database/deposit.rs`; the budget is an ABSOLUTE count on both sides, not a relative
  remainder; the pure predicate is unit-tested in
  `invalidation_model::terminal_predicate_matrix`); budget *exhaustion* refusal is E2E-proven by
  `sdk04` (an in-ladder split leaves the parent TERMINAL at the SE, and a second split over it is
  refused for that reason — the test pins the cause negatively so a plumbing error cannot make the
  refusal pass vacuously). Terminal state is publicly auditable
  (`GET /statechain/spend_budget/<id>`).
- Double-hand-over a coin **behind the receiver's back**: nonce-atomicity (one message per
  signing nonce — INV-23, `sdk12`) plus receiver-side count checks (R5). Note the honest
  counter-example: an SE *willing* to fresh-sign twice can create two conflicting states
  (`sdk15` documents this trust floor); what protects the receiver is R5 — the second "owner"
  cannot present a consistent ladder/count without the SE visibly double-signing.

**Trusted for (the irreducible statechain assumption):**
- **T-SE-1: deleting/overwriting the previous owner's key share at transfer.** If a malicious SE
  *keeps* old shares and **colludes with a previous owner**, together they can fresh-co-sign an
  immediate spend — no timelock protects against it, because a fresh signature needs no backup.
  This is THE statechain trust unit, identical in Mercury and (as full-operator collusion) in
  Spark. *Why it cannot be verified*: any proof of erasure attests one instance of the data, and
  nothing prevents a copy having been made before the proof — verifying a negative over an
  adversary's storage is impossible from outside. Mitigations, not proofs: the enclave narrows
  "the operator kept it" to "the enclave leaked it" (but see above — no client-side attestation,
  and the same operator runs both), blindness means the SE cannot identify *which* coin to steal
  without the colluding old owner, and collusion leaves evidence the receiver can pull and check
  (terminal receipts, and an **enclave-signed** signature count — R5).
  **There is no race advantage to lean on here.** The collusive spend is a FRESH co-signature — no
  timelock — and the owner's only immediate answer is his own un-timelocked trigger `T` over the
  same `F`, so the two are symmetric conflicting spends decided by first-seen and fee, between an
  attacker who is by construction online and a defender who may not be. And no wallet bumps
  anything out of the box: `fee_bump` ships as `None` on **both** presets (`SdkConfig::regtest` /
  `::mainnet`), so bumping needs an owner-supplied fee source.
  **And since 2026-09-06 it is the ONLY route by which a past owner can take a coin.** The other
  route — a past owner's own retained, MATURING flat backup of `F`, which needed no SE at all once
  its date passed — no longer exists; what a past owner holds is the shared trigger (a griefing
  tool, never a capture) and superseded states that lose the CSV race. The deletion of the old share
  is therefore the whole of the past-owner trust, not one half of it.
  *Why not split the SE across N operators* (Spark's honest-1-of-n deletion)? It shrinks B1 only if
  ≥1 operator is honest, at the price of N-way liveness for every co-sign (any operator down
  freezes all cooperative paths) and an N-fold blindness/collusion surface; this design keeps one
  blind SE and spends the complexity budget on receiver-side verification and SE-free exits
  instead.
- **T-SE-2: liveness** — refusal to co-sign freezes only the *cooperative* paths. Unilateral exit
  is pre-signed and SE-independent — the SDK walks the TES-R chain (trigger → extension → state) as
  each relative CSV matures with no SE call at all (`sdk50`; `sdk40` PART 1 drives the same chain at
  the library level, and `sdk45` shows a **keyless** third party can drive it), and an in-ladder
  split child walks its own pre-signed chain `T → X_m → SP → ext_child → state_child`
  (`exit_child_pass`) — so freeze ≠ seize; worst case your coin becomes an on-chain exit ticket with
  a bounded wait. One true boundary: the **onboarding window** — the ladder's three co-signatures
  land when the wallet first SEES the funding tx in the mempool (`check_deposit`,
  `clients/libs/rust/src/coin_status.rs`; the SDK's `claim()` pass for `LadderAtSight::Defer`), so
  the window runs from funding broadcast until that pass; an SE that dies inside it strands the
  funding in the 2-of-2. A deposit whose ladder cannot be established is NOT booked under
  `LadderAtSight::Plain` — it stays `INITIALISED` and is retried, because a visible deposit with no
  ladder is a coin with no exit and the wallet refuses to call it a coin. Under the SDK's
  `LadderAtSight::Defer` the coin IS booked with the chain's status **whether or not the establish
  pass that follows succeeds**; one that pass could not ladder is recorded with a skip reason
  (`LadderSkipReason`, `clients/libs/rust-sdk/src/events.rs`), retried on the next pass, and until
  then refused by name by the sender (`execute_ex`) and by `unilateral_exit`. Such a coin has no
  exit material of its own; its only route out is the COOPERATIVE withdrawal, which needs the SE —
  so on this lane an SE that dies after the deposit is booked but before it is laddered strands the
  value exactly as one that dies before the ladder does. Fund only after
  deposit init succeeds, and treat a deposit that stays `INITIALISED`, or one the SDK reports
  skipped, after it is visible in the mempool as a reason to stop funding further coins.

---

## 4. Bitcoin, seen through an indexer: the user's own eyes

Everything above assumes the user can *see the chain and reach it*. That view goes through an
electrum-protocol indexer (`electrum_url`), and it is a genuine trust point — **the same SPV-level
assumption as every light wallet**, made explicit:

- **Trusted for**: tip height (drives every CSV-maturity read in an exit walk — `exit_pass`,
  `watch_pass` — and the confirmation count; the exit window the depth and chain-length caps measure
  against is the compiled-in `initlock`, not a chain read), outpoint spent/unspent status (R3; and the EVENT every defence is triggered by — `F`
  being spent), first mempool sighting of a deposit (the moment its ladder is signed), broadcast
  delivery, and **fee-rate estimation** (`estimate_fee` drives the CPFP bump rate where a fee source
  exists and the sender-side fee sanity baseline; `max_fee_rate` caps only the over-payment
  direction).
- **A lying indexer could**: under-report the tip so a matured tier looks immature (stalling your
  walk, or your defence, while a rival's tier confirms elsewhere), hide the hostile spend of `F`
  that the reactive clock starts on (a defence cannot react to an event it cannot see), report a
  spent `F` as unspent (making a receiver accept a dead ladder — R3 passes on false data), never
  show you your own deposit (delaying the ladder's establishment — under `LadderAtSight::Plain` the
  coin is not booked until it is laddered, so nothing is exposed there; under the SDK's `Defer` the
  coin IS booked at first sight and only the establish pass is delayed, so the wallet can hold a
  booked coin with no exit material — visible as a `LadderSkipReason`, not silent), under-report the
  fee rate so a tier under-bids the mempool, or silently drop your broadcasts.
  It could not steal by itself — it can only blind and delay you; funds move only via signatures
  it doesn't have.
- **Fail-closed behaviors in code**: a claim admits a mempool `F` but refuses a SPENT one
  (`verify_tx0_output_is_unspent_and_confirmed`, `clients/libs/rust/src/transfer_receiver.rs`); a
  deposit that is visible but cannot be laddered is left `INITIALISED` rather than booked
  (`check_deposit` under `LadderAtSight::Plain`) or, under the SDK's `Defer`, booked and recorded
  with a skip reason that the sender and the exit refuse by name; `defend_ladders` separates a coin whose `F` was spent by something other than
  its own trigger (a permanent loss of the tiers below, reported as `lost`) from one it merely could
  not read (`blind`), and the keyless `watch_pass` reports an unreadable trigger outpoint as
  blindness for that entry rather than averaging it into a quiet pass; token-wallet balance fails
  closed when RGB state is unavailable.
- **Finality (reorgs)**: receiver acceptance is final modulo a reorg deeper than
  `confirmation_target` (regtest default 2, `SdkConfig::mainnet` default 3) — a deeper reorg
  undoes the very facts R3 checked. Raise the target for large values, and size the CSV schedule's
  floor with reorg slack in mind (B4).
- **Transport**: the dev defaults are plaintext (`http://` SE, `tcp://` electrum, `rpc://` RGB
  proxy). In production every channel (SE, electrum, RGB proxy, SSP) must run TLS/`ssl://`: an
  on-path attacker over plaintext is strictly stronger than a lying indexer — it can tamper with
  unauthenticated responses (e.g. `info/config`'s fee rate; its `initlock`/`interval` are now
  cross-checked against compiled-in constants and a mismatch is refused, so those two are no longer
  a lever), observe all metadata, and censor. (Transfer messages themselves are end-to-end
  ECIES-encrypted to the receiver's auth key regardless.) A `tor_proxy` option exists for SE HTTP
  calls only — the electrum connection is direct; run your own node for network-level privacy.
- **The remedy is architectural, not protocol**: run your own node + indexer (the regtest stack
  ships one; mainnet deployments should point `electrum_url` at their own electrs), and/or run
  multiple independent watchtowers on *different* indexer connections (§5) so one lying indexer
  cannot blind them all. Broadcasts can additionally go through any out-of-band path (a public
  broadcaster, a second node) — the pre-signed txs are portable.

**Bitcoin itself** is trusted for liveness and fee markets: each tier of a walk must confirm inside
the CSV race it is in, so sustained full-block congestion compresses your margin — the schedule's
floor and CPFP top-ups (where a fee source exists — `fee_bump: None` on both presets, B4) absorb
this; choose the schedule accordingly.

---

## 5. The watchtower: delegation without custody

*Can the user run their own watchtower? Multiple? Do we trust it?* — Yes, yes, and **no trust
needed for custody**, by construction:

- **In-process by default.** The wallet's own background task (`start_background`) claims
  incoming transfers and then runs its defences. Since 2026-09-06 only ONE of them has a subject:
  - `defend_ladders()` — one `watch_pass` per adopted `tesr-` bundle plus one `watch_child_pass`
    per adopted `ctesr-` split child and one `watch_spine_tip_pass` per `spinetip-` tip, a no-op
    until someone broadcasts a trigger. It **is** wired into `start_background`, unconditionally,
    gated to one pass per new block (a relative CSV can only mature on a block), and its liveness
    allowlist is `is_live_for_defence` — a ladder is defended from the block its deposit is first
    seen in, IN_MEMPOOL included. This is the reactive clock, and on a laddered coin it is the only
    clock.
  - `deadline_safety_due` (which calls `auto_refresh_due` first, then severs from `F` through
    `sever_from_f` for whatever the cooperative route could not save) still runs from
    `maintenance_plan` on every tick, at `auto_refresh_margin_blocks` = 144 — and **has no laddered
    subject**: its due-predicate reads `coin.locktime`, which is `None` for life, so no coin is ever
    "due". The whole-coin `min(L_k)` clock it was written to defend does not exist. *(RETIRED
    2026-09-06 as a defence; kept in the loop as an inert pass. Its own doc comments still describe
    the retired clock.)*
  - `auto_exit_due` (`auto_exit` default-on) at `auto_exit_margin_blocks`, still **derived** as
    `k_max·interval + tesr_exit_txs(1)·144` = **2,120 blocks on mainnet** and **860 on regtest**
    (`auto_exit_margin_blocks_for` in `clients/libs/rust-sdk/src/config.rs`) — a number that now
    measures nothing on any coin the code can mint. The pass reads `branch-` rows, and both
    producers of those rows refuse (`register_split_subcoins_n`, `register_combine_subcoins`); its
    leaf near-deadline loop is DELETED, because a split leaf's only exposure is the parent's trigger
    being broadcast — an event `defend_ladders`' child/tip loops already answer. *(RETIRED 2026-09-06
    as a defence for coins minted under the rule; it still services `branch-` rows from before it.)*

  Routine *background* re-anchoring stays **off** by default (`background_auto_refresh = false`):
  paying rent on an idle coin is an economics choice, and with no calendar there is no safety half
  hiding behind that flag any more — a running wallet never silently shrinks a balance, and nothing
  on a coin needs re-anchoring by a date. The `auto_refresh` pre-spend hook stays default-on and is
  likewise inert on a laddered coin. This is not a third party — it's your own process; "trusting
  the watchtower" here means trusting your own machine to be on **while you hold off-chain coins**
  (a wallet that is entirely offline when someone spends its `F` is B4's case — delegate, or exit
  before going dark).
- **Keyless delegation** (`sdk45`): everything a watchtower must broadcast is *already
  fully signed* and *pays only the owner* — for root ladders and split leaves alike.
  - *Laddered root*: the persisted `TesrBundle` (`tesr::persist` / `tesr::load`) is the tier chain,
    every tier paying the owner's own key. `tesr::watch_pass(cc, bundle)` runs one iteration from
    that bundle and an electrum connection alone — no wallet, no coin, no SE, no keys. `sdk45`
    serializes the bundle a user would hand a third party and asserts it contains none of
    `mnemonic`/`seckey`/`secret`/`private`/`privkey`/`xpriv`, then has the keyless tower defend an
    offline owner against a **hostile trigger** end-to-end. `sdk51` is the same defence run by the
    owner's own pass.
  - *The `WatchBundle`* (`export_watch_bundle`, `clients/libs/rust-sdk/src/watchtower.rs`) covers
    every live coin in one document: a laddered root becomes an entry with a `trigger` on its own
    `F`, `deadline_block: u32::MAX`, `backup_tx: None`; a split child or spine tip becomes a
    `leaf_watch_entry` with a trigger on the PARENT's `F`, the leaf's bound exit chain as the push
    list, and again `deadline_block: u32::MAX`, `backup_tx: None`. **`deadline_block: u32::MAX` is
    the whole truth, leaves included**: it keeps the height predicate permanently false, so an entry
    is due on the event of the watched outpoint being spent and on nothing else. **No key-material
    fields exist on the bundle types at all** (unit-tested, `bundle_roundtrip_and_carrier_has_no_backup`),
    and an entry whose exit chain is present but unbuildable — an unreadable row, an empty chain, a
    blind exit-cost estimate — aborts the export rather than being dropped from it
    (`an_empty_chain_aborts_the_export_instead_of_dropping_the_coin`,
    `the_export_consults_the_leaf_rows_before_the_flat_coin_shortcut`). **The one coin that is
    dropped WITHOUT an error is the coin that has no exit chain to build from**: a live coin with no
    `tesr-`/`ctesr-`/`spinetip-` row and no `branch-` row takes the export's `continue` arm and is
    simply absent from the bundle. That is B12's population, and it is reported by `flat_only_coins`
    rather than by the export — so read the two together, not the bundle alone. `watch_pass(bundle,
    electrum, margin)` in the same file is the matching keyless pass; its height predicate is inert
    on every entry the code now exports. *(RETIRED 2026-09-06: the "flat residual (branch-funded)"
    entry shape — branch txs, a deadline height and, for plain coins, the latest backup tx. The
    export still carries that lane for pre-existing `branch-` rows; nothing can mint a new one.)*

  Hand either bundle to any machine, cron it anywhere. Two scope notes: `export_watch_bundle`
  covers every `is_live_for_defence` coin **that has exit material** — root ladders from their first
  mempool sighting, and every adopted leaf; a live coin with NO ladder is omitted (see above, B12) —
  and bundles of both kinds are **snapshots**: re-export after any
  operation that mints or replaces coins (transfer, claim, split, child re-transfer, **and
  refresh** — treat `WalletEvent::CoinRefreshed` as a re-export trigger).
- **The worst a malicious/buggy watchtower can do**: broadcast *early* — which settles the
  owner's coins on-chain **to the owner** (safe; costs only the off-chain-ness; on the ladder an
  early tower merely walks the owner's own tiers to the owner's key, `sdk45`; the coloured-lane
  case, bob keeping his sats and all 250 tokens, was `sdk34`, whose original premise — a carrier
  deadline and a retained deposit backup — is retired; the file has been re-derived to the
  event-driven defence and is pending run) — or *not act* (the identical risk as running no
  watchtower). It cannot
  redirect funds (it has no keys) and it cannot destroy tokens: no entry of any kind carries a backup
  tx — a laddered entry and a leaf entry export `backup_tx: None` by construction — and a carrier is
  never given a *plain* ladder in the first place (`sdk52`); a coloured ladder is walked by
  `defend_ladders`, whose every rung is RGB-aware.
- **Multiple watchtowers compose safely**: they all hold the same pre-signed transactions, so
  they can never conflict — a second tower's broadcast is an idempotent re-broadcast
  (`sdk45`, two independent towers). Redundancy is pure upside; diversity of *indexer
  connections* also hedges §4.
- **What remains trusted: availability.** *Someone* — your process, your cron, a third party, or
  several of them — must be awake **within the CSV window once someone spends a coin's `F`**. That
  is the whole duty. There is no calendar date on any coin, no deadline height in any bundle
  (`deadline_block: u32::MAX` everywhere), and no clock the owner's own running wallet defends that
  a keyless delegate cannot — which is why an *un-triggered* ladder costs nothing to watch and why,
  **for a coin that has a ladder**, the delegate's coverage is complete rather than partial. For a
  coin that has none there is nothing to delegate, because there is nothing pre-signed to broadcast
  (B12). *(RETIRED 2026-09-06: "inside the margin
  before a flat coin's absolute deadline" and "inside `auto_refresh_margin_blocks` of a laddered
  coin's own `min(L_k)`, which no keyless tower covers".)* That is the (c) in the summary; it is
  delegable and redundant but not removable (see §7-B4).
- Privacy note: a bundle reveals the watched coins' exit txs (amounts, addresses) to the
  watchtower — a privacy cost, never a custody one.

---

## 6. Token-specific parties (RGB)

- **RGB validity is client-validated** — the receiver's own wallet validates the full consignment
  history against the Bitcoin anchors (R8). No proxy, SE, or issuer is trusted for token *rules,
  amounts, or history*; the *anchoring* of that history to Bitcoin is resolved through the
  wallet's own electrum indexer and inherits the §4 trust point (B3). (`rgb12`/`rgb13` are the
  negative tests.)
- **The RGB proxy** relays consignments; it is trusted for *availability* only (a dead proxy
  delays token transfers; claims retry idempotently). It cannot forge (validation is local) and it
  cannot steal.
- **Issuers** control issuance policy, not your holdings: there is deliberately no freeze
  (client-validated assets have no enforcement point).

### What each party learns (privacy, not custody)

| Party | Learns | Mitigation |
|---|---|---|
| SE | statechain ids, auth pubkeys, sig counts, flags, transfer timing, your IP — not amounts/outpoints/destinations (blind) | `tor_proxy` (SE HTTP only); amount-correlation at the chain boundary remains |
| Indexer | every address/outpoint/tx your wallet ever queries — full coin linkage, live interest, your IP | run your own node/electrs |
| RGB proxy | consignment contents in transit (asset, amounts, history) | self-host the proxy |
| SSP | swap amounts, invoices, coin ids involved in swaps | choose/run your own SSP |
| Watchtower | the watched coins' exit txs (amounts, addresses) | split coins across towers; self-host |

---

## 7. Optional operators, and the boundaries that cannot be solved

**Optional operators** (all custody-free):
- **Deposit-token server** — *no relation to RGB tokens*: a "deposit token" is an **onboarding
  voucher**, the anti-spam + operator-revenue gate. Every new statechain slot consumes one, but
  slots come in two classes (REQ-35, `sdk36` — re-derived, pending run):
  - **Onboarding slots** — fresh on-chain value entering the SE (a deposit address, a token
    issuance carrier). These consume a normal token: free from the SE when no token server is
    configured (`server/src/endpoints/deposit.rs`, subject to the outstanding-token cap; on mainnet
    only with the explicit operator opt-in `free_tokens_on_mainnet = true`), priced by the token
    server otherwise. Rationale: a slot is a permanent SE liability (enclave share, DB, co-signing
    duty), and a *blind* SE has no other billing point — transfers are free and unmetered, so
    pay-once-per-slot is the statechain fee model.
  - **Derived slots** — outputs of SE-co-signed flows over an *existing* statechain (in-ladder
    split children and spine-batch pieces, `transfer_many` recipients, refresh re-anchors; the
    off-chain branch split/combine pieces this list used to name are retired lanes).
    These re-house value already inside the SE, adding no on-chain onboarding surface, so they
    are **free**: the SE mints derived tokens itself (`POST /deposit/get_derived_token`, any
    network, never routed to the token server), gated on the parent's CURRENT-owner auth (a
    single-use nonce) and a per-parent **lifetime** cap
    (`max_derived_tokens_per_statechain`, default 64; 0 disables derived issuance). Without
    this, a paid deployment would charge every 2-output split 2× the onboarding fee (shipped
    token-server default is 10,000 sats) and every (auto-)refresh 1×, for zero new on-chain
    surface. The SDK never spends pooled/prepaid onboarding tokens on a derived slot, and falls
    back to them only if the SE lacks or refuses the endpoint. *Residual (blind SE)*: the SE
    cannot see how a slot is later funded, so a dishonest owner can point a fresh L1 deposit at
    a derived slot and dodge the fee for that slot — bounded by the per-parent lifetime cap and
    the outstanding-token cap, eliminable only by unblinding deposits or disabling
    derived issuance (`max_derived_tokens_per_statechain = 0`).

  **Deployment strategy (decided)**: production runs FREE — onboarding unpriced, the
  outstanding-token cap as the standing spam brake — and token-server pricing is deferred until
  spam actually appears; the derived-slot exemption above is what makes enabling it economically
  sane later. With a token server configured, the wallet reaches it **through the SE** (never
  directly; honest relay of pricing is part of the §3 trust). Worst case = losing one prepaid
  onboarding fee; it never touches existing coins (`SdkError::TokenPaymentRequired` surfaces
  cost instead of silently paying).
- **Refresh sponsor** (`refresh_sponsored`): rebates the refresh fee off-chain *after* the
  re-anchor. A sponsor that stiffs you costs exactly `fee` sats (you keep the refreshed coin);
  the failure surfaces as an explicit error ("re-anchor succeeded but the sponsor rebate
  failed", `refresh.rs`; happy path `sdk30` (re-derived, pending run), **stiffing bounded-loss
  `sdk38`** — a broke
  sponsor errors while the user keeps the refreshed amount−fee coin). A sponsor paying from a
  laddered coin rebates via an **in-ladder split**, whose child must fund its own extension and
  state tier before clearing dust — `min_child_value` = `2·(committed_fee + P2A) + dust` =
  2·(375 + 240) + 330 = **1,560 sat** at the shipped 3 sat/vB. The sponsor rebates
  `max(fee_sats + DUST_LIMIT, min_child_value)` and absorbs the difference, so the user still ends
  ≥ whole.
- **SSP** (Lightning): swaps work in **both directions on the ladder** via the HODL latch and are
  preimage-atomic — exact-amount pay/receive `sdk63` (re-derived, pending run)/`sdk64`, non-exact
  (latched in-ladder split) `sdk65`/`sdk67`, failure/rollback `sdk66`/`sdk68`, adversarial
  `sdk19`/`sdk20`/`sdk24` (LIGHTNING.md). The SSP is trusted for liveness and quotes, not funds. The latch no longer
  requires a locktime on the coin — there is none to require. One structural note: a
  **latched piece is the one case that stays terminalized**. It is deliberately left unclaimed until
  a preimage lands, which is precisely the window the temporary pending-transfer lock does *not*
  cover (that lock expires with the batch window), so the SE is asked to co-sign nothing further
  over it — closing the post-expiry rival window permanently. Every other in-ladder payment relies
  on the pending lock plus the receiver's prompt handover (R9) instead — see §3's conveyance-window
  gap for what that lock is and is not — which is what keeps ordinary children re-transferable.
  Arbitrary-amount invoices are payable from a laddered coin through the latched in-ladder split
  (`sdk65`).

**Also on this list, without a number, because it has a specified fix that is not built:** the
conveyance window of §3 — a conveyed-but-unclaimed coin is protected server-side by a one-hour
wall-clock timer and by client-side gates the adversary chooses whether to run (`sdk91`).

### The honest list: boundaries that remain (numbered, with their mitigations)

| # | Boundary | Why it cannot be removed | Mitigation (not proof) |
|---|---|---|---|
| B1 | **SE share deletion / SE+old-owner collusion** (T-SE-1) | A blind 2-of-2 co-signer's memory cannot be proven erased from outside | enclave (lockbox); blindness (target selection needs the colluder); public audit trail, incl. the enclave-signed sig count (R5). **No race head start** — the collusive spend is un-timelocked and so is the owner's trigger, so this axis is a symmetric first-seen/fee race, and `fee_bump` ships as `None` (§3). Note that lockbox and coordinator are the same operator (§3). Since 2026-09-06 this is the ONLY spend of `F` a past owner can bring that ripens into a capture — the retained flat backup that used to mature beside it is gone (T-SE-1) |
| B2 | **RETIRED 2026-09-06 — Ancestor-id substitution** (R-a, SPEC §14). It was a gap of the flat BRANCH lane only, and that lane is deleted rather than hardened: `register_split_subcoins_n` / `register_combine_subcoins` refuse, `refuse_branch_material` refuses on receive. On the ladder the ancestor chain is key-derived from the on-chain funding and each segment's own funding output, so there is no id to substitute | — | laddered coins are structurally exempt (`verify_child_bundle` [1]/[2]/[4b], `sdk58`); nothing else exists |
| B3 | **Indexer honesty & liveness** (§4) | A light client's chain view is whatever its indexer serves | own node; multiple towers on distinct indexers; out-of-band broadcast |
| B4 | **Deadline liveness** — someone must act inside the margin, and a laddered coin has exactly **one** clock: the *reactive* one. Once someone broadcasts the trigger, a defender must race the tiers per block; until then nothing on the coin ages — no tier carries a calendar date, `coin.locktime` is `None`, and there is no absolute height held by anyone. *(RETIRED 2026-09-06: clock (ii), "the coin still sits on `min(L_k)`, the flat-backup height held by its PRIOR OWNERS, after which an ancestor's matured rung spends `F`". No such rung exists.)* | Timelock security is *defined* by acting before maturity (relative CSV on the ladder) | `defend_ladders()` per new block, unconditional, from the block the deposit is first seen in (`is_live_for_defence`) — **while the wallet process is alive**; the other two passes in the loop (`deadline_safety_due` at margin 144, `auto_exit_due` at the derived `auto_exit_margin_blocks`, **2,120 mainnet / 860 regtest**) have no subject on a coin minted under the rule (§5). Keyless delegation to N towers covers the *reactive* clock — and on a laddered coin that is the ONLY clock, so the delegate's coverage is complete: every exported entry, root or leaf, carries a trigger on the watched `F` and `deadline_block: u32::MAX` — and it stops at a fee spike: a keyless tower can broadcast the pre-signed tiers at their committed fee but **cannot fee-bump them** (a CPFP child needs an input it does not hold and a signature it cannot make), so above the relay floor the defence falls back to the OWNER being online, or to an operator running the optional funded-tower variant (`FeeBumpConfig`, `fee_bump: None` on both presets — fee bumping ships with no fee source). **The carrier lane, precisely:** carriers are excluded from the COOPERATIVE re-anchor route (a plain re-anchor spends the carrier's outpoint into a fresh aggregate and destroys the allocation; the coloured re-anchor is the manual remedy). They are INCLUDED in the unilateral route, because the forced action there is the coin's own pre-signed `T`, which carries its own state and does not re-aggregate. **But that route only reaches a COLOURED ladder** — `unilateral_exit` refuses a carrier whose ladder is not coloured, and it is right to: broadcasting an RGB-unaware tier burns the asset. `SdkConfig::colored_ladder` READS the network's compiled-in enclave attestation pin, so on a pinned network (regtest today) the carrier is coloured, `unilateral_exit` admits it and the defence exists; on a network with no enclave provisioned (mainnet, testnet, signet, per `TesrParams::attestation_identity_const`) there is no pin, and on the SDK lane — the only lane with an RGB engine, so the only lane a carrier reaches — the establish pass ladders NOTHING (`AttestationIdentityUnpinned`) and every carrier is refused. Lane-precise, because the two do not behave alike: `check_deposit` under `LadderAtSight::Plain` still builds a PLAIN ladder with no pin at all, which is why a carrier must never go down it (`PlainLadderOverCarrier`). This is a **provisioning** state, not a chosen default (`SPEC.md` §0.4 V-6), and one with NO fallback lane now (B12). A carrier the coloured builder cannot take has no automatic defence because it has no exit material at all — visible rather than silent in the passes themselves (they report every coin they could not defend and return `Err` rather than a clean `Ok`) and listed by `flat_only_coins`, but note `export_watch_bundle` OMITS such a coin without erroring (§5), so a delegated tower is not told about it. Routine background re-anchoring stays default-**off**; that flag is maintenance, and there is no safety half behind it any more |
| B5 | **Onboarding window** (T-SE-2 tail) | The ladder's three co-signatures land only when the wallet first sees the funding tx in the mempool, so the window (funding broadcast → `T`/`X_0`/`S_0` co-sign) cannot be closed by ordering alone | fund only after deposit init succeeds; a deposit that is visible yet stays `INITIALISED` (`LadderAtSight::Plain`: its ladder could not be established and it was deliberately not booked), or one the SDK's `claim()` pass books and reports skipped (`LadderAtSight::Defer`), is a stop-funding signal |
| B6 | **RETIRED 2026-09-06 — Un-conveyed ancestor locktimes.** The boundary was that the deposit-anchored deadline ran late by `k·interval` for parents transferred `k` times, and nothing conveyed `k`. There is no deposit-anchored deadline and no ancestor locktime: nothing is conveyed because nothing exists. What bounds a coin's off-chain life instead is its renewal/rollover capacity — the `TesrParams` schedule and the SE's signature budget — and both ARE conveyed and counted by the census (R5), so a receiver reads the true remaining capacity off the bundle. The `k_max = 14` assumption survives only inside `auto_exit_margin_blocks_for` (`clients/libs/rust-sdk/src/config.rs`), sizing a margin that now measures nothing | — | the census; re-anchor at the renewal/rollover cap (manual today — see the header note) |
| B7 | **Loss of local state** — `wallet.db`/bundle loss is loss of funds (mnemonic alone is NOT a backup). A coin's entire exit material lives in the backup rows — `tesr-*` for a whole coin, `ctesr-*` for a split child, `spinetip-*` for a spine tip — and `export_recovery_bundle` snapshots all of them. Token wallets additionally require the entire `rgb_data_dir` (its own **plaintext** `rgb.mnemonic` seed + the RGB stash), which the recovery bundle deliberately does NOT embed | The SE is blind and cannot re-serve per-coin exit material; that's the privacy design | `export_recovery_bundle` after every operation (incl. child re-transfers and refreshes) + copy `rgb_data_dir`; watch bundles as partial redundancy; device-at-rest security for the plaintext RGB seed |
| B8 | **Payment atomicity** — a plain transfer is a gift, not an escrow | In-protocol delivery-vs-payment needs a shared arbiter | Lightning latch / invoices for atomic swaps; ordinary commerce risk otherwise |
| B9 | **Single live instance per wallet** — the wallet-record lock is in-process only; two processes/devices on one `wallet.db` (or a restored bundle beside a live original) can broadcast stale state against each other and corrupt the DB | No cross-process/cross-device coordination exists (and the blind SE cannot arbitrate) | one live instance per wallet; bundle restore is disaster recovery, not device sync |
| B10 | **Split commit ordering** — an in-ladder split sets the parent's spend budget to terminal (`set_spend_budget … 1`) BEFORE the child tiers are durably persisted, so a crash or backend fault in that window leaves the parent terminal while the child state is not fully recoverable, forcing a unilateral exit of the parent's *value* (no funds lost — the BTC exits via the parent's own ladder — but the cooperative off-chain path for that operation is gone) | The budget MUST precede the co-signature: it is the SE-side monotonic guard (`set_sig_budget`, `server/src/database/deposit.rs` — an ABSOLUTE budget, clamped to `min(count_finalized + remaining, existing)`) that, with the MuSig2 one-shot secnonce consume, prevents a second conflicting co-signature of a terminal node (INV-19 fork). No reordering is safe. **A "persist the nonces and re-call sign/second on restart" fix is unsound**: the enclave atomically nulls the secnonce on the *first* partial signature (`lockbox/src/server.cpp`), so a replayed `sign/second` returns an error, never the original server partial sig — persisting nonces buys nothing once signing has finalized | unilateral exit recovers the BTC value; a durable prepare/commit (persist the *assembled signed tx* before terminalizing, replay locally) would restore the cooperative path — deliberately not shipped as a half-mechanism (a persist with no tested recovery reader is false comfort) |
| B11 | **Enclave attestation identity is a PINNED key — CO-1 is the residual.** Every attestation is signed with ONE long-term identity key derived from the enclave seed (`utexo/attestation-identity/v1`), published at `GET /attestation_identity`, and the client verifies against a **pinned** value. Resolution is pin → config → **refuse**; a compiled-in pin is not overridable, and "neither" is a refusal, never a fallback to the served key | A chain anchor for this key cannot exist for every coin: a depth-≥2 IN-LADDER-SPLIT ancestor's funding output (`SP.out[j]`) is deliberately un-broadcast — the same fact that used to be stated of the retired branch lane, and it survives the lane's deletion because it is a property of the split, not of the lane — so a per-coin server key has no honest anchor, and the verifying key would otherwise arrive in the same HTTP body as the signature it checks. `validate_tx0_output_pubkey` is NOT a substitute — the sender picks `user_public_key`, so `U := D − E_sid` makes any attacker-chosen output pass. `ladder_binding_precheck` (coordinator aggregate vs on-chain `F`) is sound but needs the coin on chain, and **the attacker picks the depth**, so applying it only where the chain happens to have the parent means the attacker picks whether it runs | The pin works at every depth and does not depend on the coordinator's word. **Residual CO-1: a malicious enclave can attest anything** — that is the anchor the design rests on, and lockbox and coordinator are the same operator (§3). Operational cost, taken knowingly: rotation is a client release, and a second operator needs a second pin. The intended successor is an on-chain anchor with a rotation chain, for which this pin is the genesis entry. **Since 2026-09-06 the pin's absence costs more than it did, and the cost must be named rather than glossed.** It gates RECEIVING on every public network (the attested count is the census's right-hand side, R5) and there is no un-laddered shape to receive instead; and it gates the SDK's own ESTABLISH pass (`get_statechain_info` → `attestation_identity` → REFUSE), so an SDK deposit on an unpinned network is booked with no exit material and can only be withdrawn COOPERATIVELY. Until the flat backup was removed, that same coin had a unilateral exit needing NO attestation at all; it does not now. Not a live regression — no public network has an enclave provisioned, so no wallet is in that state today — but a not-yet-deployable one (B12) |
| B12 | **ADDED 2026-09-06 — A coin with no ladder has no exit, and there is no second lane.** With the flat backup gone, "un-laddered" no longer means "on the old lane"; it means NO exit material and NO conveyance. Four populations are found there (header, "What is still un-laddered"): a carrier the coloured builder could not take this pass (`RgbCarrier`), a plain ladder over a carrier (`PlainLadderOverCarrier`), a carrier no coloured ladder can ever be built for (the sub-floor pieces, whose migration hatch now opens onto refusing registration lanes), and every SDK deposit on a network with no pinned enclave identity (`AttestationIdentityUnpinned` — the SDK's establish pass gates on the attested `get_statechain_info`, while `check_deposit` under `LadderAtSight::Plain` does not). **The consequence, stated exactly, and it differs by LANE.** Under `LadderAtSight::Plain` (`mercuryrustlib::update_coins`) a deposit whose ladder cannot be established is NOT booked — `check_deposit` puts the coin back to `INITIALISED` and refuses — so nothing is exposed there. Under the SDK's `LadderAtSight::Defer` — **the only lane a wallet user takes** — the deposit IS BOOKED with the chain's status and the establish pass runs afterwards; if it is skipped the wallet holds a coin with a balance and NO exit material. Such a coin cannot be conveyed (`execute_ex` refuses by name), cannot be unilaterally exited (`unilateral_exit` has no pre-signed chain to walk), and is OMITTED from `export_watch_bundle` without an error, so a delegated tower is never told about it. **COOPERATIVE WITHDRAWAL is the only route out**: `withdraw::execute` reads no backup rows and no ladder and works on any `CONFIRMED` / `IN_TRANSFER` / `DUPLICATED` coin — which means the value is recoverable but ONLY with the SE's co-operation. **What the removal cost, plainly**: the flat backup used to give every coin a unilateral exit that needed no attestation of any kind; it is gone and nothing replaced it. Not silent — each skip reason is recorded (`ladderskip-<sid>`) and listed by `flat_only_coins` — but not defended either. Not a live regression either: mainnet, testnet and signet have no enclave provisioned at all (`TesrParams::attestation_identity_const` is `None` for each), so this is a NOT-YET-DEPLOYABLE state on those networks rather than a fault any wallet is in today; regtest is pinned and unaffected | Exit material is a ladder, and a ladder needs a colourable allocation (carriers) or an attestation the client can verify (the SDK pass); neither can be invented client-side, and cooperative withdrawal is a fallback that reintroduces the SE-liveness dependence (T-SE-2) the ladder exists to remove | `claim()` retries the coloured builder each pass; cooperative `withdraw` while the SE is up; pin an enclave identity (`SPEC.md` §0.4 V-6) — the only remedy that restores the SE-free exit. The sub-floor class has no remedy today beyond a later pass that can colour it |

### Evidence index

*Status legend (2026-09-07, corrected against `git show 9ddc4bb -- clients/tests/rust/src/`).*
**re-derived, pending run** — the test file was rewritten in commit 9ddc4bb to the rule (first-sight
establishment, the flat term 0, `LadderAtSight`) and has NOT been run against the regtest stack
since; it is cited as traceability, not as evidence (`SPEC.md` §0.2(3)). The E2E crate COMPILES and
`cargo test` is green for `mercuryrustlib` (387), `mercury-utexo-sdk` (152) and `ci-guards` (32
suites), but no E2E flow has been executed under the rule, so **no row below may be read as
measured** unless it says so. **DELETED** — the file is gone and its dispatch arm was removed from
`main.rs`; the id no longer exists (RGB_E2E 1, 2, 3, 5, 6, 8, 9, 10 and SDK_E2E 73, 78). *(The
previous edition of this legend carried a third class, "premise retired; re-derivation pending — the
file is untouched", and applied it to sdk32, sdk34, sdk39, sdk54, sdk55, sdk70, sdk82, sdk86, sdk87
and sdk88, and a fourth saying sdk41–47/53–57/70 were unchanged apart from `sdk40`'s deposit helper.
Both were WRONG: every one of those files was rewritten in 9ddc4bb. They are "re-derived, pending
run" like the rest, and the rows below say so.)*

| Claim | Test |
|---|---|
| Receiver rejects a raceable handover even from a guard-bypassing malicious sender | `sdk46` (R′ census against the real SE count at first sight: a hidden extra signature, an undercount and a flat term of 1 are each rejected — re-derived, pending run), `sdk47` (the same census across a full transfer of a pre-established ladder); `sdk54` (re-derived to the flat term 0; pending run) |
| No flat backup and no branch material travels with a ladder — root, child, tail, stub or spine tip; the census flat term is 0 by construction | unit `an_empty_vector_passes_on_both_lanes`, `plain_backups_are_refused_on_both_lanes_and_the_refusal_names_the_lane`, `rgb_material_on_a_flat_backup_is_refused_like_any_other_flat_backup`, `an_unparseable_backup_is_refused_without_being_read` (`clients/libs/rust/src/tesr.rs`); `sdk54`, `sdk70` (the flat-term-0 control and its attacks — both re-derived, pending run); `sdk55` (re-derived: it no longer attacks `validate_backup_chain_v2`; it now asserts that the flat term is IDENTICALLY ZERO, cannot be padded, and that a disclosed rival cannot be inverted — pending run) |
| Ladder at first sight: a deposit is laddered while still `IN_MEMPOOL`, the enclave count after deposit is 3, and no flat row exists | `sdk48` (re-derived, pending run) |
| A received coin carries no flat row and `locktime == None` at k = 0, 1, 2; the ladder is byte-identical across hops and `F` stays unspent | `sdk86` (re-derived: the `min(L_k)` measurement is INVERTED — it now asserts the unconditional INV-27 across two hops and 300 idle blocks, with `coin.locktime == None` and no flat row; pending run) |
| Watch bundle is keyless (no key material); keyless tower defends an offline owner; two independent towers idempotent; every exported entry is event-driven with `deadline_block: u32::MAX` | `sdk45` (all three — re-derived, pending run), `sdk51` (the owner's own pass, same hostile trigger; file unchanged); unit `bundle_roundtrip_and_carrier_has_no_backup`, `an_empty_chain_aborts_the_export_instead_of_dropping_the_coin`, `the_export_consults_the_leaf_rows_before_the_flat_coin_shortcut` (`clients/libs/rust-sdk/src/watchtower.rs`) |
| A prior owner's superseded state loses the CSV race; the honest owner's lowest-CSV state wins | `sdk51`; `sdk40` (PART 2 — re-derived, pending run). *(The matured-flat-backup halves that used to sit here — `sdk32` (C), `sdk34` (E) — asserted material that no longer exists: there is no matured spend of `F` for a past owner to broadcast. Both files were re-derived in 9ddc4bb onto the coloured / event-driven forms; pending run.)* |
| A received coloured child is defended on the EVENT of its parent's `F` being spent — `defend_ladders`' child loop, not a scheduled deadline pass (the leaf near-deadline loop is deleted) | `sdk34` (re-derived to the event-driven defence — a received piece has no calendar deadline and `defend_ladders` answers a hostile trigger on the shared `F`; pending run) |
| A laddered carrier is never "due" for the deadline passes at any margin (`coin.locktime` is `None`) | `sdk87` (re-derived: the deadline pass leaves a laddered carrier ALONE at any margin, and the RGB-safe sever remains only as an owner-named call; pending run) |
| An RGB carrier is never given a PLAIN ladder (terminal-freeze) while a plain coin in the same wallet is | `sdk52` |
| A carrier IS laddered — COLOURED: establish, renew against a rival, convey; then walk it out on chain with the allocation intact | `sdk74` (re-derived to first-sight, flat-term-0 establishment — pending run), `sdk75` |
| A sub-floor carrier the coloured builder can never take | `sdk78` is **DELETED** (file gone, dispatch arm removed from `main.rs`) — SDK_E2E=78 no longer exists; it was retired with the hatch's payout route, which now returns `Err` at `register_combine_subcoins` before anything is registered. What survives it is `rgb16` (`RGB_E2E=16`, `rgb16_legacy_lane_uncolourable.rs`), which reproduces sdk78's measurement deterministically without an SE — rgb-lib answers `Invalid coloring info` for a legacy-lane carrier because `accept_incoming_tokens` imports only the genesis, so the stock has no witness ord and no revealed seal at the piece's outpoint. The gap it names is unchanged and open: a sub-floor carrier on a pinned network has no ladder, no conveyance and no SE-free exit (B12). `rgb16`'s own file was NOT touched by 9ddc4bb (it predates the rule and needs no Mercury server), so its last run is its own; it has not been re-run under the rule |
| A coloured carrier idled a "year" keeps its allocation, and every RGB-unaware route to it is refused by name | `sdk32` (re-derived onto the coloured lane; pending run) |
| The plain off-chain split is GONE, so a retained trigger has nothing to race in a batch payment | `sdk69` (the case that used to assert the refusal — re-derived, pending run) |
| A laddered coin never ages: unbounded **off-chain** renewal, zero on-chain bytes, and no calendar anywhere on the coin (INV-27, unconditional) | `sdk43`, `sdk41` (both re-derived in 9ddc4bb; pending run); `sdk40` (PART 3) and `sdk50` (the walk completes with no absolute-locktime backup in sight) — both re-derived, pending run |
| Received split child is FIRST-CLASS: handover completes (sender locked out, `A_child` invariant), then paid onward off-chain — whole and split | `sdk60` (alice→bob→carol, funding outpoint unspent throughout), `sdk17` (partial second hop) — both re-derived, pending run |
| In-ladder split child bundle: valid child ACCEPTED, 11 adversarial variants REJECTED (aggregates, hidden state, Model-A, parent terminality, child-superseded race, count padding, value spoof) | `sdk58` (re-derived, pending run) |
| Parent and ancestor-segment terminality from the enclave-signed payload (`attested_terminal`) | `sdk58`, `sdk60` (both re-derived, pending run). *(RETIRED with the branch lane: the per-input requirement, non-tree rejection and `terminal_parents_tests`.)* |
| **The conveyance window is the ONLY server-side gate on this path, and a bypassing payer gets a `sign/first` session once it lapses** (409 inside, HTTP 200 + `server_pubnonce` outside; payee still claimed his coin intact in the run) | `sdk91` — a MEASUREMENT, not a pass/fail; `sdk90` measures the client-side gates only. `EXPECT_LATCH=1` turns both into assertions when REQ-61's owner latch ships |
| The window SQL's release rule (non-batch 1 h; latch batch until `MAX(expires_at)`; plain batch until `batch_time + batch_timeout`) | **No behavioural test** — no test DB or embedded Postgres exists here; `open_window_invariant_tests` models the arithmetic in Rust and asserts `OPEN_TRANSFER_WINDOW_SQL` carries that shape |
| SSP pre-payment value gate reads TRUE coin value (SATS census-bound peek, RGB consignment-derived amount) | `sdk37`; `sdk20` (SATS gate through real `execute_pay` + RLN) — both re-derived, pending run |
| Transfer-signature (R1) reject paths | `unit::transfer_signature_tests` (`ladder_interval_check_rejects_wrong_and_increasing` in the same module exercises the RETIRED lib arm, not the receive path) |
| The census's right-hand side is enclave-signed: an under-reported `num_sigs`, a replayed nonce, another coin's attestation, a coordinator-chosen signing key, and the budget-less `v1` preimage are each REFUSED | `unit::sig_count_attestation_tests` (`lib/src/transfer/receiver.rs`) |
| Ladder CSV cadence is client-canonical, not SE-served — a whole lifecycle (establish → renew to the budget → roll over → exit) driven off `TesrParams` alone | `sdk44` (re-derived, pending run) |
| Sponsored-refresh bounded loss (stiffing sponsor → user keeps refreshed coin) | `sdk38` |
| Depth-2 coloured sub-coin exits on-chain, allocation preserved | `sdk39` — re-derived onto the coloured lane (the legacy flat coloured split it used to drive is gone); pending run. The coloured on-chain walk it shares the property with is `sdk75`, whose own file is unchanged |
| A laddered claim admits a mempool `F` (the coin is booked with the chain's status) and refuses a spent one | code `verify_tx0_output_is_unspent_and_confirmed` (`clients/libs/rust/src/transfer_receiver.rs`) — **no E2E asserts the mempool admission on the receive path, and no ordinary send can produce one**: the sender's coin filter in `execute_ex` admits only `CONFIRMED` / `IN_TRANSFER` (`tb01` records that refusal). Reachable only via a reorg between send and claim — UNPROVEN. (`sdk48` ladders a mempool deposit; nothing conveys one.) *(RETIRED: the mempool-root rejection for branch roots — there are no branch roots.)* |
| SSP RGB pay through Lightning (RGB gate wiring end-to-end) | **RESIDUAL — no E2E.** What changed on 2026-09-06 is that the gate now has material to judge: `peek_pending_transfers` derives the RGB envelope from the conveyed BUNDLE (a coloured root ladder's or coloured child's LEAF consignment as the `{"c","a","s"}` envelope) plus `rgb_assignment_txid` / `rgb_assignment_vout` and the witness chain in `child_witness_txids`, and `SspService::execute_pay` validates against it (`validate_pending_token_ex`) and refuses only when NEITHER `branch_txs` nor `child_witness_txids` is present — where it previously had no consignment to check at all. That is wiring, not evidence: still no end-to-end flow. The gate's validation logic is cited to `sdk37` and its SATS wiring to `sdk20` — both re-derived, pending run |
| SE nonce atomicity (no double-sign behind the receiver's back) | `sdk12` (re-derived, pending run) |
| Fresh double-sign trust floor — an SE *willing* to double-sign creates a race (the honest counter-example) | `sdk15` (re-derived, pending run) |
| SE refusal ≠ seizure (unilateral exit without SE) | `sdk50` (SDK walks the whole TES-R chain, no SE call — re-derived, pending run), `sdk40` (PART 1, library level — re-derived, pending run), `sdk45` (a KEYLESS third party can drive the same exit) |
| A terminal node is not re-spendable: an in-ladder split leaves the parent TERMINAL at the SE and a second split over it is refused (cause pinned negatively, so plumbing errors cannot pass vacuously) | `sdk04` |
| Stale-state clawback defeated by the CSV ladder + watchtower — and since 2026-09-06 the CSV race is the ONLY clawback a past owner can attempt: no matured ancestor spend of `F` exists (T-SE-1) | `sdk51`, `sdk45`; `sdk40` (PART 2 — re-derived, pending run) |
| The exit-headroom gate (`check_exit_headroom_with_margin`) has no caller; the split-depth cap measures against the fixed `initlock` window | `sdk82`, `sdk88` — both re-derived: each now asserts that a conveyed child has NO EPOCH to run out of (plain and coloured respectively), the epoch having gone with the flat backup it was read from; pending run |
| Token state client-validated (forged/invalid consignments rejected) | `rgb12`, `rgb13` — both re-derived, pending run |
| SSP swap atomicity + adversarial refusals, both directions on the ladder | exact `sdk63`/`sdk64` (both re-derived, pending run), non-exact (latched in-ladder split) `sdk65`/`sdk67` (files unchanged), failure/rollback `sdk66` (unchanged)/`sdk68` (re-derived, pending run), adversarial `sdk19`/`sdk24` (unchanged) and `sdk20` (re-derived, pending run) |
| An LN pay failure never strands the coin (exact lane restores it as exitable; non-exact rolls back) | `sdk68` (exact — re-derived, pending run), `sdk66` (non-exact — file unchanged) |
| Sender pre-claim cancel = overwrite-by-resend (impossible after claim); SSP latch abort; receiver-failure paths | `tm01`, `sdk24`, `sdk19` |
