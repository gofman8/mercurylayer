# Spark core protocol — condensed implementation notes

Distilled from buildonspark/spark (Go operator + protos). Source clone: `~/Claude/spark-study/spark`.

Every `Mercury mapping` bullet below states what the **shipped** Utexo protocol does today — TES-R
laddering (`../PROTOCOL.md`), the HODL-invoice Lightning latch (`../LIGHTNING.md`), first-class split
children (`../CHILDREN.md`), and the feature-by-feature comparison in `../PARITY.md`.

**One protocol, two coin SHAPES — both current, neither is a legacy lane.**
`deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT` env are **deleted**; `claim()` ladders
every fresh confirmed ROOT coin unconditionally.
- **Laddered** — every plain BTC deposit. Trigger / Extension / State tiers, **relative** CSV locks,
  all un-broadcast.
- **Un-laddered** — an RGB **carrier** (deliberately never laddered) and a **split sub-coin whose
  funding is un-broadcast** (it cannot root a trigger). These keep the signed-once backup transaction
  with decrementing absolute nLockTimes and transfer by backup-chain handover. Load-bearing for RGB
  assets, not deprecated.

## Deposit (L1 → tree)
- Address = P2TR of (user_signing_key + SO_keyshare) point-add; proof-of-possession sigs returned.
  Static addresses are cached/reusable + rotatable; ephemeral are one-shot.
- Tree creation pre-signs THREE txs before the deposit is considered claimed:
  1. **Node tx** (CPFP): UTXO → pay-to-self (same 2-of-2), no lock on input, anchor output.
  2. **Refund tx**: node-tx-out → user address, relative timelock 2000 blocks.
  3. **Watchtower refund** (direct-from-CPFP): node-tx-out → watchtower, no lock (fee-bump path).
- Claim = start_deposit_tree_creation (get SO sig shares) + finalize (commit user-signed refunds).
- **Mercury mapping (laddered — every plain deposit).** `claim()` pre-signs a three-tier TES-R ladder
  over the confirmed funding outpoint `F`:
  `F → T` (**TRIGGER**, *no* timelock, signed once at deposit) `→ X_m` (**EXTENSION**, relative CSV
  `E_m = E0 − m·δE`) `→ S_k` (**STATE**, relative CSV `Δ_k = D0 − k·δ`, pays the current owner).
  All three tiers are **v3/TRUC with a P2A anchor** and stay **UN-BROADCAST**. Structurally this is
  Spark's node-tx + refund-tx pair plus one extra tier, but with BIP-68/112 **relative** locks instead
  of Spark's 2000-block refund lock.
- Consequence, and the biggest single difference from the pre-TES-R shape: a relative lock only starts
  counting once its **parent confirms**, and `T` carries no lock at all, so **nothing matures until
  somebody broadcasts `T`**. An **idle coin never ages** — no calendar deadline, no "exit before your
  floor", **0 vB of idle rent**. Same zero-footprint benchmark as Spark, reached *without* Spark's
  1-of-n key-deletion trust (invalidation here is consensus-level, §"Transfer" below).
- Spark's watchtower-refund tier maps onto our **keyless watch bundle**: it carries no key material and
  a second independent tower is idempotent (`sdk45`), and it is event-driven — the alarm is a public
  trigger broadcast, not a calendar date (`sdk51`). Owner-side CPFP still exists; the P2A anchor on
  every tier is the fee-bump path, and towers are delegable and mandatory-forever-acceptable.
- **Mercury mapping (un-laddered).** An RGB carrier is never laddered — a plain tier spend would
  destroy the allocation (terminal-freeze, `sdk52`) — and a split sub-coin over an un-broadcast funding
  outpoint cannot root a trigger. Those coins carry the signed-once backup tx + their ancestors branch,
  with SE **single-use per node** (spend budget) making a race impossible (`rgb04`, exit `sdk39`).

## Tree/leaf + timelocks
- TreeNode: value, verifying_pubkey (user+SO), parent, pre-signed node_tx + refund_tx, timelocks.
- Initial timelock 2000 blocks; **every transfer decrements ~100**; renewal required at ≤300 via
  `renew_leaf` (3 cases: reset node lock via zero-timelock split node; reset refund only; from-zero).
- Split leaf → smaller leaves: **split node tx with timelock 0** (intermediate), children get 2000.
- **Mercury mapping — decrement.** On a laddered coin the decrement is over the **relative** state CSV,
  not an absolute nLockTime: each transfer co-signs a fresh state exactly one `δ` **lower** than the one
  it replaces (replace-by-lower-timelock), so the new owner's state always matures first. Shipped
  mainnet dials (`TesrParams::mainnet()`): `D0/δ/D_floor = 1440/36/144`, `E0/δE/E_floor = 720/36/144`,
  `m_max = 15`. Activity therefore *shortens* the unilateral-exit wait instead of pushing the coin
  toward an expiry — there is no floor to approach.
- **Mercury mapping — renewal.** ≈ Spark's `renew_leaf`, but unbounded and with no ≤300 trigger
  condition: a lower-CSV extension replaces `X_m` **horizontally** and, when the extension budget is
  exhausted, the ladder **rolls over** — the current state becomes a self-split and a fresh level hangs
  off it. Both are **off-chain**, zero on-chain bytes, unlimited (`sdk43` renews, rolls over, renews
  again, then exits the whole deep chain).
- **Mercury mapping — `refresh`.** No longer a deadline reset (there is no deadline). It is the
  **re-anchor** primitive: one cooperative on-chain tx moves the coin to a fresh funding outpoint and
  mints a new ladder, killing every exit right rooted at the old `F`. User-pays and operator-sponsored
  modes (`sdk30`); the sponsored rebate is itself an in-ladder split, so it is sized
  `max(fee + DUST_LIMIT, min_child_value)`. Un-laddered carriers renew by the same re-anchor.
- **Mercury mapping — split.** Spark's zero-timelock split node maps to the **in-ladder split**: a
  state tier `SP` spends `X_m.out[0]` — a **descendant of the trigger**, not a rival for `F` — and pays
  a piece child plus a change child. Each child funds its OWN two tiers plus dust, so admission is
  gated on `min_child_value` = **1306 sat at 2 sat/vB** (`sdk58`, `sdk59`); children get their own tier
  pair rather than a fresh 2000-block refund.
- **Mercury mapping — un-laddered shape.** This is where the decrementing *absolute* nLockTime backup
  chain still lives (init/interval from server config, `tb05`), together with SE single-use per node and
  the optional epoch deadline (`rgb07`). Its automatic protection is `auto_exit_due`, which materializes
  a carrier near its deadline (`sdk34`).

## Transfer (key-tweak handover) — v3 single call
- Sender pre-signs refund txs for the new owner epoch (sequence = old timelock − 100), builds
  key-tweak package (per-SO ECIES-encrypted VSS tweak shares), signs package hash.
- SO prepare: validate + lock leaves, FROST-sign refunds; commit: apply tweaks (old SE share
  deleted → sender can't spend), state SENDER_KEY_TWEAKED.
- Receiver `claim_transfer`: provides own refund sigs + own tweaks → COMPLETED.
- Statuses: SENDER_KEY_TWEAK_PENDING → SENDER_KEY_TWEAKED → COMPLETED; leaves TRANSFER_LOCKED.
- **Mercury mapping — key rotation.** `transfer_sender`/`transfer_receiver` IS this handover (single SE,
  no VSS/ECIES fan-out; the server share rotates via lockbox `keyupdate`). Interrupt/resume = Mercury
  duplicate handling + `statechain_id` state.
- **Mercury mapping — what backs invalidation.** On a laddered coin the hop also co-signs the fresh
  lower-CSV state and **discloses exactly one superseded state**, which the receiver's census counts
  against the enclave sig-count. A stale state loses at **consensus** (it matures later), with enclave
  refusal only as defense-in-depth — where Spark relies on 1-of-n honest key deletion (`sdk40` PART 2,
  `sdk46`/`sdk47` census, `sdk54` adversarial `verify_bundle`).
- **Mercury mapping — split children are first-class.** A received piece is not a dead end: the claim
  completes the standard SE key handover, so the receiver co-owns `A_child` (invariant across the
  rotation, which keeps the pre-signed exit chain valid) and the sender is **permanently locked out**.
  The child can then be paid onward off-chain — whole (`child_retransfer`) or split
  (`child_in_ladder_pay`, a depth-2 ancestors chain) — at one co-signature and one disclosed superseded
  state per hop, counted by the receiver's census (`sdk60`: alice → bob → carol with the funding
  outpoint unspent throughout; `sdk17`: partial second hop). Spark has no direct analogue — its leaves
  need operator renewal to keep moving.

## Swaps (denomination) — WHY THEY EXIST
- Spark leaves are fixed outputs; to pay exact amounts users swap leaves with the SSP
  (`start_leaf_swap_v2` / swap-v3 primary+counter with **adaptor signatures** for atomicity).
- **Mercury+RGB: NOT NEEDED.** A non-exact payment is served by the **in-ladder split** described
  above — the payer's own coin produces the piece and the change off-chain, so there is no denomination
  inventory, no SSP swap counterparty and no operator liquidity. The only lower bound is
  `min_child_value` (1306 sat at 2 sat/vB), a fee floor rather than a denomination grid.
- No adaptor signatures are used anywhere in the shipped protocol. Cross-user atomicity already exists
  two ways: Mercury's atomic transfer via the swap protocol (`tb03`) and the preimage-gated Lightning
  latch (`tb04`, and see below).

## Lightning (preimage swaps)
- Send: `initiate_preimage_swap(payment_hash, transfer→SSP)` locks leaves on hash; SSP pays BOLT11;
  `provide_preimage` → SO validates sha256(preimage)==hash → transfer completes to SSP.
- Receive: user VSS-splits preimage across SOs (`store_preimage_share`); SSP pays user leaves; SOs
  release shares; SSP reconstructs preimage to settle inbound HTLC.
- **Mercury mapping.** A **HODL-invoice latch** over the ladder, working in **both directions** and for
  exact and non-exact amounts: `sdk63` (pay), `sdk64` (receive), `sdk65` (non-exact pay), `sdk67`
  (non-exact receive), `sdk66`/`sdk68` (failure + rollback), `sdk19`/`sdk20`/`sdk24`/`sdk25`
  (adversarial), `sdk21` (remote SSP over HTTP), `tb04` (the raw latch primitive). A single SE holds the
  gate — no VSS preimage sharing, and no preimage share ever leaves the enclave boundary.
- Non-exact LN goes through the same in-ladder split. The **LN-latched piece is the one case that stays
  terminalized**: it sits unclaimed past the pending-transfer lock's window, so it is frozen rather than
  left re-transferable. Everything else keeps terminalization optional.
- LSP counterparty = SSP role (unchanged).

## Exits
- Cooperative: via SSP — user transfers leaves to SSP, SSP's **connector tx** pays user's L1 address;
  SO enforces atomicity (transfer valid iff connector txid bound). exitSpeed = fee tiers.
- Unilateral: broadcast pre-signed node tx chain + refund tx after relative locks; watchtower can
  fee-bump. Single-node trees have a dedicated RPC (`exit_single_node_trees`).
- **Mercury mapping — cooperative.** SE co-signs a direct spend (`withdraw`) — simpler, no SSP and no
  connector tx.
- **Mercury mapping — unilateral, laddered coin.** `unilateral_exit()` **walks the pre-signed chain
  tier by tier**, waiting out each *relative* timelock: broadcast `T` → wait `E_m` → broadcast `X_m` →
  wait `Δ_k` → broadcast `S`, reporting `wait_blocks` between tiers; funds land at the wallet's own
  seed-derived backup address. 372–828 vB total, ≤ ~2,160 blocks (~15 d) fresh and shrinking with
  activity (`sdk50` via the public SDK, `sdk40` PART 1 against real consensus, deep post-rollover chains
  `sdk43`, adversarial `sdk58`). This is not "broadcast your backup and win a race" — the race framing
  belongs to the un-laddered shape only.
- **Mercury mapping — unilateral, un-laddered coin.** Branch broadcast (no locktime on the branch) plus
  the stored pre-signed backup, which *is* absolute-locktime-gated; `sdk39` exits a depth-2 colored
  sub-coin end to end (~267 vB at depth 1).
- **Mercury mapping — offline owner.** A watchtower drives the same laddered walk from a **keyless**
  bundle (`sdk45`), including against a hostile trigger broadcast by a previous owner (`sdk51`).
- RGB settles once the anchors are mined (unchanged).

## Tokens (BTKN) — for RGB replacement
- TTXO model: token_outputs with owner pubkey, uint128 amount, revocation commitment, SO withdrawal
  sig (enables offline L1 exit), status (AVAILABLE/PENDING/FROZEN).
- Ops: CREATE (metadata: name≤20, ticker≤6, decimals, max_supply u128, is_freezable) / MINT
  (issuer only) / TRANSFER (Σin==Σout) / FREEZE-UNFREEZE (issuer sig, only if freezable).
- Two-phase: start_transaction (server sets expiry, returns keyshare) → commit_transaction
  (aggregate sigs per input).
- **RGB mapping.** CREATE≈issue (NIA fixed / IFA inflatable); MINT≈IFA inflate; TRANSFER≈RGB transfer
  over coins (consignments, client-validated); FREEZE = N/A (no consensus meaning client-side);
  token_identifier ≈ RGB contract id.
- **Which coin shape carries RGB.** The **un-laddered** one, always: a carrier is never laddered because
  a plain tier spend would destroy the allocation (terminal-freeze, `sdk52`), so RGB rides the
  signed-once backup chain + ancestors branch. The SE stays blind to RGB contents throughout; disclosure
  happens only at exit.

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
- Ours, dispatch: `SDK_E2E=1..68` (with gaps where tests were retired during the single-protocol
  migration) plus `chaos22`, alongside the `rgb*` / `ta*` / `tb*` suites. Mirror coverage of the list
  above now sits at `sdk58` (11 adversarial in-ladder-split cases), `sdk54`/`sdk55` (bundle and ladder
  validation), `sdk66`/`sdk68` (LN failure + rollback), `sdk40` PART 2 / `sdk51` (stale-state and
  hostile-trigger defeat) and `chaos22` (no cheat succeeds under concurrency).
