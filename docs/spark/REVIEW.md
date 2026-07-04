# Adversarial security review

An adversarial pass over the Spark-parity statechain + RGB fork, looking for behaviours the spec did
not cover — places where a message or value can be **delayed, replayed, malformed, reordered, or
changed** for an attacker's gain. Findings were produced by per-dimension finders reading the real
code + the full-suite trace logs, then each was independently verified against the actual defence
code (refute-by-default). 13 candidates verified → 10 confirmed. This document records the outcome.

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
