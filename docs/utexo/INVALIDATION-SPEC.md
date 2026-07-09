# Old-State Invalidation — Normative Specification

This document is the single authoritative reference for the old-state invalidation mechanism of
the Mercury-Layer statechain fork (Utexo, Spark-compatible API; `feat/spark`). It specifies *how a
previously valid off-chain state is made unspendable-in-practice* when ownership moves, and what
every party MUST do to keep that guarantee. Requirements are numbered **IVL-REQ-n**, invariants
**IVL-INV-n**, error semantics **IVL-ERR-n**; the key words MUST / MUST NOT / SHOULD / MAY are
RFC-2119. Items overlapping the system spec cross-reference [SPEC.md](SPEC.md) numbering
(REQ-n / INV-n / ERR-n). For the comparative design discussion (Spark, Ark, SuperScalar, vanilla
Mercury) see [learn/invalidation.md](learn/invalidation.md); for the long-form explainer see
[learn/invalidation-deep-dive.md](learn/invalidation-deep-dive.md); for fee/size/time pricing see
[research/invalidation-economics.md](research/invalidation-economics.md); for exit mechanics see
[learn/exits.md](learn/exits.md); for open audit findings see
[AUDIT-2026-07.md](AUDIT-2026-07.md).

## 0. Scope, terminology, relationship to SPEC.md

**Scope.** Invalidation of stale ownership states for both flat coins (backup-timelock ladder) and
off-chain sub-coins (structural exit branches), the SE-side hard refusals that back them, the
receiver-side validation duties, and the deadline/watchtower model. LN swaps, RGB token semantics
and deposit-token economics are out of scope (see SPEC.md §7–8).

| Term | Meaning |
|---|---|
| **coin** | A P2TR UTXO locked to a 2-of-2 MuSig2 aggregate of owner share + SE share. A *flat* coin's funding output is on-chain. |
| **sub-coin** | A coin whose funding tx is a pre-signed, **un-broadcast** split/combine tx. Its funds exist off-chain until materialized. |
| **structural node** | A coin consumed by a split/combine (the "parent"). It must become *terminal* at the SE. |
| **exit branch** | The ordered list (root-first) of un-broadcast structural txs from an on-chain outpoint down to a sub-coin's funding output. Built and stored under `branch-<statechain_id>` (clients/libs/rust-sdk/src/transfer.rs:477-510; `insert_backup_txs` under `branch-<id>` at :504-510). |
| **backup ladder** | The sequence of pre-signed backup txs for one coin, one per ownership hop, with strictly decrementing absolute locktimes. |
| **stale state** | Any backup tx or co-signed spend held by a *previous* owner of a coin (or ancestor). |
| **terminal** | The SE's public predicate: `sig_budget` set ∧ `finalized >= sig_budget` (IVL-INV-8) — the SE will never co-sign the node again. Publicly checkable via `GET /statechain/spend_budget` (server/src/endpoints/lightning_latch.rs:291-297). |
| **single-use refusal** | The equivalent hard refusal for `single_use` coins: ≥ 1 finalized signature ⇒ 410 at `sign/first` (IVL-ERR-1). NOT reflected in the public `terminal` field — a spent single-use coin reports `terminal = false` unless a budget was also set — so IVL-REQ-12's ancestor check only ever succeeds against the budget form. This is why IVL-REQ-7 sets an explicit budget before every split. |
| **materialization** | Broadcasting an exit branch so a sub-coin's funding output becomes an on-chain UTXO (turning it into a flat coin). |

**Relationship to SPEC.md.** SPEC.md defines the whole system; this document *refines* its
invalidation cluster (INV-4/5, INV-18..21, INV-23..25, REQ-5..7, REQ-16..18, REQ-24/25,
ERR-1/2/3/7/12) into a self-contained normative spec with exact formulas, parameter constraints
and the precise exactness domain of the exit deadline. Where the two disagree, this document is
in error and MUST be corrected — SPEC.md items are anchored to the same code.

## 1. Protocol parameters

The SE is configured with two ladder parameters (server/src/server_config.rs:42-44):

| Server config | Client name | Default profile | Deployed profile (server/Settings.toml:2-3) |
|---|---|---|---|
| `lockheight_init` | `initlock` | 10000 blocks | 1000 blocks |
| `lh_decrement` | `interval` | 100 blocks | 10 blocks |

Defaults are set in server/src/server_config.rs:69-70. Clients obtain the live values from
`GET /info/config`, which maps `lockheight_init → initlock` and `lh_decrement → interval`
(server/src/endpoints/utils.rs:85-86; client type `InfoConfig`, lib/src/utils.rs:19-23).

- **IVL-REQ-1** Clients MUST obtain `initlock`/`interval` from `GET /info/config` and MUST NOT
  hard-code them. Every locktime computation and every receiver-side ladder validation uses the
  fetched values (clients/libs/rust/src/deposit.rs:50-69; transfer receiver, §5). A watchtower
  that must operate offline MUST cache `initlock` (and the deposit height) while online
  (wallet.rs:634-639).
- **IVL-REQ-2 (parameter constraints)** The SE operator MUST configure `1 <= interval <= initlock`.
  `interval` SHOULD divide `initlock` exactly; if it does not, the unusable remainder
  `initlock mod interval` is dead ladder headroom. Derived quantities:
  - **ladder capacity** = `floor(initlock / interval)` ownership hops before the ladder floor
    reaches the co-sign tip (100 hops in both profiles);
  - **exclusive-window width** = `interval` blocks (§2, IVL-INV-3).
- **IVL-REQ-3 (sizing trade-offs)** `initlock` bounds the *worst-case unilateral-exit wait* and the
  current owner's *maximum stale-backup exposure horizon*; at ~144 blocks/day the deployed profile
  gives ≈ 6.9 days, the default profile ≈ 69 days. `interval` is the current owner's exclusive
  head-start over the most recent previous owner. Operators SHOULD choose `interval` no smaller
  than the reorg-plus-reaction margin they expect of watchtowers (a few blocks minimum), and
  SHOULD choose `initlock` large enough that `capacity = initlock/interval` exceeds the expected
  per-coin transfer count within the coin's intended lifetime, but small enough that a forced exit
  wait (`<= initlock` blocks) is acceptable. Raising capacity by shrinking `interval` weakens the
  race margin; raising it by growing `initlock` lengthens exit waits. There is no in-protocol
  renegotiation of either parameter for an existing coin (§9).

## 2. The backup ladder (flat coins)

Ground truth: `calculate_block_height` (lib/src/transaction.rs:148-163). For backup tx number
`qt` (qt = 0 is the first backup, co-signed when the deposit is first seen; each transfer
increments qt):

```
locktime(qt=0) = H_anchor + initlock
locktime(qt>0) = locktime(qt=0) − interval · qt
```

so the k-th hop's backup matures at **L_k = H_anchor + initlock − interval·k**, where `H_anchor`
is the chain tip at first co-sign (deposit detection for flat coins; split height for sub-coins,
§9). Withdrawals bypass the ladder via `get_locktime_for_withdrawal_transaction` (no future
locktime). Enforcement locus: the SE co-signs blindly (it sees a challenge, never the
transaction) and does not verify ladder locktimes — IVL-INV-1/2 hold because the honest client
constructs them (`calculate_block_height`) and receiver-side validation rejects violations
(IVL-REQ-4, §5); no SE-side check backs them.

- **IVL-INV-1 (formula)** Every ladder backup's nLockTime satisfies L_k = H_anchor + initlock −
  interval·k exactly. Verifying test: receiver-side check below; unit `invalidation_model`
  (clients/libs/rust-sdk/src/invalidation_model.rs).
- **IVL-INV-2 (strict monotone decrement)** L_k < L_{k−1} for every hop; the *current* owner holds
  the strictly lowest locktime of all co-signed backups for the coin (= SPEC INV-5). Consequence:
  in an honest broadcast race the newest state is valid first.
- **IVL-INV-3 (exclusive window)** The current owner after k hops has the exclusive broadcast
  window `[L_k, L_{k−1})`, exactly `interval` blocks wide. Before L_k *nobody* can broadcast any
  ladder state — the only spend path is SE co-signing. From L_{k−1} onward the ladder degrades to
  a first-seen broadcast race (proven: sdk13 ladder defeat of a stale clawback, sdk14 watchtower
  race under fee pressure).
- **IVL-INV-4 (capacity)** A coin supports at most `floor(initlock / interval)` transfers before
  L_k reaches H_anchor; the SDK-independent wall-clock bound is that the current owner's backup
  matures `(initlock − k·interval)` blocks after H_anchor regardless of transfer activity.
- **IVL-REQ-4 (receiver decrement validation — refines SPEC REQ-16)** A receiver MUST validate,
  before accepting a coin, that consecutive backups decrement by *exactly* `interval`
  (lib/src/transfer/receiver.rs:330-338: `prev_lock_time − current_lock_time != interval` ⇒
  `SignatureSchemeValidationError`), that **every** conveyed backup's locktime satisfies
  `tip < nLockTime <= tip + initlock` (checked per-backup inside the validation loop,
  receiver.rs:318 → bounds in receiver.rs:455-467; inner errors `LocktimeTooLow`/`LocktimeTooHigh`,
  surfaced as `SignatureSchemeValidationError`),
  that at least one backup exists (receiver.rs:347-349), and that the SE-reported signature count
  matches the conveyed ladder: `statechain_info.num_sigs == backup_transactions.len()`
  (clients/libs/rust/src/transfer_receiver.rs:485-486, error `num_sigs is not correct`). Failure
  of any check MUST abort the claim; the coin is never booked.
- **IVL-INV-15 (receive-time freshness; capacity exhaustion is receiver-rejected)** Because the
  IVL-REQ-4 bounds apply to *every* conveyed backup, two guarantees follow: (1) a receiver can
  never be handed a coin any of whose ladder states has already matured — no end-of-life coin
  (any `nLockTime <= tip`) is claimable; (2) ladder-capacity exhaustion (IVL-INV-4) is enforced at
  claim time: at hop `capacity = floor(initlock/interval)` the conveyed locktime would be
  `<= tip`, so the hand-off is rejected at claim (`LocktimeTooLow`) and the coin MUST instead be
  cooperatively withdrawn (and, if desired, re-deposited — §9).

## 3. Structural invalidation (splits, combines, spend budget)

Off-chain split/combine creates sub-coins whose funding is un-broadcast. Stale-state safety here
does NOT come from the ladder — it comes from making the consumed parent *terminal* and the branch
*unconditionally broadcastable*.

*Combine status.* Combine is specified here for completeness: the locktime-free rule is enforced
in lib for the combine path too (audit [12] fix) and a colored combine exists at lib level
(`create_colored_combine_tx`, clients/libs/rust/src/rgb.rs:330, exercised by rgb02/05/08), but
plain-BTC combine is NOT currently exposed as an SDK operation. Every "split/combine" requirement
below binds combine if and when it is wired; today only split ships end-to-end.

- **IVL-INV-5 (locktime-free branch — = SPEC INV-4)** Every non-withdrawal structural tx MUST
  carry `nLockTime = 0` (lib/src/transaction.rs:389-394; audit H5 fix, combine mirrored per audit
  [12]). Rationale: parent backups are anchored at the *deposit* tip while a tip-relative split
  locktime would be anchored at the *split* tip, which can invert the exit race; height 0 always
  wins against any locktimed ancestor backup *if broadcast in time* (§6).
- **IVL-INV-6 (value conservation — = SPEC INV-25)** Each branch tx satisfies
  Σ outputs <= Σ inputs; the receiver enforces this (clients/libs/rust/src/transfer_receiver.rs:897-914)
  because `tx.verify` alone does not check the fee rule, and a value-creating tx is
  consensus-invalid, i.e. an unexitable branch.
- **IVL-REQ-5 (spend-budget endpoint)** `POST /statechain/spend_budget`
  (server/src/endpoints/lightning_latch.rs:251-274) MUST accept `{statechain_id, auth_sig,
  remaining}` with `remaining ∈ {0, 1}` (else 400 `remaining must be 0 or 1`), MUST verify the
  owner auth signature (else 403), and sets
  `sig_budget = min(existing_budget, finalized + remaining)` — the monotonic clamp of IVL-INV-7
  (server/src/database/deposit.rs:181-192, min-clamp at :189-190), never the unclamped
  `finalized + remaining`.
- **IVL-INV-7 (monotonic tightening — = SPEC INV-24)** A budget may only TIGHTEN:
  `new_budget = min(existing_budget, finalized + remaining)`
  (server/src/database/deposit.rs:181-192, min-clamp at :189-190). Without the clamp, a re-issued relative budget after a
  terminal spend would re-open the node for a conflicting second spend (INV-19 fork).
- **IVL-INV-8 (terminal predicate)** `terminal := sig_budget set ∧ finalized >= sig_budget`, where
  `finalized = COUNT(statechain_signature_data WHERE challenge IS NOT NULL)`
  (server/src/database/deposit.rs:146-152; endpoint lightning_latch.rs:291). A terminal node is
  refused at `sign/first` with 410 (server/src/endpoints/sign.rs:96-103).
- **IVL-REQ-6 (fail-closed)** Every read of single-use/budget/epoch state on the enforcement and
  query paths MUST fail CLOSED on a DB error: `sign/first` returns 503 and refuses to co-sign
  (sign.rs:63-82, audit [1]); `GET /statechain/spend_budget/<id>` returns 503 rather than a
  permissive `terminal=false` (lightning_latch.rs:283-290); the budget write returns 503 rather
  than reporting success on a failed update (lightning_latch.rs:262-271).
- **IVL-REQ-7 (SDK ordering — = SPEC REQ-18)** The SDK MUST set the parent's spend budget to
  `remaining = 1` IMMEDIATELY BEFORE requesting the split/combine co-signature
  (clients/libs/rust-sdk/src/transfer.rs:346-355). The structural co-sign then *consumes* the
  budget, leaving the parent terminal. No later transfer/withdraw/backup of the parent can be
  signed — even by a malicious sender who kept its keys.
- **IVL-REQ-8 (single-use deposits — RGB tree roots only)** Deposit roots of the RGB off-chain
  DAG flows SHOULD be opened single-use via the dedicated API
  (`get_deposit_bitcoin_address_single_use`, clients/libs/rust/src/deposit.rs:10-14; exercised by
  rgb04): `sign/first` refuses a second spend once `finalized >= 1` with 410
  `single-use coin already spent` (sign.rs:83-88; = SPEC REQ-5/ERR-1). Plain-BTC SDK trees are
  **NOT single-use anywhere**: `split_coin` opens both sub-coins with the plain deposit API
  (clients/libs/rust-sdk/src/transfer.rs:316-330 → deposit.rs:6-8, `single_use = false`), as does
  `UtexoWallet::get_deposit_address` (wallet.rs:335) — necessarily so, since a single-use node
  gets exactly ONE lifetime co-signature (sign.rs:83-88), which would forbid the fresh sub-ladder
  tx1 plus later transfers this spec requires of sub-coins (§9). For plain-BTC trees the sole
  SE-side structural guard is the spend budget (IVL-REQ-7), backed by receiver checks
  (IVL-REQ-12). The cost of single-use MUST be disclosed wherever it is offered: a single-use coin skips
  its deposit backup entirely (no tx1 is created, clients/libs/rust/src/coin_status.rs:75-79), so
  the depositor has NO pre-signed unilateral exit between deposit confirmation and the first
  (terminal) spend — a deliberate, narrow exception to SPEC REQ-2 that callers of the single-use
  API accept because the tree branch becomes the exit.
- **IVL-INV-9 (public auditability)** `GET /statechain/spend_budget/<id>` is unauthenticated and
  returns `{sig_budget, finalized, terminal}` (lightning_latch.rs:278-298). Anyone — in particular
  any receiver, §5 — can verify a node is terminal, and any SE co-signature issued *beyond* a
  published budget is publicly attributable SE misbehaviour.

## 4. Epoch deadlines

- **IVL-REQ-9 (epoch semantics — = SPEC REQ-6, INV-21)** A coin MAY carry an `epoch_deadline` in
  **unix seconds** (clients/libs/rust/src/deposit.rs:16-21). The SE MUST refuse `sign/first` once
  its own clock reaches the deadline (`now >= deadline` ⇒ 410, sign.rs:110-125) — i.e. epochs
  bound *new state creation only*. Unilateral exit never contacts the SE, so exit remains live
  forever after the epoch; funds are never stuck (verified: rgb07). Coins with
  `epoch_deadline = NULL` are co-signable indefinitely. The epoch clock is the SE's wall clock,
  not block height; clients SHOULD keep margin for clock skew.

## 5. Receiver obligations on accepting a coin or sub-coin

A receiver that skips any obligation below MAY accept a state its counterparty can still
invalidate. Each failure aborts the claim before the coin is booked.

- **IVL-REQ-10 (transfer binding + ladder)** The receiver MUST decrypt and verify the transfer
  message against its own auth key, verify every backup's transaction signature and blinded-MuSig2
  scheme against the SE's statechain info, and run the full ladder validation of IVL-REQ-4
  including the `num_sigs` cross-check (lib/src/transfer/receiver.rs:270-352;
  clients/libs/rust/src/transfer_receiver.rs:485-486). Error: `SignatureSchemeValidationError` /
  `num_sigs is not correct`.
- **IVL-REQ-11 (tx0 / branch validation — = SPEC REQ-16 + INV-4/25)** For the funding side the
  receiver MUST either resolve tx0 on-chain, or, for a branch-carried sub-coin, run
  `validate_branch` (clients/libs/rust/src/transfer_receiver.rs:796-916), which MUST enforce ALL
  of:
  0. the branch is a **TREE** — no outpoint consumed by more than one branch input, whether across
     two txs or as a duplicate input within one combine tx (`reject_non_tree_branch`,
     transfer_receiver.rs:394-407, called first at :823; error `exit branch consumes outpoint ...
     more than once — non-tree branch / internal double-spend`). A repeated prevout is script-valid
     per input yet un-broadcastable as a whole (the two spends are mutually exclusive on-chain), so
     the coin it funds would be unexitable (fund stranding). Unit-tested (`non_tree_branch_is_rejected`);
  1. non-empty branch, every tx fully signed and each spending its predecessor's outputs (linkage /
     prevout collection, :826-871);
  2. the root input(s) on-chain, **unspent and confirmed** (:852-868; errors `exit-branch root
     output is spent` / `exit-branch root is not confirmed`), where *confirmed* means
     `confirmations >= confirmation_target` from the receiver's own client config
     (`verify_tx0_output_is_unspent_and_confirmed`, transfer_receiver.rs:918-951) — depth is a
     receiver-local policy, not a protocol constant. A root whose electrum height is 0
     (mempool/0-conf) is treated as UNCONFIRMED: the confirmations math is guarded by `height > 0`
     (:940), so an RBF-able mempool root (or any of N combine roots) is rejected rather than
     mis-booked as confirmed — without the guard `blockheight - 0 + 1` trivially clears
     confirmation_target;
  3. **every branch tx's locktime already satisfied**: `nLockTime <= tip` (:879-889, audit [11];
     error `...unreached locktime ... violates INV-4`) — otherwise the receiver would book a coin
     it cannot exit while the sender's matured backup sweeps the root;
  4. per-hop value conservation Σout <= Σin (:897-914, IVL-INV-6);
  5. full consensus script verification of every input against its prevout (:912-913).
- **IVL-REQ-12 (terminal ancestors — = SPEC REQ-17/INV-20, ERR-7)** For a branch-carried sub-coin
  the receiver MUST require `n_parents >= max(required_terminal_ancestors, 1)` conveyed structural
  ancestors, where `required_terminal_ancestors = Σ tx.input.len()` over all branch txs
  (`required_terminal_ancestors`, transfer_receiver.rs:409-417; consumed at :495) — one terminal
  ancestor per structural **INPUT**, not per hop, so a multi-input combine forces all N inputs
  named + terminal (for a linear split chain each tx has one input, so this equals the hop count).
  The predicate is `terminal_parents_sufficient(n_parents, required) = n_parents >= required.max(1)`
  (transfer_receiver.rs:377-379); the shortfall error is `off-chain sub-coin names N terminal
  ancestor(s) but its exit branch consumes M structural input(s) — refusing (the sender may be
  hiding a non-terminal, double-spendable ancestor; a combine of N carriers needs all N named +
  terminal)` (transfer_receiver.rs:429-433). Then, **for each ancestor**, query
  `GET /statechain/spend_budget/<id>` and require `terminal == true` (transfer_receiver.rs:436-455;
  error `structural parent <id> is NOT terminal at the SE`). A non-2xx budget query MUST also
  reject (`could not query terminal state of parent ...`) — never assume terminal on silence.
  *Caveat (SPEC §14):* ancestor ids are not cryptographically bound to branch outpoints (the SE is
  blind); the count check defeats *omission*, not *substitution* — see §11.

## 6. Exit and deadlines

### 6.1 Unilateral exit algorithm

- **IVL-REQ-13 (branch first, conflicts are hard errors)** `unilateral_exit`
  (clients/libs/rust-sdk/src/wallet.rs:717) MUST broadcast the stored exit branch root-first
  before touching the backup (`broadcast_branch_if_any`, wallet.rs:571-613). A mempool conflict on
  a branch tx means a competing spend of the same input — the exit is being RACED — and MUST
  surface as a hard error plus `WalletEvent::ExitBranchConflict` (wallet.rs:596-601, audit H1
  fix), never be swallowed as an idempotent success. Only idempotent-rebroadcast errors are
  tolerated — `is_idempotent_rebroadcast` (wallet.rs:1028-1038): already in block chain / already
  in utxo set / txn-already-known / already in mempool / already have transaction — checked at
  :606. Note: a *sibling* sub-coin owner (e.g. the
  split's change-holder) broadcasting the shared branch is benign, not a race — the branch txs are
  byte-identical, so the rebroadcast hits exactly this tolerated already-known case;
  `ExitBranchConflict` requires a *different* spend of the same input.
- **IVL-REQ-14 (locktime gate, re-callable — = SPEC REQ-25)** The latest backup is broadcast only
  if `locktime <= tip`; otherwise the call MUST return
  `ExitStatus{complete: false, wait_blocks: locktime − tip}` and MUST be safely re-callable — the
  branch stays broadcast either way (wallet.rs:787-793).
- **IVL-REQ-15 (missing branch is explicit)** If a coin has no stored branch AND its funding txid
  is not on-chain, exit MUST fail with an explicit "restore the recovery bundle (`branch-*` rows)"
  error rather than broadcast an unfunded backup (wallet.rs:759-773; audit [20]).

**Cooperative materialization.** `withdraw` broadcasts the branch first, then one SE-co-signed
withdrawal tx per coin with no locktime wait (wallet.rs:473-522, branch materialization at :509).

### 6.2 The exit deadline and its exactness domain

`estimate_exit_cost` (wallet.rs:618) reports, for branch-carrying coins,
`exit_deadline_block = H_deposit_confirmation + initlock` — the branch root's on-chain deposit
confirmation height plus initlock (`deposit_anchored_exit_deadline`, wallet.rs:685-710; audit [10]
fix). This is the height by which the (locktime-free) branch MUST be on-chain to beat ancestor
stale backups.

- **IVL-INV-10 (exactness domain — state it precisely)** The true earliest hostile maturity is
  `min` over all ancestor backup locktimes. For an ancestor transferred `k` times before the split
  the splitter's own retained backup matures at `H_cosign + initlock − k·interval`
  (IVL-INV-1). Therefore the implemented deadline is:
  - **exact** for a parent that was **never transferred** before splitting *and* carries a
    deposit backup (every non-single-use parent, i.e. the plain-BTC SDK case) — up to the deposit
    co-sign→confirmation gap: tx1 is co-signed when the funding is first *seen*
    (coin_status.rs:59-78), so `H_cosign <= H_confirm` and the reported deadline errs **late** by
    `H_confirm − H_cosign`, the unsafe direction; the gap is unbounded if the funding lingers
    unconfirmed under fee pressure. For a **single-use root** (IVL-REQ-8) no tx1 exists at all, so
    no ancestor backup threatens the branch at `k = 0` and the reported deadline is purely
    conservative (safe direction);
  - **too late by `k·interval` blocks** for a parent transferred `k` times pre-split. `k` is NOT
    observable by the receiver from local state (the SE conveys no ancestor locktimes). This
    conveyed-locktimes residual is the remaining half of audit item **[17]** (compounding [10]).
    The other half — a watchtower pass acting on the deadline — landed in remediation batch 5:
    `UtexoWallet::auto_exit_due(margin_blocks)` force-exits any owned off-chain sub-coin within
    `margin_blocks` of its (deposit-anchored) deadline and emits
    `WalletEvent::ExitDeadlineApproaching` (wallet.rs, commit 07aa335). Nothing in this document
    may be read as claiming the reported deadline is exact for pre-transferred ancestors; the
    margin argument exists precisely to absorb that error.
- **IVL-REQ-16 (normative watchtower rule)** Because `k` is unobservable, a receiver/watchtower
  that may go offline MUST do one of:
  1. **eager materialization** — broadcast the exit branch immediately on receipt (the branch is
     locktime-free, IVL-INV-5; an online receiver is therefore always safe); or
  2. **conservative margin** — treat the effective deadline as
     `exit_deadline_block − M·interval` where `M` is an assumed upper bound on any ancestor's
     pre-split transfer count; since no protocol message bounds `M`, an offline-capable watchtower
     that cannot justify a bound MUST fall back to option 1. The shipped implementation of this
     rule is `auto_exit_due(margin_blocks)` with `margin_blocks >= M·interval`, called on an
     interval (e.g. alongside `claim()`).
  A watchtower MUST key its force-exit decision on the deadline, never on `wait_blocks`
  (`wait_blocks` is when the exit *completes*, not when it must *start* —
  clients/libs/rust-sdk/src/types.rs:119-129). It MUST cache `initlock` and the deposit height
  while online and MUST NOT fall back to the pre-[10] formula `leaf_locktime + interval`
  (wallet.rs:634-639).

## 7. Error semantics

| ID | Trigger | Behaviour (verified) | SPEC |
|---|---|---|---|
| IVL-ERR-1 | second spend of a single-use coin | HTTP 410 `single-use coin already spent (SE refuses a second spend)` (sign.rs:83-88) | ERR-1 |
| IVL-ERR-2 | `sign/first` past `epoch_deadline` | HTTP 410 `epoch deadline passed (now=… >= deadline=…)` (sign.rs:114-124) | ERR-2 |
| IVL-ERR-3 | `finalized >= sig_budget` | HTTP 410 `spend budget exhausted (terminal node: SE refuses further co-signatures)` (sign.rs:96-103) | ERR-3 |
| IVL-ERR-4 | DB error reading single-use/budget/epoch state, or budget read/write failure | HTTP 503 fail-closed, refuse to co-sign / report / write (sign.rs:63-82; lightning_latch.rs:262-290) | — |
| IVL-ERR-5 | `remaining ∉ {0,1}` on POST spend_budget; bad auth sig | HTTP 400 `remaining must be 0 or 1`; HTTP 403 (lightning_latch.rs:254-261) | — |
| IVL-ERR-6 | too-few / non-terminal / unqueryable ancestor at claim | client errors: shortfall `off-chain sub-coin names N terminal ancestor(s) but its exit branch consumes M structural input(s)` (transfer_receiver.rs:429-433); `structural parent … is NOT terminal at the SE` / `could not query terminal state…` (transfer_receiver.rs:436-455) | ERR-7 |
| IVL-ERR-7 | branch fails receiver validation (non-tree/internal double-spend, linkage, spent/unconfirmed root, unreached locktime, value creation, script) | client errors per check (transfer_receiver.rs:796-916), claim aborted | INV-4/25 |
| IVL-ERR-8 | exit branch conflicts in mempool; ladder locktime unreached; missing branch | hard error + `ExitBranchConflict` event (wallet.rs:551-558); `ExitStatus{complete:false, wait_blocks}` (wallet.rs:741-749); explicit restore error (wallet.rs:714-729) | REQ-25 |
| IVL-ERR-9 | server-nonce reuse over a different message at `sign/second` | HTTP 409, single-active-state (SPEC INV-23; sdk12 Part C) | ERR-12 |

## 8. Security analysis (normative invariants)

**IVL-INV-11 (four defence layers).** Old state is invalidated by the conjunction of:
(1) the **timelock ladder** ordering honest exit races (§2); (2) **SE hard refusal** of
conflicting/expired state — single-active-state nonce binding (SPEC INV-23; the enclave's atomic
secnonce consume, lockbox/src/db_manager.cpp:294-297, is the authoritative guard; the advisory
per-coin lock in sign.rs:19-23 is fail-open defence-in-depth), spend budget, single-use, epoch
(§3–4); (3) **receiver-side independent validation** (§5); (4) **deadline/watchtower discipline**
bounding exposure (§6). No single layer is sufficient; each MUST hold as specified.

Attacker capability matrix (what each class can and cannot achieve):

| Attacker | Can | Cannot | Evidence |
|---|---|---|---|
| Previous owner (flat coin) | Broadcast their stale backup once `L_{k'}` matures — a race the current owner wins with an `interval`-block head start | Broadcast before their locktime; get the SE to re-sign old state | sdk13, sdk14 |
| Malicious sender of a sub-coin | Attempt branch-root front-running (detected as `ExitBranchConflict`); try to convey a locktimed/value-creating/short-ancestor branch (all rejected at claim) | Double-spend a terminal parent at the SE (budget consumed pre-co-sign, IVL-REQ-7); re-open a budget (IVL-INV-7) | sdk08, sdk10, rgb04 |
| Malicious SE alone | Refuse service (owner exits unilaterally, REQ-2/24); co-sign a fresh conflicting state → carries a *current* locktime, so no ladder advantage → plain first-seen race | Move funds without the owner's key (SPEC REQ-1); covertly violate a published budget — `GET spend_budget` is public (IVL-INV-9) | sdk15 |
| SE + old owner collusion | Fresh double-sign = the same plain race; on structural nodes the extra signature is publicly attributable via the terminal audit | Win retroactively via the ladder; hide the misbehaviour from auditors | sdk15 |
| Observer replaying an owner auth sig (audit **[15]**, closed UPDATE 3) | Historically: `set_spend_budget remaining=0` → brick to unilateral-exit-only, or destroy SE co-sign state via `withdraw/complete`. Both now require a single-use, endpoint-bound challenge (`GET /auth/challenge/<sid>`, sig over `sha256(nonce‖endpoint)`, atomically consumed) — a captured signature can no longer be replayed or redirected | Steal funds; forge a transfer; replay onto the two irreversible endpoints | AUDIT-2026-07 [15] |

**IVL-INV-12 (trust floor).** The residual trust assumption is exactly: *the SE honestly refuses
conflicting/expired state*. If it does not, invalidation degrades to a fair on-chain broadcast
race with no timelock advantage to either side (fresh double-signs carry current-height
locktimes) — the same floor as vanilla Mercury and full-collusion Spark. sdk15 demonstrates this
floor empirically.

## 9. Lifetime and renewal

**IVL-INV-13 (no *off-chain* ladder renewal; on-chain refresh exists).** There is NO mechanism to
reset or extend an existing coin's ladder purely off-chain (contrast Spark's `renew_leaf`,
[research/protocol-notes.md](research/protocol-notes.md)) — a coin's off-chain lifetime is bounded
at deposit time and only an on-chain touch resets it. The lifetime-extension options and their
exact effects:

| Option | On-chain cost | Effect on deadlines |
|---|---|---|
| **Refresh (re-anchor)** — `UtexoWallet::refresh` / `refresh_sponsored` (`clients/libs/rust-sdk/src/refresh.rs`) | **1 tx + 1 deposit token** (SE-co-signed spend of the coin's outpoint → a fresh aggregate; the fresh `get_deposit_address` at refresh.rs:158 consumes a pre-paid deposit token via `take_token`, wallet.rs:334) | Brand-new coin: new `H_anchor`, full `initlock` horizon and full ladder capacity; the old outpoint is spent so all old backups die. Fee **user-paid** (refreshed coin = `amount − fee`) or **operator-paid** (off-chain rebate preserves the user's total; the SE holds no funds and cannot co-fund the tx — it stays a single-input spend). Cooperative (needs the SE); verified by `SDK_E2E=30`. |
| Cooperative withdraw + re-deposit | 2 txs (withdraw + new funding) | Same brand-new-coin effect as refresh, but two on-chain txs — refresh supersedes it. |
| Materialize the branch (sub-coins) | branch txs (pre-paid reserves, §6) | Sub-coin becomes a *flat* coin; its OWN fresh ladder (anchored `H_split + initlock`, see below) now solely governs — ancestor deadlines vanish. |
| Keep transacting before deadlines | none | Does NOT extend anything: each hop *lowers* the leaf locktime by `interval`; the root deadline (§6.2) is untouched. |

**Fresh sub-ladders.** Each split output receives a fresh first backup via `create_tx1` with
`qt_backup_tx = 0` (call at clients/libs/rust-sdk/src/transfer.rs:465 →
clients/libs/rust/src/deposit.rs:46-70, which passes 0 as `new_transaction`'s qt argument at
deposit.rs:62; the `tx_n = 1` parameter only labels the stored row). Hence sub-coin ladder anchor = `H_split + initlock`:
tree *depth* does not consume ladder capacity — each leaf gets its own
`floor(initlock/interval)`-hop budget. Note the UX consequence: a freshly received sub-coin's
unilateral exit waits ≈ `initlock` blocks (≈ 6.9 days on the deployed profile) minus hops already
taken on its own ladder.

**IVL-INV-14 (tree lifetime bound).** A tree's off-chain lifetime is bounded by the earliest
ancestor stale-backup maturity (§6.2) — at latest `H_deposit + initlock`, earlier by `k·interval`
per pre-split ancestor transfer. Self-splitting extends only the *leaf* ladder, never the *root*
deadline. **Epoch interaction:** an `epoch_deadline` (§4) additionally bounds *new state creation*
in wall-clock time; it neither extends nor shortens the exit-race deadlines, and exit remains
possible after it forever.

## 10. Traceability

| Item(s) | Verifying test(s) |
|---|---|
| IVL-REQ-1 | not unit-testable (the unit model hardcodes both profiles as fixtures by design, invalidation_model.rs:37-41); verified by the config fetch on every deposit (clients/libs/rust/src/deposit.rs:50-69), exercised by every SDK E2E, plus receiver-side ladder validation against the fetched values (§5) |
| IVL-REQ-2 | `unit::invalidation_model` (clients/libs/rust-sdk/src/invalidation_model.rs) — ladder math, capacity, window width |
| IVL-INV-1..4 | SDK_E2E=27 `sdk27_invalidation_time` — ladder over time, maturity boundary; `unit::invalidation_model` |
| IVL-REQ-4, IVL-INV-15, IVL-REQ-10 | sdk04/sdk17 (multi-hop transfer + claim), upstream Mercury receiver suite; mercurylib receiver checks (lib/src/transfer/receiver.rs:270-352, locktime bounds :455-467) |
| IVL-INV-5, IVL-INV-6, IVL-REQ-11 | sdk12 (honest branch accepted), rgb03/rgb06 (DAG depth 2–3); chaos sdk22 (oracle asserts value conservation INV-1/13/25 and cheat refusal INV-5/18/19, chaos22_oracle.rs:5-12; INV-4 exercised implicitly — the cheats lose to the locktime-0 branch); `unit` tree check `non_tree_branch_is_rejected` (clients/libs/rust/src/transfer_receiver.rs:1073-1106) |
| IVL-REQ-5..7, IVL-INV-7..9 | sdk08 (terminal node), `unit::types::terminal_predicate` (clients/libs/rust-sdk/src/types.rs:183-190), `unit::invalidation_model::terminal_predicate_matrix` (invalidation_model.rs:379-394, incl. the audit-[15] `(Some(0), 0)` brick row), rgb04 (single-use) |
| IVL-REQ-8 | rgb04 |
| IVL-REQ-9 | rgb07 (epoch, RGB path); SDK_E2E=27 `sdk27_invalidation_time` part d (epoch on the plain-sats path) |
| IVL-REQ-12 | sdk10 (terminal-parent verify), `unit` terminal-parents count + Σ-inputs (clients/libs/rust/src/transfer_receiver.rs:1040-1144, incl. `required_ancestors_counts_inputs_not_hops` :1046-1068 — the Σ-inputs combine rule — and the `terminal_parents_sufficient` cases :1110-1144) |
| IVL-REQ-13..15 | sdk07 (exit + cost), sdk12 (branch exit), sdk13 (stale clawback defeated), audit [20] regression |
| IVL-INV-10, IVL-REQ-16 | sdk14 (watchtower race + deadline/fee quantification); SDK_E2E=27 `sdk27_invalidation_time` part c — deadline-gap arithmetic on a k=2 pre-transferred parent; `unit::invalidation_model` |
| IVL-ERR-1..9 | sdk08/rgb04 (410s), rgb07 + sdk27 part d (410 epoch, IVL-ERR-2), sdk12 Part C (409 nonce reuse), sdk13/sdk14 (race outcomes), receiver unit tests |
| IVL-INV-11, IVL-INV-12 | sdk13, sdk14, sdk15 (malicious-SE trust floor); chaos sdk22 |
| IVL-INV-13, IVL-INV-14, exit-cost scaling | SDK_E2E=26 `sdk26_invalidation_scale` — depth scaling + measured exit costs; sdk07 (267 vB depth-1: 155 vB branch + 112 vB backup, types.rs:158-200) |
| IVL-INV-13 (refresh re-anchor, both fee modes) | SDK_E2E=30 `sdk30_refresh` — user-pays (fee from coin) + operator-pays (off-chain rebate); horizon reset (headroom 700→999≈initlock), old outpoint spent, refreshed coin spendable; `unit::refresh::refresh_fee_and_amount_arithmetic` |

E2E dispatch: clients/tests/rust/src/main.rs (`SDK_E2E=N ML_NETWORK=regtest cargo run` from
clients/tests/rust); all tests above exist as of this revision.

## 11. Known limitations & open items

Each item below qualifies the invalidation guarantees above; none may be omitted when citing this
spec.

1. **[17] (HALF-CLOSED, MEDIUM) — auto-exit landed; conveyed ancestor locktimes still absent.**
   Remediation batch 5 (commit 07aa335) shipped `UtexoWallet::auto_exit_due(margin_blocks)` — a
   watchtower pass that force-exits any owned off-chain sub-coin within `margin_blocks` of its
   deposit-anchored deadline and emits `WalletEvent::ExitDeadlineApproaching`. What remains open:
   the receiver still cannot compute the TRUE min-ancestor deadline locally (transfer messages
   convey no ancestor locktimes), so IVL-INV-10's deadline stays late by `k·interval` for
   pre-transferred ancestors and the `margin_blocks` argument must absorb that error
   (IVL-REQ-16 option 2), or the branch must be broadcast eagerly (option 1). An online receiver
   is always safe; an owner offline longer than their margin is exposed only if an ancestor was
   transferred more than `margin_blocks / interval` times before splitting.
2. **[15] (CLOSED for the irreversible ops, AUDIT UPDATE 3) — owner-auth replay griefing.** The
   static `sha256(statechain_id)` owner auth was replayable; an observer in the request path
   could *brick* a coin to unilateral-exit-only via `set_spend_budget remaining=0` (griefing/DoS,
   no theft). Fixed for the two irreversible targets: `set_spend_budget` and `withdraw/complete`
   now require a single-use, endpoint-bound SE challenge (5-minute nonce from
   `GET /auth/challenge/<sid>`, signature over `sha256(nonce‖endpoint)`, atomically consumed).
   The lower-harm `transfer`/`sign` endpoints intentionally keep the static auth (harm bounded by
   the coin protocol + enclave consume); the `fresh_auth` mechanism exists to extend coverage.
   The terminal predicate was sound throughout — the attack tightened budgets, never loosened
   them (IVL-INV-7).
3. **Blind-SE ancestor substitution (accepted, SPEC §14).** IVL-REQ-12's count check defeats
   omission of ancestors, not substitution of terminal decoys; the defence is that the receiver
   holds the fully-signed branch and can exit immediately.
4. **Fixed-fee pre-signed exits (accepted, SPEC §14).** Branch txs carry only the split-time fee
   reserve (`(parent_sats/100).clamp(300, 2000)` sats, transfer.rs:596-599), and no fee-bump
   support — neither CPFP nor RBF — exists anywhere in the SDK (SPEC §14's "no CPFP/RBF
   fee-bump"). A *manual* CPFP child on the backup is possible in principle, since its sole
   spendable output pays the owner's own P2TR key (lib/src/transaction.rs:166-173), but nothing
   constructs one. In a fee spike an exit confirms slowly; the ladder ordering (IVL-INV-2) still
   holds, but IVL-REQ-16 margins SHOULD account for confirmation latency (quantified in sdk14).
5. **u32 amount width (accepted, SPEC §14).** Coin sats are booked as `u32`; several paths cast
   with `as u32` and would silently truncate above ~42.9 BTC (e.g.
   clients/libs/rust/src/coin_status.rs:229), while the split path errors via `u32::try_from`
   (clients/libs/rust-sdk/src/transfer.rs:421). Out of intended per-coin range, not uniformly
   guarded.
6. **SGX enclave rebuild (operational blocker, AUDIT-2026-07).** The deployed lockbox binary
   embodies the authoritative nonce single-use guard (IVL-INV-11 layer 2); the SGX build path is
   not reproducible until the rebuild lands, so layer 2's deployment story is operationally
   unverified even though the deployed code is correct.

Audit status ([AUDIT-2026-07.md](AUDIT-2026-07.md), remediation UPDATE 3): every CONFIRMED
fund-loss/theft finding (all 11 HIGH, including the LN-atomicity cluster) AND all griefing/DoS
MEDIUMs (incl. [15]'s irreversible-op replay and [16] batch poisoning) are fixed and verified on
the live stack. The remaining open items are item 1 above ([17]'s conveyed-locktimes half),
[13] (enclave pubnonce binding — inert until the SGX rebuild it ships with), and [28] (LOW,
conservative-safe by design). Mainnet remains gated on the operational blockers: the SGX rebuild,
a full E2E re-run + independent re-audit, and a professional third-party audit.
