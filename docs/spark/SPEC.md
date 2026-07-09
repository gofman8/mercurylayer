# Mercury + RGB Spark-parity — System Specification

Normative specification of the Spark-parity system built on Mercury Layer statechains with RGB
assets and a single statechain entity (SE). Requirements are labelled **REQ-n**, invariants
**INV-n**, and error semantics **ERR-n**. Each is mapped to a verifying test in
[§12 Traceability](#12-traceability). Keywords MUST/SHOULD/MAY per RFC 2119.

Scope: the SE (Mercury server + lockbox), the client libraries (`mercurylib`, `mercuryrustlib`,
`mercury-rgb`), the wallet SDK (`mercury-spark-sdk`), the SSP service (`mercury-ssp`), and their
Bitcoin/RGB/Lightning interactions. Companion docs: [core-concepts](learn/core-concepts.md),
[invalidation](learn/invalidation.md), [PARITY.md](PARITY.md).

---

## 1. Roles and trust

- **Owner** — a wallet holding one key share of a coin. Can spend only with the SE; can always
  exit unilaterally without it.
- **SE (statechain entity)** — server + lockbox holding the other key share of every coin. Blind
  MuSig2 co-signer: never sees amounts/addresses. Enforces single-active-state, spend budgets,
  epochs. Cannot move funds alone; cannot block a unilateral exit.
- **SSP** — an application-level party (owner + Lightning node) bridging Mercury↔Lightning. Not
  trusted with custody: swaps are atomic (§8).
- **Issuer** — any owner that issues an RGB asset. No privileged runtime role beyond holding the
  contract issuance rights.

**REQ-1** The SE MUST NOT be able to move a coin's funds without the owner's co-signature (2-of-2).
**REQ-2** An owner MUST be able to exit to L1 without any SE cooperation (pre-signed material only).
**REQ-3** Trust reduces to: *the SE is honest about refusing conflicting/expired state*, plus the
owner *exits before its backup locktime floor / epoch deadline*. No custody rests on the SE.

---

## 2. Data model

### 2.1 Coin
A statechain coin is a Bitcoin P2TR UTXO whose key is the MuSig2 aggregate of `owner_pubkey` and
`se_pubkey`, plus SE-side state `{statechain_id, auth_pubkey, amount, locktime, single_use?,
epoch_deadline?, sig_budget?}` and client-side state (backup txs).

Coin status lifecycle (client): `INITIALISED → IN_MEMPOOL → UNCONFIRMED → CONFIRMED →
{IN_TRANSFER → TRANSFERRED | WITHDRAWING → WITHDRAWN | DUPLICATED | INVALIDATED}`.

**INV-1** A coin's `amount` equals the sats of its funding output.
**INV-2** A `CONFIRMED` coin has ≥ `confirmation_target` confirmations of its funding UTXO (or, for
an off-chain sub-coin, a validated exit branch — §2.3).

### 2.2 Sub-coin (off-chain)
A sub-coin is a coin whose funding tx is **un-broadcast**: it is an output of a split/combine tx
that the SE co-signed but nobody broadcast. Its `utxo_txid:vout` points at that un-broadcast tx.

### 2.3 Exit branch
An exit branch is the chain of fully-signed split/combine txs from a spend of an **on-chain**
outpoint down to the tx that funds a sub-coin, stored root-first under `branch-<statechain_id>`.

**INV-3** Every tx in a branch is consensus-valid against its predecessor's outputs; the branch
root spends an on-chain, unspent, confirmed outpoint. (Enforced by `validate_branch`.)
**INV-4** Branch (structural) txs carry no relative/absolute locktime — they are immediately
broadcastable.

### 2.4 Backup ladder
Each coin has ≥1 pre-signed backup tx paying the owner's address, at absolute locktime
`h + initlock − interval·k`. The first backup (`k=0`) is at `deposit_height + initlock`; every
transfer hands the new owner a backup one `interval` lower.

**INV-5** For any coin, the current owner's latest backup locktime is strictly lower than every
previous owner's backup locktime (current owner wins the exit race).

The ladder is a finite budget (`initlock` blocks, spent by each transfer and by wall-clock time).
When it nears the floor the coin can be moved to L1 by exit (§9), or COOPERATIVELY re-anchored
on-chain via `refresh` (§9.4, REQ-31) — a fresh SE-co-signed spend into a new aggregate that resets
the ladder and root deadline. There is deliberately no off-chain ladder renewal.

### 2.5 Ancestor record
For each sub-coin, its structural ancestors (the split/combine parents) are stored under
`parents-<statechain_id>` (parent id + inherited ancestors).

---

## 3. SE API (normative)

All endpoints are HTTP JSON on the Mercury server. Encrypted transfer messages are opaque to the
SE (owner-encrypted); the SE never deserializes `TransferMsg`.

### 3.1 Deposit / keygen
- `POST /deposit/init` `{token_id, auth_key, ...}` → `{server_pubkey, statechain_id, ...}`.
  Registers a new coin key-share. **REQ-4** MUST require a valid deposit token.
  `single_use` and `epoch_deadline` MAY be set at init.

### 3.2 Signing (blind MuSig2)
- `POST /sign/first` `{statechain_id, signed_statechain_id, ...}` → `{server_pubnonce}`.
- `POST /sign/second` `{statechain_id, session, server_pub_nonce, ...}` → `{partial_sig}`.

**REQ-5** `sign/first` MUST reject if `single_use` and the coin already has ≥1 finalized signature
(ERR-1).
**REQ-6** `sign/first` MUST reject if `epoch_deadline` is set and the SE clock ≥ it (ERR-2).
**REQ-7** `sign/first` MUST reject if `sig_budget` is set and finalized signatures ≥ budget (ERR-3).
**INV-6** The SE co-signs at most one *chain* of spends per coin (no second conflicting first-round
nonce is issued for a coin whose challenge is already set) — the single-active-state rule.

### 3.3 Transfer relay
- `POST /transfer/sender` → `x1` (receiver-binding scalar).
- `POST /transfer/update_msg` `{statechain_id, auth_sig, new_user_auth_key, enc_transfer_msg}` —
  stores the encrypted message. **REQ-8** MUST validate the sender's auth signature.
- `GET /transfer/get_msg_addr/<auth_key>` → encrypted messages for a receiver.
- `POST /transfer/receiver` — rotates the SE key share to the new owner; **REQ-9** after this the
  previous owner's share MUST be unusable.
- `POST /transfer/unlock` — releases a batch-locked coin (owner or SE side).

### 3.4 Withdraw
- `POST /withdraw/...` — SE co-signs a fresh direct spend to an L1 address (cooperative exit).

### 3.5 Lightning latch (v1 + v2)
- `POST /transfer/paymenthash` `{statechain_id, auth_sig, batch_id}` → `{hash}` — SE generates a
  preimage, returns `sha256(preimage)`; the coin transferred under `batch_id` is claim-locked.
- `GET /transfer/paymenthash/<batch_id>` → `{hash}` — the batch's hash (external hash if set, else
  `sha256(SE preimage)`).
- `POST /transfer/transfer_preimage` — returns the SE preimage **iff the batch is unlocked**
  (`locked=false`). **REQ-10** MUST NOT reveal the preimage while locked (ERR-4).
- `POST /transfer/paymenthash/external` `{statechain_id, auth_sig, batch_id, payment_hash}` — bind
  a latch to an EXTERNAL 32-byte hash (BOLT11). **REQ-11** MUST validate `payment_hash` is 32-byte
  hex and the auth signature.
- `POST /transfer/unlock/preimage` `{batch_id, preimage}` — unlock the batch iff
  `sha256(preimage)` equals the stored external hash. **REQ-12** MUST reject a non-matching
  preimage (ERR-5); on match, MUST unlock every coin in the batch (sender-side confirm).

### 3.6 Spend budget (terminal nodes)
- `POST /statechain/spend_budget` `{statechain_id, auth_sig, remaining∈{0,1}}` → `{sig_budget}` —
  owner-signed; sets an absolute co-signature ceiling. **REQ-13** MUST reject `remaining ∉ {0,1}`
  and a bad auth signature. Irreversible (budget only tightens).
- `GET /statechain/spend_budget/<id>` → `{sig_budget, finalized, terminal}` — public;
  `terminal = budget set ∧ finalized ≥ budget`.

---

## 4. Deposit

**Flow.** `get_deposit_address(amount)` → `/deposit/init` → P2TR aggregate address. Owner funds it.
The background watcher detects the UTXO (`update_coins`), waits `confirmation_target`, creates the
first backup tx (`create_tx1`, locktime `h+initlock`), flips the coin to `CONFIRMED`, emits
`DepositConfirmed`.

**REQ-14** A deposit slot MUST consume a deposit token; if payment is required the SDK MUST surface
`SdkError::TokenPaymentRequired` rather than silently proceeding (ERR-6).
**INV-7** After a deposit confirms, `get_balance().available_sats` increases by the deposit amount.

---

## 5. Transfer (sats)

**Flow.** Sender: `/transfer/sender` (get `x1`), sign the coin over to the receiver's address,
build the receiver's backup (locktime = previous − interval), `create_transfer_update_msg[_with_branch]`,
`/transfer/update_msg`. Receiver (async): fetch messages, validate, `/transfer/receiver` (SE
rotates its share).

**REQ-15** `transfer(address, amount)` MUST move exactly `amount`: either an exact subset of coins
(§5.1) or an off-chain split minting the exact piece (§6). No dust or overpayment.
**REQ-16** The receiver MUST validate: transfer signature binds the coin to its key; tx0/branch is
valid; latest backup pays the receiver; backup locktimes decrement correctly; num_sigs matches.
**REQ-17 (G1)** For a branch-carrying (sub-coin) transfer, the receiver MUST verify every
`terminal_parents` ancestor is terminal at the SE (`GET spend_budget`, `terminal==true`) and
reject otherwise (ERR-7).
**INV-8** Claiming is idempotent: repeated `claim()` passes book each transfer at most once.

### 5.1 Coin selection
`select::plan(coins, target)` returns `Exact(subset)` if a subset sums to `target`, else
`WithSplit{whole, split, split_amount}`, else `Insufficient{available}`.
**INV-9** `Exact(s)` ⟹ `Σ coins[s] = target`. `WithSplit` ⟹ `Σ whole < target ∧ split_amount =
target − Σ whole ∧ coins[split] > split_amount`. `Insufficient` ⟸ `Σ coins < target` — but the
reverse does **not** hold: since audit [29] the planner also returns `Insufficient` when the
remainder can only be minted as an unviable piece (no split candidate covers
`remainder + fee_reserve + min_split_output`, where `min_split_output = 330` (dust) `+` the sub-coin's
own backup fee at the live rate = `330 + ceil(112 · fee_rate)`; the planner also requires
`remainder ≥ min_split_output` so the minted piece can fund its own backup — `select.rs:114-121`).
See [GRANULARITY-SPEC.md](GRANULARITY-SPEC.md) GRN-REQ-5.

---

## 6. Off-chain split & combine

**Split.** `split_coin(id, piece)` builds one SE-co-signed, un-broadcast tx spending the coin into
`{piece sub-coin, change sub-coin}` (minus a fee reserve), records both as sub-coins with their own
backup ladders, the shared exit branch, and ancestor records; sets the parent's spend budget to 1.

**REQ-18** Before co-signing a split/combine, the SDK MUST set the parent(s)' `spend_budget` to 1
(exactly one more co-signature). After the split, each parent MUST be terminal.
**INV-10** `piece_sats + fee_reserve < parent_sats`; `change_sats = parent_sats − piece_sats −
fee_reserve`. `fee_reserve = clamp(parent_sats/100, 300, 2000)`.
**INV-11** A split tx has exactly one input (the parent) and one output per split entry plus, for
colored splits, one OP_RETURN; `output_vouts.len() == splits.len()`.
**Combine.** N coins → M outputs in one SE-co-signed (per-input) tx; each input matched to its coin
by outpoint; per-input MuSig2 over all prevouts.

---

## 7. Tokens (RGB)

Assets are RGB contracts (NIA fixed-supply, IFA inflatable). Allocations ride coins/sub-coins.

**Issuance.** `issue_token`/`issue_inflatable_token`: issue in the RGB engine, then fund + register
a statechain coin as the carrier in one colored on-chain tx.
**REQ-19** IFA issuance MUST create one colorable UTXO per allocation (fungible + each
inflation-right) before issuing.
**INV-12** After issuance the carrier holds the full fungible `supply`; IFA inflation-right stays
free in the engine.

**Mint (IFA).** `mint_tokens`: on-chain inflate in the engine, then bind the newly-minted allocation
to a fresh statechain coin.
**REQ-20** `mint_tokens` MUST isolate the newly-minted allocation (pre-inflate snapshot) so binding
never consumes already-bound supply.
**Burn.** `burn_tokens` burns engine-held free balance (on-chain). Statechain-bound supply must be
exited first.

**Transfer.** `transfer_tokens`/`batch_transfer_tokens`: a colored off-chain split carves the
recipient piece(s) + change; the consignment rides `BackupTx.rgb_consignment` as a
`ConsignmentEnvelope{c, a, s}`. When no single carrier holds the requested amount, the transfer
automatically COMBINEs several carriers of the same asset (`colored_combine_transfer`) into one
SE-co-signed colored combine tx (N input carriers → recipient piece + change), conserving the
asset's allocation across all combined inputs.
**REQ-21 (G2)** The receiver MUST book the amount the CONSIGNMENT assigns to its own witness
outpoint (`accept_offchain_amount`), treating the envelope amount `a` only as a cross-checked hint;
a mismatch MUST reject the transfer (ERR-8).
**REQ-22** The receiver MUST book under the consignment's cryptographically-verified `contract_id`,
not a sender-claimed id.
**INV-13** Token conservation: for a (batch) transfer, `Σ recipient amounts + change =
Σ allocations of the combined input carriers` (a single-carrier transfer is the N=1 case).
**N/A** RGB has no issuer freeze (no consensus enforcement point); documented, not faked.

---

## 8. Lightning swaps (SSP)

### 8.1 Pay (Mercury → Lightning)
`pay_lightning_invoice(ssp, invoice)`: mint the exact coin, `create_external_hash_latch` bound to
the invoice's payment hash, hand the coin to the SSP; the SSP pays the BOLT11; the LN preimage
`unlock_by_preimage`s the coin and is returned to the payer as proof.

**REQ-23** The SSP MUST verify the latch hash equals the invoice payment hash before paying.
**INV-14 (atomicity)** The SSP can claim the coin **iff** it holds the preimage, which exists **iff**
the invoice was paid. No payment ⟹ latch expires ⟹ payer keeps the coin. The returned preimage
MUST satisfy `sha256(preimage) == invoice_hash`.

### 8.2 Receive (Lightning → Mercury)
`create_lightning_invoice(ssp, amount)`: the SSP latch-transfers an exact coin to the user under an
SE-held preimage and issues a HODL invoice on that hash; on payment the SSP confirms the latch
(releasing the coin) then retrieves the preimage and claims the HTLC.

**INV-15 (atomicity)** The SE reveals the preimage only after the latch is unlocked (coin released),
so the SSP can take the HTLC money **only after** the user's coin is claimable. No payment ⟹ latch
expires ⟹ SSP keeps its coin. A wallet with zero on-chain presence can receive.

---

## 9. Exit

### 9.1 Cooperative (normal)
`withdraw(address, coins?)`: the SE co-signs a fresh direct spend to L1. For sub-coins the branch
is materialized first (branch txs are locktime-free). One on-chain tx per coin, no wait.

### 9.2 Unilateral (SE gone)
`unilateral_exit(coins?)`: broadcast the branch (instant) then the coin's latest pre-signed backup
(subject to its locktime).
**REQ-24** `unilateral_exit` MUST require no SE interaction.
**REQ-25** A backup whose locktime is unreached MUST be reported as `ExitStatus{complete:false,
wait_blocks>0}`, not an error; callable again after the wait.
**INV-16** After branch + backup confirm, funds are at the owner's address; RGB allocations settle
on-chain.

### 9.3 Cost
`estimate_exit_cost(coin)` → `{branch_txs, branch_vbytes, backup_vbytes, total_vbytes, wait_blocks}`.
**INV-17** `total_vbytes = branch_vbytes + backup_vbytes` (measured from the actual pre-signed txs);
`fee_sats_at(rate) = ceil(total_vbytes · rate)`; `wait_blocks = max(0, backup_locktime − tip)`.

### 9.4 Refresh (cooperative on-chain re-anchor)
`refresh(id, fee_rate?)` / `refresh_sponsored(id, sponsor, fee_rate?)`: one SE-co-signed single-input
spend of the coin's current 2-of-2 outpoint into a FRESH deposit aggregate (a new `statechain_id`,
same owner; a sub-coin's exit branch is materialized first). This resets both the backup ladder and
the tree's root deadline, avoiding the ladder floor without going to L1.
**REQ-31** `refresh` MUST spend the current outpoint into a fresh aggregate that gets a fresh full
ladder and a fresh root deadline; because the old outpoint is now spent, EVERY previous owner's
backup is permanently invalidated. It is COOPERATIVE — it needs the SE; if the SE is gone the owner
exits unilaterally (§9.2) instead. The fee is drawn from the coin (single-input, blind SE), so the
user-pays variant yields `amount − fee`; `refresh_sponsored` reimburses that fee OFF-CHAIN from a
funded sponsor (rebate `fee + dust`), leaving the user ≥ whole.

**REQ-32 (auto-refresh)** When `SdkConfig::auto_refresh` is set (default), the SDK MUST re-anchor a
coin nearing its ladder floor BEFORE the coin is spent, transparently: `auto_refresh_due(margin)`
re-anchors every confirmed, non-carrier coin whose headroom (`locktime − tip`) is ≤
`auto_refresh_margin_blocks`, and `transfer`/`transfer_many` MUST run this (and await the fresh
coins' confirmation) before selecting coins, so an aging coin never fails a handover or hands a
receiver a coin past its exit-race deadline — the re-anchor fee is the only visible effect. The
background watcher MUST also run the pass each poll so coins are refreshed proactively. Token
CARRIERS are excluded (a plain re-anchor would destroy the allocation — see §9.5). `sdk33`.

### 9.5 Watchtower (automatic deadline protection)
`auto_exit_due(margin)`: a maintenance pass that protects any owned OFF-CHAIN coin within `margin`
blocks of its deposit-anchored exit-race deadline (§9.3), before an ancestor can broadcast a stale
backup. The background watcher MUST run it each poll when `SdkConfig::auto_exit` is set (default),
with `auto_exit_margin_blocks` (default 288 ≈ 2 days — sized to absorb the audit-[17] `k·interval`
gap plus congestion/reorg slack).
**REQ-33** For a **plain** sub-coin the watchtower MUST force a unilateral exit (§9.2). For a
**received token carrier** — which the plain exit refuses — it MUST instead MATERIALIZE the coin by
broadcasting ONLY its exit branch (settling the RGB allocation on-chain and spending the shared
root), NEVER the sats-sweeping backup; it emits `TokenCarrierMaterialized`. An issued/flat carrier
has no exit branch (no ancestor, no clawback risk) and MUST be skipped. This gives a received token
the same automatic clawback protection plain coins already have. `sdk34`.

**REQ-34 (keyless watch delegation)** `export_watch_bundle()` MUST emit, per off-chain coin, only
pre-signed exit material and public metadata — the exit branch, the deadline, and (plain coins
only) the latest backup tx — and MUST contain no key material; a token carrier's entry MUST omit
the backup tx entirely (structurally denying an RGB-destroying sweep). `watch_pass(bundle,
electrum, margin)` MUST protect the bundled coins using only an electrum connection (no wallet,
DB, SE, or keys), tolerate idempotent re-broadcasts (so N independent watchtowers compose), and
surface genuine rejections. The full trust analysis is [TRUST-MODEL.md](TRUST-MODEL.md) §5.
`sdk35`.

---

## 10. Invalidation & security invariants

> The normative specification of the old-state invalidation mechanism (ladder formula, parameter
> constraints, receiver obligations, exit deadlines and their exactness domain, attacker matrix)
> is [INVALIDATION-SPEC.md](INVALIDATION-SPEC.md) (IVL-REQ/IVL-INV/IVL-ERR numbering). The items
> below are the system-level summary; where they overlap, INVALIDATION-SPEC.md is authoritative.

**INV-18 (no old state)** Split/combine spend into NEW outpoints; a child cannot confirm before its
parent (its input is the parent's output), so there is no old-vs-new race within a tree.
**INV-19 (fork prevention)** The SE refuses a second spend of any node (single-use / spend budget),
so a node cannot be forked into two conflicting children.
**INV-20 (terminal ancestors)** A sub-coin's receiver only accepts it if every structural ancestor
is terminal at the SE (REQ-17) — a malicious sender cannot double-spend a parent afterwards. The
receiver derives the required ancestor count from the branch itself: it requires at least one named
terminal ancestor **per branch hop** (`n_parents ≥ branch_len`, `≥ 1`), so a sender cannot hide a
non-terminal, double-spendable ancestor by shipping an empty or short `terminal_parents` list
(ERR-7). Verified by `unit::terminal_parents_tests` + `sdk10`.
**INV-21 (bounded lifetime)** With `epoch_deadline` set, the SE stops co-signing new state past the
deadline; unilateral exit still works forever (needs no SE), so funds are never swept.
**INV-22 (UTXO granularity)** Exact amounts are native (1-sat resolution) via off-chain split —
strictly finer than fixed-denomination leaves.
**INV-23 (nonce single-use)** The SE binds each server nonce to exactly ONE challenge: `sign/second`
sets the challenge atomically only if it was NULL (or identical — idempotent retry) and otherwise
refuses (ERR-12). A second finalize over one nonce with a different message is therefore impossible,
which is what makes the blind-MuSig2 scheme safe against an owner who controls the raw signing
requests — without it, two partial signatures over one secnonce would leak the SE's key share and
yield two co-signed conflicting spends while `count_finalized_signatures` (and hence single-use /
budget / epoch enforcement) counted only one. Verified by `sdk12` Part C.
**INV-24 (budget monotonic)** `set_spend_budget` may only TIGHTEN a coin's `sig_budget`
(`new = min(existing, count+remaining)`); it can never raise it, so an already-terminal node cannot
be re-opened for a second conflicting spend. Verified by `sdk08` (unchanged terminal behaviour).
**INV-25 (branch value conservation)** The receiver's `validate_branch` rejects any exit branch whose
txs create value (`Σ outputs > Σ inputs` at any hop): `tx.verify` checks scripts but not the fee
rule, so without this a sender could hand over a coin whose branch is script-valid yet un-broadcastable
(the receiver could never exit on-chain while the sender keeps the funds). Verified by `sdk12`/`sdk10`
(honest branches still accepted).
**INV-26 (received amount = spendable only)** A transfer's received token amount counts only
`Fungible` assignments, never `InflationRight` (the right to mint). Booking an inflation right as
spendable balance would let a right-holder inflate a receiver's balance out of nothing
(conserves INV-12/INV-13). Verified by `sdk09`.

---

## 11. Error semantics

- **ERR-1** single-use second spend → HTTP 410 `single-use coin already spent`.
- **ERR-2** past epoch deadline → HTTP 4xx epoch refusal.
- **ERR-3** spend budget exhausted → HTTP 410 `spend budget exhausted`.
- **ERR-4** preimage requested while latch locked → HTTP 404 `not available ... still locked`.
- **ERR-5** wrong preimage on `unlock/preimage` → HTTP 403.
- **ERR-6** deposit token requires payment → `SdkError::TokenPaymentRequired{token_id,
  deposit_address, fee_sats}`.
- **ERR-7** non-terminal ancestor → receiver validation error `structural parent ... is NOT
  terminal`, transfer not booked.
- **ERR-8** consignment/envelope amount mismatch → receiver rejects `consignment assigns X ...
  envelope claimed Y`.
- **ERR-9** `InsufficientBalance{requested, available}` on over-balance transfer.
- **ERR-10** double-withdraw / spend of a non-CONFIRMED coin → refused with the coin's status.
- **ERR-12** second `sign/second` reusing a server nonce over a different message → HTTP 409
  `server nonce already finalized with a different challenge`.

---

## 13. Query, utility & invoice API

Client-side conveniences (no new SE state); mirror Spark's query/signing/invoice surface.

**REQ-26** `sign_message_with_identity_key(msg)` MUST produce a BIP340 Schnorr signature over
`sha256(msg)` under a STABLE identity key (derived at `m/1000h/0h/0h`, unchanged as coins come and
go); `validate_message_with_identity_key(msg, sig, pubkey)` MUST verify it and reject a tampered
message.
**REQ-27** `transfer_many(recipients)` MUST pay each recipient its exact amount from one off-chain
split (N pieces + change), with the same branch + terminal-parent guarantees as a single transfer
(REQ-17/REQ-18).
**REQ-28** `create_sats_invoice`/`create_tokens_invoice` MUST encode {address, amount, asset?,
memo?, expiry?} into a `sparkinv1…` string that round-trips through `decode_spark_invoice`;
`fulfill_spark_invoice` MUST reject an expired invoice (ERR-11) and otherwise pay the embedded
amount/asset to the embedded address.
**REQ-29** `list_coins`/`get_transfers`/`get_transfer` MUST reflect the wallet's current coins and
activity; `get_withdrawal_fee_quote` MUST return a positive fee at the electrum-estimated rate.
**REQ-30** `get_token_l1_address` returns the RGB engine funding address; `query_token_transactions`
returns the contract's transfer history.

- **ERR-11** `fulfill_spark_invoice` on an expired invoice → `invoice expired at …`.

## 12. Traceability

Each requirement/invariant is verified by at least one test. Pure-logic items have unit tests;
protocol items have E2E tests (regtest). See [testing-guide](build/testing-guide.md) for how to run.

| Item | Test |
|---|---|
| REQ-1, REQ-3 | design (2-of-2 keys); exercised by every co-sign flow |
| REQ-2, REQ-24, REQ-25, INV-16, INV-17 | `sdk07` (unilateral exit + cost), `unit::types::tests::exit_cost_math`, `unit::invalidation_model::exit_cost_scaling_model` |
| REQ-4, REQ-14, ERR-6, INV-7 | `sdk01` deposit; `unit::types::tests::error_semantics` |
| REQ-5, ERR-1 | `rgb04` (single-use refusal) |
| REQ-6, ERR-2, INV-21 | `rgb07` (epoch deadline) |
| REQ-7, REQ-13, REQ-18, ERR-3, INV-19 | `sdk08` (terminal node), `unit::types::terminal_predicate`, `unit::invalidation_model::terminal_predicate_matrix` |
| REQ-8, REQ-9, REQ-15, REQ-16, INV-5, INV-8 | `sdk01`, `sdk04`, upstream `tb01/tb05/tm01/ta02/ta03` |
| REQ-10, ERR-4 | `sdk03` (latch locked pre-settlement) |
| REQ-11, REQ-12, ERR-5, REQ-23, INV-14 | `sdk05` (pay), `unit::ssp::swap_tests::preimage_matches_hash` |
| INV-15 | `sdk06` (receive) |
| REQ-15, INV-9 | `sdk01`; `unit::select` (exact/split/insufficient) |
| REQ-17, INV-20, ERR-7 | `sdk10` (terminal-parent verify: honest accept, non-terminal reject) |
| REQ-18, INV-10, INV-11 | `sdk01`/`sdk08`; `unit::split_math` |
| INV-18, INV-19 | `rgb03`/`rgb06` (off-chain DAG), `rgb04` |
| REQ-19, REQ-20, INV-12, INV-13 | `sdk09` (IFA issue + mint + batch) |
| REQ-21/INV-13 (multi-carrier combine) | `sdk31` (token combine) |
| REQ-21, REQ-22, ERR-8 | `sdk02`, `sdk09`; `unit::envelope` |
| ERR-9 | `sdk04` (`unit::select` insufficient) |
| ERR-10 | `sdk04` (double-withdraw / split-parent refusal) |
| INV-22 | `sdk01`/`sdk09` (exact-amount splits) |
| REQ-26 | `sdk11`; `unit::identity_tests::sign_validate_roundtrip` |
| REQ-27 | `sdk11` (multi-recipient) |
| REQ-28, ERR-11 | `sdk11`; `unit::invoice::tests` (roundtrip, reject) |
| REQ-29, REQ-30 | `sdk11` (query API + fee quote) |
| REQ-31 (refresh / re-anchor) | `sdk30` (refresh + sponsored refresh) |
| REQ-32 (auto-refresh in transfer) | `sdk33` (maintenance pass, embedded transfer, opt-out) |
| REQ-33 (watchtower carrier materialize) | `sdk34` (received-carrier auto-materialize, clawback defeated) |
| REQ-34 (keyless watch delegation) | `sdk35` (keyless bundle, 2 towers idempotent, malicious-sender rejection); `unit::watchtower::tests` |
| INV-20 (ancestor-count binding), ERR-7 | `unit::terminal_parents_tests`, `sdk10`, `sdk12` (honest accept) |
| INV-23, ERR-12 | `sdk12` Part C (nonce-reuse refused) |
| INV-24 | `sdk08` (terminal node stays terminal) |
| INV-25 | `sdk10`/`sdk12` (honest branch accepted; value-inflating branch rejected) |
| INV-26 | `sdk09` (IFA received amount = fungible only) |

## 14. Known limitations (adversarial review)

Findings from the adversarial review that are **documented assumptions**, not code changes:

- **Blind-SE ancestor binding.** The SE stores no per-`statechain_id` funding outpoint (it is blind),
  so the receiver cannot cryptographically bind `terminal_parents` ids to specific branch outpoints.
  INV-20's count check defeats omission; full defence against *substitution* of terminal decoys relies
  on the receiver holding the fully-signed branch and being able to exit immediately (win the race for
  the on-chain root). Honest senders always set each node terminal.
- **Batch atomicity.** `transfer_many` / `batch_transfer_tokens` hand off pieces independently; there
  is no all-or-nothing guarantee across recipients. A dropped hand-off leaves that piece reclaimable
  by the sender (the split parent is terminal, so no double-spend), but the batch is not atomic.
- **Amount width.** Coin sats are booked as `u32` (`utxo.value as u32`, `coin_status.rs`); a single
  coin above ~42.9 BTC would truncate. Out of range for the intended per-coin sizes; not guarded.
- **Mint concurrency.** `mint_tokens` isolates the freshly-minted allocation by a before/after snapshot
  and does NOT hold the wallet lock across its (minutes-long) on-chain confirmation wait, to avoid
  blocking the background claim watcher. A concurrent same-asset receive into the *same* wallet during
  a mint could be misattributed — issuers must not mint and receive the same asset concurrently.
- **Unilateral-exit fees.** Exit broadcasts pre-signed fixed-fee branch/backup txs with no CPFP/RBF
  fee-bump; in a fee spike an exit may confirm slowly. The decrementing-locktime ladder (INV-5) still
  guarantees the latest state wins the race.

> **P0 remediation status (2026-07-05 review).** The second adversarial review's six P0 blockers are
> now **FIXED on `feat/spark`** and verifiable in code: the enclave/challenge nonce-reuse crypto break
> (C1 — challenge-binding refuses reuse, `sign.rs`), the two SSP fund-loss bugs (C2/C3 — SSP
> pre-payment recipient/amount gate, `ssp.rs`), the split-locktime exit-race inversion (H5 — branch
> txs are now locktime-free, INV-4), branch-conflict masking (H1 — `reject_non_tree_branch`,
> `transfer_receiver.rs`), token-carrier destruction (H2 — carrier excluded from plain-BTC split,
> `transfer.rs`), and the mnemonic-only-backup durability gap (H3 — recovery bundle). Two caveats
> remain before mainnet: the **SGX lockbox must be rebuilt and redeployed** for the enclave-side
> single-use secnonce to take effect, and the **full E2E suite (regtest + lockbox + RLN) must be re-run
> and the result re-reviewed**. See [REVIEW.md](REVIEW.md#second-adversarial-review-2026-07-05--full-protocol-production-readiness-pass).

Unit tests live in `clients/libs/rust-sdk/src/*` (`#[cfg(test)]`); E2E dispatch via
`SDK_E2E`/`RGB_E2E` in `clients/tests/rust`; upstream Mercury suite runs by default.
