# Spark core protocol — condensed implementation notes

Distilled from buildonspark/spark (Go operator + protos). Source clone: `~/Claude/spark-study/spark`.

## Deposit (L1 → tree)
- Address = P2TR of (user_signing_key + SO_keyshare) point-add; proof-of-possession sigs returned.
  Static addresses are cached/reusable + rotatable; ephemeral are one-shot.
- Tree creation pre-signs THREE txs before the deposit is considered claimed:
  1. **Node tx** (CPFP): UTXO → pay-to-self (same 2-of-2), no lock on input, anchor output.
  2. **Refund tx**: node-tx-out → user address, relative timelock 2000 blocks.
  3. **Watchtower refund** (direct-from-CPFP): node-tx-out → watchtower, no lock (fee-bump path).
- Claim = start_deposit_tree_creation (get SO sig shares) + finalize (commit user-signed refunds).
- Mercury mapping: deposit init + backup tx ≈ node/refund pair; our tree sub-coins skip backups
  (single-use). Watchtower path = N/A (no watchtower service; CPFP by owner).

## Tree/leaf + timelocks
- TreeNode: value, verifying_pubkey (user+SO), parent, pre-signed node_tx + refund_tx, timelocks.
- Initial timelock 2000 blocks; **every transfer decrements ~100**; renewal required at ≤300 via
  `renew_leaf` (3 cases: reset node lock via zero-timelock split node; reset refund only; from-zero).
- Split leaf → smaller leaves: **split node tx with timelock 0** (intermediate), children get 2000.
- Mercury mapping: flat coins = native decrementing nLockTime backups (init/interval from server
  config). Tree sub-coins = SE single-use per node + epoch deadline instead (no renewal mechanism;
  bounded life, exit before deadline).

## Transfer (key-tweak handover) — v3 single call
- Sender pre-signs refund txs for the new owner epoch (sequence = old timelock − 100), builds
  key-tweak package (per-SO ECIES-encrypted VSS tweak shares), signs package hash.
- SO prepare: validate + lock leaves, FROST-sign refunds; commit: apply tweaks (old SE share
  deleted → sender can't spend), state SENDER_KEY_TWEAKED.
- Receiver `claim_transfer`: provides own refund sigs + own tweaks → COMPLETED.
- Statuses: SENDER_KEY_TWEAK_PENDING → SENDER_KEY_TWEAKED → COMPLETED; leaves TRANSFER_LOCKED.
- Mercury mapping: transfer_sender/transfer_receiver key rotation IS this (single SE, no VSS/ECIES
  fan-out; server key share rotates via lockbox `keyupdate`). Interrupt/resume = Mercury duplicate
  handling + statechain_id state.

## Swaps (denomination) — WHY THEY EXIST
- Spark leaves are fixed outputs; to pay exact amounts users swap leaves with the SSP
  (`start_leaf_swap_v2` / swap-v3 primary+counter with **adaptor signatures** for atomicity).
- Mercury+RGB: NOT NEEDED — multi-input combine + multi-output split creates exact amounts
  off-chain. Keep adaptor-sig atomic swap idea only if cross-user atomic swap parity is wanted
  (Mercury has atomic transfer tb03 via swap protocol already).

## Lightning (preimage swaps)
- Send: `initiate_preimage_swap(payment_hash, transfer→SSP)` locks leaves on hash; SSP pays BOLT11;
  `provide_preimage` → SO validates sha256(preimage)==hash → transfer completes to SSP.
- Receive: user VSS-splits preimage across SOs (`store_preimage_share`); SSP pays user leaves; SOs
  release shares; SSP reconstructs preimage to settle inbound HTLC.
- Mercury mapping: **lightning latch** endpoints on dev (paymenthash-gated transfer; single SE holds
  the gate — no VSS). tb04 test demonstrates. LSP counterparty = SSP role.

## Exits
- Cooperative: via SSP — user transfers leaves to SSP, SSP's **connector tx** pays user's L1 address;
  SO enforces atomicity (transfer valid iff connector txid bound). exitSpeed = fee tiers.
- Unilateral: broadcast pre-signed node tx chain + refund tx after relative locks; watchtower can
  fee-bump. Single-node trees have a dedicated RPC (`exit_single_node_trees`).
- Mercury mapping: cooperative = SE co-signs direct spend (withdraw) — simpler, no SSP/connector.
  Unilateral = backup tx (flat) or branch broadcast (tree). RGB settles once anchors are mined.

## Tokens (BTKN) — for RGB replacement
- TTXO model: token_outputs with owner pubkey, uint128 amount, revocation commitment, SO withdrawal
  sig (enables offline L1 exit), status (AVAILABLE/PENDING/FROZEN).
- Ops: CREATE (metadata: name≤20, ticker≤6, decimals, max_supply u128, is_freezable) / MINT
  (issuer only) / TRANSFER (Σin==Σout) / FREEZE-UNFREEZE (issuer sig, only if freezable).
- Two-phase: start_transaction (server sets expiry, returns keyshare) → commit_transaction
  (aggregate sigs per input).
- RGB mapping: CREATE≈issue (NIA fixed / IFA inflatable); MINT≈IFA inflate; TRANSFER≈RGB transfer
  over coins (consignments, client-validated); FREEZE = N/A (no consensus meaning client-side);
  token_identifier ≈ RGB contract id.

## RPC surface (UtexoService, for API-shape parity)
deposit: generate[_static]_deposit_address, rotate_static, query_unused/static_addresses,
start/finalize_deposit_tree_creation. tree: query_nodes, renew_leaf, exit_single_node_trees.
transfer: start_transfer_v2/v3, start_leaf_swap_v2, initiate_swap_primary_transfer, claim_transfer,
claim_transfer_sign_refunds_v2, finalize_node_signatures_v2, query_pending/all/by_id.
lightning: initiate_preimage_swap_v2/v3, store_preimage_share[_v2], provide_preimage,
query_preimage, query_htlc. exit: cooperative_exit_v2, initiate_static_deposit_utxo_refund.
util: query_balance, get_utxos_for_address/identity, query_utexo_invoices, subscribe_to_events,
get_signing_commitments, get_signing_operator_list, update/query_wallet_setting.

## Test-structure notes (for the mirror track)
- Go: handler unit tests (~715 fn) + grpc_test integration (~140 fn: deposit 25, transfer 36,
  lightning 18, coop-exit 5, consensus/atomicity regressions, tokens 11 files).
- JS: spark-sdk unit (38 files) + integration (20) + ssp (9); issuer-sdk (10); bare (11).
- Adversarial set worth mirroring: double-claim deposit, duplicate-leaf claim, transfer interrupt +
  recovery, wrong preimage, refund-validation, conflicting spend refusal, atomicity regressions.
- Harness: bitcoind regtest + miner + electrs + 5 signers + 5 SOs (run-everything.sh); deterministic
  faucet keys. Ours: rgb-lightning-node regtest (bitcoind+electrs+proxy) + lockbox stack.
