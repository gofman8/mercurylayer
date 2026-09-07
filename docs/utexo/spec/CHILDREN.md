# First-class received split children

A received non-exact (split) payment is a **first-class, off-chain re-spendable coin** — Spark
parity — not an exit-only claim that has to be materialized on-chain before it can be paid onward.
The child is
conveyed WITH the standard Mercury key handover plus a temporary coordinator-side pending-transfer
lock; it is never terminalized on conveyance and its signing budget is never re-opened.

## Status

Built. It was exercised end to end before the 2026-09-06 no-flat-backup rule, and the flows have
not been re-run against that rule yet — read the dated note under *Evidence* before quoting any of
them. The pending-transfer lock is live on the coordinator
(`has_open_transfer`, `server/src/database/transfer_sender.rs`, checked in `sign_first`/`sign_second`,
`server/src/endpoints/sign.rs`); the conveyance carries the handover (`convey_child_bundle` /
`open_child_conveyance`, `clients/libs/rust/src/tesr.rs`) under message shape **4**, "a child
conveyance with key handover". `protocol_version` selects a SHAPE, not a generation:
`ADMISSIBLE_PROTOCOL_VERSIONS = [2, 4]` is an exact set (`admissible_shape`,
`clients/libs/rust/src/transfer_receiver.rs`) — 2 is a root-ladder conveyance, 4 a child conveyance;
the un-laddered shape 0 no longer exists and cannot be received (since 2026-09-06) — so an unknown
value is refused BY NAME rather than read as "at least". The
receiver completes the handover and books the child as a normal coin; onward re-transfer is
`child_retransfer`; a received child can itself be split (`child_in_ladder_pay` /
`child_in_ladder_pay_many`, `clients/libs/rust-sdk/src/transfer.rs`).

A child bundle conveys NO flat backup at any level of the tree: no coin carries an absolute-locktime
backup (`create_tx1` is deleted; a coin's only exit material is its TES-R ladder, co-signed at the
first mempool sighting of `F`), so `ChildTesrBundle::parent_flat_backups` is always empty and
`refuse_conveyed_flat_backups` (`clients/libs/rust/src/tesr.rs`) refuses a non-empty one by name. The
census that admits a child has no flat term — `PARENT_V2_BASELINE = 0`, `CHILD_V2_BASELINE = 0` — and
a child, like every laddered coin, has no calendar: nothing on it or on any ancestor matures on its
own (SPEC.md INV-27).

**The child is now the ONLY off-chain sub-coin the protocol mints.** The plain off-chain split
`split_coin` — which produced a sub-coin with a `branch-` exit chain and no ladder — is deleted with
`ParentShape::Unladdered` and `ManyRoute::PlainSplit`, so every piece a payment carves is a child (or
a spine tip) with its own `ctesr-` bundle. Two consequences worth stating explicitly, because they
pull in opposite directions:

- **The child's funding is STILL un-broadcast, and always will be.** `SP.out[j]` is never broadcast
  until someone exits, which is exactly where the 0 vB of idle rent comes from. Nothing about
  colouring a tier changes that, and every rule here that depends on it — the ancestor segment chain
  (`F→T→X_m→SP`, the parent's own ladder tiers) conveyed for un-broadcast-funding validation, the
  pinned attestation identity a depth-≥2 ancestor needs because it has no chain anchor (TRUST-MODEL
  B11) — stands unchanged.
- **But "its funding is not on chain" is no longer a LICENCE to convey it flat — there is no flat
  lane to license.** No coin has a flat backup to convey, so `transfer_sender::execute`
  (`clients/libs/rust/src/transfer_sender.rs`) refuses by name any coin with no ladder row ("a coin's
  only exit material is its TES-R ladder … there is no un-laddered lane"), and the recorded
  skip-reason predicate answers **never**: `is_legitimate_flat_reason` returns `false` for every
  reason and `LadderSkipReason::permits_flat_conveyance` (`clients/libs/rust-sdk/src/events.rs`)
  delegates to it. `PermanentLicence::FundingNotOnChain` survives only as a NAME inside comments
  (`clients/libs/rust/src/transfer_sender.rs`, `clients/libs/rust-sdk/src/transfer.rs`), where it
  records what the refusal replaced; the classifier `assert_flat_conveyance_is_legitimate` is gone
  from the tree altogether — it appears in no source file, comments included, and a document that
  cites it as surviving code is citing nothing. `UtexoWallet::transfer` routes a child to
  `child_retransfer`, and a child has nothing else it could be conveyed as.

Evidence: **sdk60** (Alice → Bob → Carol, the whole child re-transferred off-chain, funding outpoint
`F` never spent until Carol's exit), **sdk59** (in-ladder pay, receiver completes the handover),
**sdk58** (11 adversarial in-ladder-split cases; asserts a plain child stays NON-terminal),
**sdk70** (verifier binding), **sdk76** (received-parent split ancestor census), **sdk77** (coloured
in-ladder split), **sdk80** (the watchtower/conveyance ordering window on the child-split lanes),
**sdk41/sdk49/sdk01** (transfers unaffected by the lock), **sdk91** (the open-transfer window
measured against a payer that skips its own client and POSTs `/sign/first` directly — the
coordinator answers `409 Conflict`, *"coin has an open transfer"*). **sdk90** measured only the
CLIENT-side halves of that window (the wallet's own coin lookup and the sender-side
outstanding-conveyance refusal); both are the payer's own software, so sdk90 never reached the
server-side gate — sdk91 is the test that does. Measurements are regtest.

*(2026-09-06: **every flow named above is pending run, and none of them is evidence under
SPEC.md §0.2(3) until it runs.** sdk58, sdk59, sdk60, sdk76, sdk77, sdk80 and sdk70 were re-derived
against the no-flat-backup census — flat term 0 at every slot, enclave count 3 after deposit.
sdk41/sdk49/sdk01 are ordinary transfer flows that the same rule moved under (a conveyance now
carries `backup_transactions: []`, and a deposit is laddered instead of backed by `tx1`), so their
earlier runs measured a shape that no longer exists; an earlier green is not a current one. sdk91's
subject — the coordinator's own open-transfer window query — is untouched by the rule, but it has
not been re-run either. What HAS been measured since the change is the offline suite: `cargo test`
green for mercuryrustlib, for the SDK and for the ci-guards, with the E2E crate compiling.)*

## The mechanism — the normal transfer, applied to the child

1. **Sender** in-ladder-splits: the child slot `A_child = sender + SE`; the sender co-signs the child
   ladder (`ext_child + state_child`) paying the receiver (Model A) under `A_child`, and terminalizes
   the PARENT. It conveys the child bundle **plus the key-handover material** — `x1` from
   `get_new_x1`, `t1`/`transfer_signature`, and the ancestor **segment** `F→T→X_m→SP` (the parent's
   ladder tiers) for un-broadcast-funding validation. It conveys no flat backup — there is none. The
   CHILD is not terminalized.
2. **Receiver** claim: `verify_child_bundle` (parent `F` read on-chain through the ancestor segment,
   exact-equality census) and THEN completion of the key handover (`POST /transfer/receiver`): the SE
   rotates its share so `A_child` is INVARIANT
   (`sender_share + SE_old == receiver_share + SE_new == A_child`) and re-points `auth` to the
   receiver. Every pre-signed child tier, co-signed under the invariant `A_child`, stays valid. The
   receiver now co-owns `A_child` and the sender is locked out — first-class.
3. **Onward re-transfer** (receiver → next): a normal Model-A transfer of the child. `child_retransfer`
   co-signs a fresh state over `ext_child.out[0]` one δ lower — so it out-races the state it replaces —
   discloses the replaced state as superseded, and conveys the **multi-hop** bundle (ancestor chain
   rooted at the on-chain `F`). The next receiver runs the same census in its N-hop form —
   `se_num_sigs == Σ tiers + Σ superseded` per slot across the ancestor chain, the flat term ZERO by
   construction (`PARENT_V2_BASELINE = 0`, `CHILD_V2_BASELINE = 0`) — now including the
   child-superseded segment (in sdk60: `child_num_sigs == 0 + 2 + 1`).
4. **Child-level split**: paying a non-exact amount out of a received child runs the same split one
   level down. `child_in_ladder_split` terminalizes the CHILD (budget → 1, then the `CSP` co-sign) and
   hands the terminalized child segment to the grandchildren as an ANCESTOR; each grandchild is
   conveyed non-terminal with its own handover, exactly as in step 1.

The rule is uniform at every level: **the node being split is terminalized; the piece being conveyed
is not.**

## Why the child is not terminalized, and why its budget is never re-opened

Terminalize-then-reopen — making the child terminal at conveyance and later re-opening its budget for
the receiver — is unsound and must not be built:

- It fights the **monotonic-budget clamp (INV-19)** in `set_sig_budget`
  (`server/src/database/deposit.rs`): a budget may only tighten, never loosen, precisely so a node
  cannot be re-opened for a second, conflicting spend. Any reopen is a raw UPDATE that resurrects the
  fork class the clamp exists to prevent.
- The sender stays the child's owner until the receiver completes, so it could re-address the
  pending-transfer row to an attacker key AFTER the victim accepted (`insert_new_transfer` deletes by
  `statechain_id`, `server/src/database/transfer_sender.rs`), re-arm, self-complete and reopen-to-self —
  a double-spend of the child. Binding the reopen to the pending recipient does not close this,
  because the sender controls that recipient value for the whole window.

With the handover instead, the sender's share is rotated out, so it can never co-sign a child rival;
the CSV race — the child's weaker defence — never has a rival to arbitrate. The census, checked at
completion, rejects any pre-conveyance hidden state.

**The child's two-layer safety, and it is one indivisible change:**

- the **census** (`child_num_sigs == 0 + tiers + superseded`, checked at receive) closes any
  PRE-conveyance rival;
- the **pending-transfer lock** (an open transfer on the child) closes any POST-conveyance rival,
  until the handover makes the lockout permanent.

Terminality and the handover (auth rotation) are PERMANENT lockouts; the pending-transfer lock is
TEMPORARY — it expires with the open-transfer window (`OPEN_TRANSFER_WINDOW_SQL`,
`server/src/database/transfer_sender.rs`), a `CASE` on `batch_id` rather than a conjunction: the
batched branch holds until the batch's receivers can no longer claim — `MAX(l.expires_at)` over the
batch's `lightning_latch` rows, falling back through `COALESCE` to `batch_time + batch_timeout` for
a plain (non-Lightning) batch — while the non-batch branch, every ordinary payment, is a hard-coded
`updated_at > NOW() - INTERVAL '1 hour'` with no setting that shortens it. That window query is never executed by a unit test — there
is no test database, no sqlx offline fixture and no embedded Postgres here, so
`open_window_invariant_tests` models the arithmetic in Rust and checks the SQL carries that shape;
the non-batch branch is executed for real only by sdk91 against the live stack, and the BATCHED
branch's timeout is not exercised end to end at all. A child left non-terminal
AND exit-only — conveyed without a handover, or held past the lock's expiry without completing it —
can be out-raced by a rival the still-owner sender co-signs. That is theft. So the four moves are a
single unit and must stay so: `in_ladder_split` not setting the child budget to 0,
`verify_child_bundle` not requiring `child_terminal` (it still requires `parent_terminal` and the
census), `convey_child_bundle` carrying the handover material, and the receiver COMPLETING the
handover.

## The pending-transfer lock

Between the receiver's census check and its handover completion, the still-owner sender would
otherwise be able to co-sign a lower-CSV rival — `server/src/endpoints/sign.rs` refuses on single-use,
spend-budget and epoch, none of which covers an in-flight transfer. The lock closes it: when
`/transfer/sender` (`get_new_x1`) opens a transfer, the coordinator refuses that `statechain_id`'s
co-signs until the transfer completes (`key_updated`), is cancelled (`cancelled_at`, authorised by
the recorded recipient once anything has been conveyed) or its window lapses, and
`has_open_transfer_to_other_auth` (`server/src/database/transfer_sender.rs`, called from
`server/src/endpoints/transfer_sender.rs`) rejects re-addressing an open transfer to a different
recipient — it splices a key-inequality into the SAME window query, and the two must stay the same
rule. It is a RELEASABLE lock, not a monotonic budget — no INV-19
conflict, no reopen. It is coordinator-side and lockbox-testable, and it hardens every transfer, not
only the child lane. This TOCTOU affects the ordinary transfer path too, which is why the lock is not
scoped to children.

## Two ordering rules the lanes must obey

- **All sender pre-signs happen BEFORE `get_new_x1`.** In `transfer_sender::execute`
  (`clients/libs/rust/src/transfer_sender.rs`) the sender's only pre-sign is `presign_receiver_state`
  (co-signing the receiver-paying state `S'`; `cosign_colored_receiver_state` on a coloured ladder),
  and it needs no `x1`; only `t1 = o1 + x1` in `create_transfer_update_msg` does. There is no
  receiver backup to co-sign: `create_backup_transactions` / `create_tx1` are deleted and a laddered
  conveyance carries zero `backup_transactions`. If `get_new_x1` ran first it would arm the lock and
  block the sender's one legitimate pre-sign, breaking every transfer. With the pre-sign first, no
  co-sign after the transfer opens is legitimate, so refusing it is safe by construction. *(Until
  2026-09-06 `create_backup_transactions` also ran here, ahead of `get_new_x1`; the ordering rule is
  unchanged, its subject is one co-sign instead of two.)*
- **A durable arm-down precedes every superseding co-sign.** The watchtower's child loop in
  `defend_ladders_inner` (`clients/libs/rust-sdk/src/wallet.rs`) filters on the coin's DURABLE status
  alone; it has no supersession check and cannot have one on this lane, because a grandchild bundle
  carries the ROOT parent's ids and the child's own terminalization lives in `ancestors`. So any lane
  that co-signs a superseding state must move the coin out of CONFIRMED durably **before** the
  co-sign and before any conveyance — otherwise the tower may be admitted to broadcast a state the
  recipients' chains supersede, which voids their pieces. This holds for `execute_ex`,
  `child_retransfer`, `cosign_colored_child_retransfer` and the `in_ladder_pay` /
  `child_in_ladder_pay` / `child_in_ladder_pay_many` lanes. sdk80 measures the window with markers the
  wallet does not write (the SE's `num_sigs` for the child, the recipient's coordinator mailbox depth)
  and asserts zero admitted samples; the CI guard
  `ci-guards/tests/deny_armed_tower_during_conveyance.rs` pins the ordering in source.

## Residual gaps

- **The SENDER still picks the version.** Both child gates check the shape exactly — the pre-pay
  census (`prepay_child_census`) and the claim path's child block (`validate_encrypted_message`,
  `clients/libs/rust/src/transfer_receiver.rs`) each call `admissible_shape` and then refuse anything
  that is not `SHAPE_CHILD`, so a child bundle conveyed under any other tag is rejected rather than
  processed by rules that predate the lane (unit-pinned by
  `the_child_lane_admits_exactly_shape_four`). What remains is the FLOOR: `protocol_version` is a
  sender-declared field, and a conveyance made at a shape carrying neither the key handover nor the
  transfer signature is a downgrade the receiver does not choose. The uniffi FFI performs that
  downgrade itself, stripping `protocol_version`, `tesr_ladder` and `child_tesr_bundle` — exact-set
  dispatch is what makes a stripped tag fail CLOSED: since 2026-09-06 there is no flat census arm on
  the receive path at all (`statechain_info.num_sigs != backup_transactions.len()` is retired with
  shape 0), so a stripped tag is refused by `admissible_shape` with no arm to land on. The fix is a
  two-sided version check with a floor the RECEIVER sets. See SPEC.md A-12.
- **The child lane can bump its EXIT but not its WATCH.** `exit_child_pass_with_bump` and
  `exit_spine_tip_pass_with_bump` (`clients/libs/rust/src/tesr.rs`) exist and are wired into
  `unilateral_exit` (`clients/libs/rust-sdk/src/wallet.rs`). The watch half is not:
  `watch_child_pass_seen` has no bump variant, so a tower defending a child tier is stuck at the rate
  the tier was signed at. And the 1P1C package bump that rescues a tier above the committed
  3.0 sat/vB needs a funded UTXO, a signer and a Core RPC endpoint, so a KEYLESS tower has no move at
  all. See SPEC.md A-1.
- **Value floor — of a TWO-RUNG child, and of nothing else.** `min_child_value` is `V_min` evaluated
  at the shipped 3.0 sat/vB — 1 560 sat — correct at that rate and no other, and below it a two-rung
  child's own unilateral walk costs more than the piece. It is **not** the floor a payment is
  admitted at, and reading it as one overstates what a received leaf is guaranteed. REQ-83's leaf
  bands (`LeafShape::for_value`, `lib/src/tesr.rs`) mint thinner legs deliberately: a one-rung
  `ThinPiece` at `min_spine_tip_value` (945 sat at the same rate), then a rung-less `Ladderless`
  stub floored at the dust limit, then a sub-dust `Tail` — and the piece floor
  `split_output_floors` actually applies is `SplitLegRole::Tail.min_value`, which is **1 satoshi**.
  The bands below one rung do not exit unaided (`LeafShape::exits_unaided` is false for them); that
  is the owner's accepted trade — small leaves leave with their group — not a protection the payee
  can lean on. See SPEC.md G11 and PARTIAL-PAYMENT-ECONOMICS.md.
- **A prior owner of an ancestor cannot void a piece — but a sub-economic piece is still not worth
  defending.** *(Rewritten 2026-09-06; this bullet used to read "a prior owner of an ancestor can
  void a sub-economic piece with one 112-vB backup, at zero marginal cost per extra piece".)* No
  prior owner holds a matured spend of `F`: the only spends of `F` in past owners' hands are the
  retained triggers `T` — no timelock, so the current owner or their watcher can always pre-empt, and
  a broadcast `T` merely starts every leaf's walk — and the superseded states, which lose the CSV
  race. What survives is the VALUE bound: defending a piece costs its walk (`3 + 2d` transactions),
  so a piece below `V_min` is stranded on chain at its own aggregate, not taken. See SPEC.md G4, X-2
  and L-2, TRUST-MODEL.md, and PARTIAL-PAYMENT-ECONOMICS.md §10.

## See also

PROTOCOL.md §5.4 (the in-ladder split and why the child is not terminalized) and §5.11 (the
exact-equality census, REQ-38); SPEC.md (REQ/INV/threat rows); TRUST-MODEL.md (what the operator can
and cannot do); LIGHTNING.md (the non-exact LN latch, which conveys a child mailbox row born
batch-locked); PARTIAL-PAYMENT-ECONOMICS.md (what a partial payment costs the receiver).
