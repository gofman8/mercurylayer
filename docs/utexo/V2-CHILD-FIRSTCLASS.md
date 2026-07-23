# First-class received split children (Spark parity) — the sound design

**Status (2026-07-23): DESIGN SETTLED after four adversarial cycles. The reopen approach is OUT; the
sound design is "convey the child WITH the key-handover + a temporary pending-transfer lock". Implement.**

## The requirement (user)
A received non-exact (split) payment must be a **first-class, off-chain re-spendable coin — Spark parity**,
not an exit-only claim that must be materialized on-chain to re-spend.

## Why the earlier answers were wrong
- **Exit-only claim (shipped as a simplification):** my in-ladder split conveyed only the child's *exit
  bundle* (`ChildTesrBundle`), NOT a key-handover. So the receiver never became a co-owner of `A_child`
  (= sender + SE), could only broadcast the pre-signed exit, and needed the sender terminalized to stop a
  rival. That is NOT the spec — V2-MIGRATION.md:393-394 specifies a **multi-hop census** so children are
  transferable; ARK-SPARK-PARITY.md specifies the off-chain transfer IS the SE key-handover.
- **SE handover + budget-reopen (rejected, 2 reviews):** trying to terminalize the child then RE-OPEN its
  budget for the receiver is unsound: (a) it fights the **monotonic-budget INV-19 clamp**
  (`server/src/database/deposit.rs:236-238` — "a budget may only tighten, never loosen … re-open the node
  for a second, conflicting spend / INV-19 fork"); any reopen is a raw UPDATE resurrecting the fork class
  the clamp prevents. (b) The sender stays the child's owner until the receiver completes, so it can
  re-address the pending-transfer row to an attacker key AFTER the victim accepted (`insert_new_transfer`
  deletes by `statechain_id` only, `server/src/database/transfer_sender.rs:62`), re-arm, self-complete,
  and reopen-to-self → double-spend the child. Binding the reopen to the pending recipient does NOT close
  this (the sender controls that recipient value for the whole window). **Do not build the reopen.**

## The sound design — the NORMAL transfer, applied to the child
The child needs terminalize/reopen ONLY because it is conveyed without a handover. Convey it WITH the
standard Mercury key-handover (the mechanism proven by sdk41/sdk49) and every problem dissolves:

1. **Sender** in-ladder-splits: child slot `A_child = sender + SE`; co-sign the child ladder
   (`ext_child + state_child`) paying the receiver (Model A) under `A_child`; terminalize the PARENT
   (existing, unchanged). Convey the child bundle **+ the key-handover material** (`x1` via `get_new_x1`,
   `t1`/`transfer_signature`, and the ancestor **branch** `F→T→X_m→SP` for un-broadcast-funding
   validation). Do NOT terminalize the child.
2. **Receiver** claim: `verify_child_bundle` (parent `F` on-chain via the branch, exact-equality census)
   THEN **complete the key-handover** (`POST /transfer/receiver`): the SE rotates its share so `A_child`
   is INVARIANT (`sender_share+SE_old == receiver_share+SE_new == A_child`) and re-points `auth` to the
   receiver. The pre-signed exit ladder (co-signed under the invariant `A_child`) stays valid. The
   receiver now co-owns `A_child` and the sender is **locked out** (auth rotated) ⟹ **first-class**.
3. **Onward re-transfer** (receiver → next): a normal Model-A transfer of the child — co-sign a new state
   over `ext_child.out[0]` one δ lower, disclose the old state as superseded, convey the **multi-hop**
   bundle (ancestor chain rooted at on-chain `F`). Verified by the **N-hop census**
   (`se_num_sigs == v1_backups + Σ conveyed_tiers`, V2-MIGRATION.md:393-394) — a generalization of
   `verify_child_bundle`.

**Why no terminalize/reopen:** after the handover the sender's share is rotated out, so it can never
co-sign a child rival — the CSV race (the child's weaker defense) is irrelevant because the sender has no
valid rival to broadcast. The census (checked at completion) rejects any pre-conveyance hidden state.

## The one new primitive — a temporary pending-transfer lock
GAP (a TOCTOU that also affects the CURRENT transfer): between the receiver's census-check and its
handover completion, the still-owner sender could co-sign a lower-CSV rival (`server/src/endpoints/sign.rs`
refuses only single-use / spend-budget / epoch — **no pending-transfer check**). Close it with a
**temporary** lock: when `/transfer/sender` (`get_new_x1`) opens a transfer, the coordinator refuses the
sender's co-signs on that `statechain_id` until the transfer completes (`key_updated`) or times out
(batch timeout). This is a RELEASABLE lock, NOT a monotonic budget ⟹ no INV-19 fight, no reopen. It must
NOT block the sender's own pre-sign of the receiver-paying state (do the pre-sign BEFORE opening the
transfer). Coordinator-side, lockbox-testable. It also hardens every V2 transfer.

## Progress
- **✅ Pending-transfer lock LANDED + VERIFIED + DEPLOYED (commit 19e6668).** `sign_first`/`sign_second`
  refuse a co-sign while a transfer of the coin is open; `get_new_x1` rejects re-addressing an open
  transfer to a different recipient. Safe after the `get_new_x1` re-order (7e23891). Verified sdk49/41/01
  (transfers) + sdk58/59 (in-ladder split) green on the live server. This is the enabling primitive.
- **Refined child security model (replaces terminalize+reopen):** with the pending-lock live, the child
  is NO LONGER terminalized. Its anti-hidden-state safety is: the **census** (`child_num_sigs == v1_backups
  + conveyed_tiers`, checked at receive) closes any PRE-conveyance rival, and the **pending-lock** (an open
  transfer on the child) closes any POST-conveyance rival. So `in_ladder_split` drops
  `set_spend_budget(child_sid, 0)` and `verify_child_bundle` drops the `child_terminal` requirement (KEEP
  `parent_terminal` + the census). **INTERDEPENDENT:** dropping `child_terminal` is only safe TOGETHER
  with the receiver completing the handover (which locks out the sender via auth rotation); they must land
  as one coherent change, not piecemeal.

## Implementation targets
- Client sender (`clients/libs/rust-sdk/src/transfer.rs in_ladder_pay`, `clients/libs/rust/src/tesr.rs
  convey_child_bundle`): stop terminalizing the child; build + convey the handover material (t1 /
  transfer_signature) + the ancestor branch alongside the child bundle.
- Client receiver (`clients/libs/rust/src/transfer_receiver.rs` child branch): after `verify_child_bundle`,
  COMPLETE the handover (drive `/transfer/receiver` for `child_sid`), then book the child as a normal
  first-class coin (not an exit-only claim); drop the `child_claim_sids` spend-exclusion once first-class.
- Coordinator (`server/src/endpoints/transfer_sender.rs` + `server/src/endpoints/sign.rs`): the
  pending-transfer lock (open on `insert_new_transfer`, checked in `sign_first`/`sign_second`, released on
  `key_updated` or batch timeout). Reject re-address of an open pending row (`insert_new_transfer` DELETE)
  while a transfer is in flight.
  - **⚠ ORDERING PITFALL (verified):** in `clients/libs/rust/src/transfer_sender.rs::execute`, `get_new_x1`
    (line 268 — opens the transfer) runs BEFORE the sender's own pre-signs: `create_backup_transactions`
    (line 278, `create_tx1`) and `presign_receiver_state` (line 304, co-signs `S'`). A pending-lock that
    refuses co-signs once a transfer is open would therefore BLOCK the sender's own legitimate pre-signs
    and break EVERY transfer. **The lock is only safe if `execute` is first re-ordered to do ALL sender
    pre-signs (backups + `S'`) BEFORE `get_new_x1`** — they need no `x1` (only `t1 = o1 + x1` in
    `create_transfer_update_msg` does). Then, once the transfer is open, no further co-sign is legitimate,
    so refusing them is safe by construction. Re-run the full transfer suite (sdk41/49/50 + the split
    tests) to prove no regression before relying on the lock.
- Census (`clients/libs/rust/src/tesr.rs verify_child_bundle`): generalize to N hops
  (`Σ conveyed_tiers` across the ancestor chain rooted at on-chain `F`), per V2-MIGRATION.md:393-394.
- E2E (sdk60): Alice pays Bob a non-exact amount; Bob adopts + COMPLETES the handover (first-class); Bob
  re-transfers the child OFF-CHAIN to Charlie; Charlie adopts + exits; funds land at Charlie. Adversarial:
  sender-rival-during-pending REJECT (pending lock), hidden-state REJECT (census), re-address REJECT.

Supersedes `V2-SPLIT-FINDINGS.md`'s "received children are exit-only" and the reopen in
`v2-child-retransfer-unsound`.
