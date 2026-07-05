# Adversarial security review

An adversarial pass over the Spark-parity statechain + RGB fork, looking for behaviours the spec did
not cover — places where a message or value can be **delayed, replayed, malformed, reordered, or
changed** for an attacker's gain. Findings were produced by per-dimension finders reading the real
code + the full-suite trace logs, then each was independently verified against the actual defence
code (refute-by-default). 13 candidates verified → 10 confirmed. This document records the outcome.

> **Two reviews live here.** The first (below) hardened the SE co-signing crypto and value rules.
> The **[second review](#second-adversarial-review-2026-07-05--full-protocol-production-readiness-pass)**
> (2026-07-05) is a whole-protocol production-readiness pass — verdict **NOT production-ready**, with 3
> CRITICAL blockers. Read it before drawing any "it's done" conclusion.

## Fixed

| # | Sev | Finding | Fix | Test |
|---|-----|---------|-----|------|
| 0 | High | **MuSig2 nonce reuse.** The enclave never nulls its sealed secnonce after signing (`lockbox/src/server.cpp` `generate_partial_signature`), and `sign/second` overwrote the challenge unconditionally — so two `sign/second` calls over one server nonce yield two partial sigs over different messages → SE key-share extraction + two co-signed conflicting spends while the finalized counter shows one. | SE-side (the lockbox is internal-only): `update_signature_data_challenge` now sets the challenge atomically only when it is NULL or identical, and `sign_second` returns **409** otherwise — the second finalize never reaches the enclave. INV-23 / ERR-12. | `sdk12` Part C |
| 1,2 | High | **Finalized-count under-count / TOCTOU.** The single-use/budget guard reads `count_finalized_signatures` (rows with a non-null challenge) at `sign/first`, but the count is only mutated at `sign/second`; nonce reuse (finding 0) also made two sigs count as one. | Closed by finding 0's one-sig-per-nonce guarantee: each nonce finalizes at most once, so the count is exact. | `sdk12` Part C |
| 3 | High | **Budget could be raised.** `set_sig_budget` used a relative `count + remaining`; calling it again after a node was spent recomputed a *higher* budget and re-opened the node. | `set_sig_budget` is now monotonic — `min(existing, count+remaining)`; a budget can only tighten. INV-24. | `sdk08` |
| 5 | High | **Branch value inflation.** `validate_branch` ran `tx.verify` (scripts only, not the fee rule), so a sender could hand over a coin whose exit branch is script-valid yet creates value → un-broadcastable → receiver can never exit while the sender keeps the funds. | `validate_branch` now rejects any branch tx with `Σ outputs > Σ inputs`. INV-25. | `sdk10`/`sdk12` (honest still accepted) |
| 7 | High | **InflationRight booked as balance.** `offchain_assigned_amount` summed `Fungible \| InflationRight`, so a received inflation right inflated the receiver's spendable balance out of nothing. | Count `Fungible` only; inflation rights move solely via the mint path. INV-26. | `sdk09` |
| — | High | **Empty `terminal_parents` accepted.** `verify_terminal_parents` returned `Ok` on an empty sender-supplied list, so a sender could ship a branch-funded sub-coin naming zero ancestors and bypass INV-20 entirely. | Receiver now requires `n_parents ≥ branch_len` (≥1) — at least one terminal ancestor per branch hop. INV-20 / ERR-7. | `unit::terminal_parents_tests`, `sdk10`, `sdk12` Part B |

The receiver's terminal-ancestor check (above) is the enforcement that a sender actually set each
node's spend budget: a skipped budget → `terminal=false` → the sub-coin is rejected. Combined with
the monotonic budget (INV-24), this closes the off-chain double-spend against a malicious sender
**without** making sub-coins `single_use` (an `single_use` sub-coin was tried and reverted — its
absolute one-co-signature limit conflicts with the sub-coin's own exit-backup co-signature; the
*relative* budget mechanism is the correct tool).

## Documented (SPEC §14, not code-changed)

- **4** Batch (`transfer_many`) hand-off is not all-or-nothing.
- **6** Coin sats booked as `u32` — truncates above ~42.9 BTC/coin.
- **8** `mint_tokens` snapshot isolation is not locked against a concurrent same-asset receive.
- **9** Unilateral exit uses fixed-fee txs (no CPFP/RBF).
- Blind-SE residual: the receiver cannot cryptographically bind `terminal_parents` ids to branch
  outpoints; substitution defence relies on immediate exit.

## Dismissed on verification

- `epoch_deadline` "only checked at sign/first" — **spec-covered**: unilateral exit needs no SE, so
  funds are never stuck (INV-21).
- `decode_spark_invoice` accepts any version/expiry — **refuted**: expiry is enforced at
  `fulfill_spark_invoice` (ERR-11); version is advisory.
- Exit cost estimate "is fiction" — **refuted**: cost is measured from the real pre-signed txs
  (INV-17).

---

# Second adversarial review (2026-07-05) — full-protocol production-readiness pass

A second, broader pass over the **whole protocol and implementation** (not just code quality): per-
dimension finders read the real code, then each candidate was independently verified refute-by-default.
**18 confirmed + 2 partial.** The first review's fixes above all still hold (challenge-binding, budget
monotonicity, value conservation, `terminal_parents` count, `Fungible`-only balance).

## Verdict: NOT production-ready

Do **not** deploy to mainnet with real funds. There is one CRITICAL protocol-crypto break plus two
CRITICAL application-layer fund-loss bugs, any one of which drains real money. The SE cannot steal by
design (blind 2-of-2) **except** that the enclave nonce bug (C1) breaks exactly that guarantee. The
unilateral-exit ladder is sound in the happy path and most spec limitations are honestly disclosed.
The P0 blockers below must be fixed and re-reviewed before any mainnet exposure.

> **Remediation update (2026-07-05):** all six P0 blockers now have fixes landed on `feat/spark`
> (C1 enclave single-use secnonce + sign/first serialization; C2/C3/M3 SSP pre-payment gate; H5
> locktime-free split branch; H1 branch-conflict surfaced; H2 token-carrier exclusion; H3 recovery
> bundle + corrected docs). Two caveats remain before mainnet: the **SGX lockbox must be rebuilt and
> redeployed** for P0-1's enclave consume to take effect, and the **full E2E suite (regtest +
> lockbox + RLN) must be run** against these changes and the result **re-reviewed**. Status table:
> [PLAN.md](PLAN.md#post-review-remediation-backlog-2026-07). P1/P2 items are still open.

> No review proves the absence of vulnerabilities. Treat this as one input; before mainnet, run
> repeated independent reviews, protocol/property fuzzing (nonce lifecycle, split-locktime arithmetic,
> SSP settlement races), and a professional third-party audit.

## CRITICAL

| # | Finding | Why it's a problem / how exploited | Fix |
|---|---------|-----------------------------------|-----|
| **C1** | **Concurrent `sign/first` defeats the MuSig2 nonce-reuse fix → SE key-share leak → arbitrary theft.** `server/src/endpoints/sign.rs:98` + `lockbox/src/{db_manager.cpp:244,enclave.cpp:190,server.cpp:118}`. | The first review's fix binds *one challenge per server-nonce row*, but two concurrent `sign/first` create two pubnonce rows both resolving to the **one** sealed secnonce the enclave holds; the enclave signs with whatever secnonce is sealed, never checking it matches the caller's pubnonce. Two `sign/second` over the two rows → same secnonce, two messages → nonce reuse → the SE's private key share is recoverable → full 2-of-2 theft. The null-challenge dedup is a non-transactional read-then-generate, so it does not serialize. | Make single-use hold **at the enclave**: in `partial_signature` atomically load-and-null the sealed secnonce in one DB txn and refuse if absent. Add per-`statechain_id` serialization of `sign/first` (`SELECT … FOR UPDATE` or a partial unique index `WHERE challenge IS NULL`) as defence-in-depth. |
| **C2** | **SSP pays a Lightning invoice for a coin not addressed to it.** `clients/libs/rust-sdk/src/ssp.rs:183-185,210-216`. | `execute_pay` never checks the latched transfer's recipient is the SSP. An attacker latches *any* coin to a batch, presents *any* invoice, and the SSP pays it over LN — free money out of the SSP. Violates INV-14. | Before `send_payment`, resolve the latched `statechain_id`(s), fetch each pending transfer's `new_user_auth_public_key`, require `== SSP receiving key`; abort otherwise. Post-pay, treat `claimed_transfers==0` as a hard error. |
| **C3** | **SSP does not enforce latched coin amount ≥ invoice + fee.** `ssp.rs:184-185` (discards `quote_sats`). | Attacker latches a tiny coin and presents a large invoice; the SSP pays the full invoice for a fraction of its value. Unbounded loss per swap. | After the C2 recipient check and before `send_payment`, derive the incoming coin amount and require `≥ amt_msat/1000 + fee_sats`. |

## HIGH

| # | Finding | Why / how | Fix |
|---|---------|-----------|-----|
| **H1** | **`broadcast_branch_if_any` treats `txn-mempool-conflict` as success** (`wallet.rs:354`). | Masks an in-progress double-spend of the branch root: Bob is told he exited while a competing spend actually wins the on-chain root → false success + fund loss. | Drop `txn-mempool-conflict` from the tolerated set for branch txs; surface a distinct competing-spend event; only tolerate idempotent rebroadcast of our OWN branch txid. |
| **H2** | **BTC selection picks token-carrying coins as plain BTC** (`transfer.rs:35-48,140,254,276`; `wallet.rs:605-618`). | A routine `transfer()` of sats silently spends the RGB carrier UTXO and **destroys the allocation** — tokens permanently lost, no warning. | Persist a token-carrier flag at registration; exclude carriers from every BTC selection path; segregate carrier sats out of `available_sats`. |
| **H3** | **No backup/restore path; mnemonic-only backup is a lie** (`wallet.rs:32-33` doc; `transfer_receiver.rs`; `tokens.rs`). | Exit branches, per-sub-coin backups, `terminal_parents`, and the entire RGB stash (under a *separate* rgb.mnemonic) live only in `wallet.db`/`rgb_data_dir`, which the SE cannot re-serve after claim. A user who backs up the mnemonic and loses the disk loses all off-chain + token funds. | Ship recovery-bundle export/import (coin+backup + `branch-*`/`parents-*` + RGB stash) OR derive the RGB seed from the wallet mnemonic. Correct the doc/learn pages immediately. |
| **H4** | **`batch_timeout` (120 s) races LN settlement** (`transfer_receiver.rs:165`; `ssp.rs:188-217`). | The honest SSP pays over LN, but the 120 s batch expires before it can claim during ordinary multi-hop routing → coin reverts to the user, SSP is out the payment. No adversary needed. Breaks INV-14. | Gate `validate_batch` on the latch's own `expires_at` (25 h) for LN-latch batches; refuse `send_payment` unless ample batch time remains. |
| **H5** | **Split locktime computed vs current tip while parent backups are deposit-height-based** (`transfer.rs:317-318,539-542`; `rgb.rs:227-230`). | The "branch wins the exit race" invariant (INV-4/INV-5) is arithmetically **false** for any post-deposit split: a stale parent backup can mature before the child branch. | Anchor split/branch locktime to the parent's own ladder base, or honor INV-4 literally and set branch locktime to 0/low-constant (unconditionally broadcastable, always below any deposit-based parent backup). |
| **H6** | **Incoming token that fails RGB booking is permanently stranded** (`wallet.rs:252-266`; `tokens.rs:758`). | A transient RGB-proxy hiccup during `claim()` leaves a valid token unbooked and invisible forever; never retried. | Each `claim()` pass, scan CONFIRMED coins whose backup carries a consignment but have no booked allocation and re-run `accept_incoming_tokens` idempotently with backoff. |

## MEDIUM / LOW

- **M1** (medium) Owner auth sig is a static `sha256(statechain_id)` — no nonce/expiry/endpoint binding → replayable across every owner endpoint incl. irreversible `withdraw`/`complete`/`set_spend_budget`.
- **M2** (medium) `create_tx_out` subtracts fee with unchecked `u64` and no dust floor → a small sub-coin's backup can be below-dust or underflow (stuck funds).
- **M3** (medium) `unlock_by_preimage` returns `Success` even when it unlocks zero coins (no-op batch_id) → masks failed swaps.
- **L1** (low) `sign/first` NULL-challenge guard is a non-atomic read-then-generate → concurrent `sign/first` can clobber the enclave secnonce and strand an in-flight session (self/replay DoS). (Same root cause as C1.)
- **L2** (low) `validate_signature` / `get_auth_key_by_statechain_id` panic on attacker-controlled malformed input (unwrap on `from_str`/`from_slice`/`RowNotFound`) → per-request DoS + statechain_id existence oracle.
- **L3** (low) Fixed `TOKEN_BLINDING=777` + one shared consignment per batch → every recipient learns co-recipients' amounts; seals predictable to any observer.
- **L4** (low) external-latch reads ignore `expires_at` → expired rows never GC'd (DB hygiene).
- **L5** (low) `unilateral_exit` silently reports a lost/missing off-chain branch as "no branch" (indistinguishable from a flat coin).
- **L6** (low) Unauthenticated `deposit/get_token` mints unbounded `tokens` rows on non-mainnet/no-token-server deployments → DB write-amplification DoS.
- **L7** (low) The SDK background watcher never auto-exits before the stale-state deadline → an offline off-chain receiver can be clawed back (documented trust assumption + DX gap).

## Partials

- Exit-cost estimate reported `wait_blocks` (leaf sweep height), not the ancestor stale-backup race deadline — **fixed** this pass (`exit_deadline_block` added to `ExitCostEstimate`).
- Batch atomicity across recipients (carried from review 1, SPEC §14) — unchanged.

## Remediation

The full ordered remediation backlog (P0 → P2) with rationale is in [PLAN.md](PLAN.md#post-review-remediation-backlog-2026-07). P0 blocks mainnet.
