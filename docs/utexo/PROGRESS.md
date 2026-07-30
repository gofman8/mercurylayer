# Progress tracker — Utexo (Spark-compatible API) on Mercury + RGB

Working file: updated at every push so work is resumable at any point.
Branches: mercurylayer `feat/spark` · rgb-lib `feat/spark` (both on gofman8/UTEXO-Protocol).

## Status (2026-07-29) — ONE protocol

The log below is historical and stays as written. What ships today:

* **One protocol.** `claim()` establishes a TES-R ladder (trigger `T` → extension `X_m` → state `S`,
  relative CSV, all tiers un-broadcast) for every fresh confirmed ROOT coin, unconditionally.
  `deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT` env are **deleted**; no test pins any
  other lane. See [PROTOCOL.md](PROTOCOL.md).
* **Two coin SHAPES, both current — the un-laddered one is not "legacy".** LADDERED: every plain
  deposit. UN-LADDERED: an RGB **carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation — terminal-freeze, PROTOCOL.md §5.10), and a split sub-coin whose funding is
  un-broadcast cannot root a trigger [B0]. The un-laddered shape keeps the signed-once backup and
  transfers by backup-chain handover; it is load-bearing for RGB tokens (sdk52).
* **Received split children are first-class** ([CHILDREN.md](CHILDREN.md)):
  the claim completes the standard SE key handover (the receiver co-owns `A_child` — invariant across
  the rotation, which keeps the pre-signed exit chain valid — and the sender is permanently locked
  out), and the child pays onward off-chain WHOLE (`child_retransfer`) or SPLIT
  (`child_in_ladder_pay`, a depth-2 `ancestors` chain). One co-signature and exactly one disclosed
  superseded state per hop, which the receiver's census counts and proves out-raced. sdk60
  (alice→bob→carol, funding outpoint unspent throughout) + sdk17 (partial second hop).
* **Lightning both directions on the ladder** through the HODL latch (sdk63–68,
  [LIGHTNING.md](LIGHTNING.md)). The LN-latched piece is the ONE case that stays terminalized (it
  sits unclaimed past the pending-transfer lock's window).

Several E2Es named in the tables below have since been **retired** with the machinery they tested.
Every proof cell that named one now points at the live test carrying the claim; where a claim was
subsumed rather than re-proven, that is said outright.

## Done (verified green, pushed)

| Phase | Commit | Proof |
|---|---|---|
| P0 plan + parity matrix + research notes | 5533b34 | docs/utexo/{PLAN,PARITY,research/} |
| P1 primitives port (mercurylib colored signing, rgb.rs, bridge, server single_use+epoch, rgb-lib minimal) | 508ced8 | RGB_E2E=1,2 |
| P2 Rust SDK (`mercury-utexo-sdk`): frictionless wallet, off-chain split amount-maker, **branch-carrying transfers** | 1cfbcd3 | SDK_E2E=1 + upstream suite |
| P3 tokens on RGB (issue/balances/off-chain transfer, verified-contract booking) | 61c5e84 | SDK_E2E=2 |
| P4 lightning latch swap legs | eebc570 | SDK_E2E=3 (retired) → LN now proven on the ladder by sdk63 (pay), sdk64 (receive), sdk67 (in-ladder receive) |
| P6 docs suite (learn/ + build/) | 2e4d280 | docs/utexo/ |
| P7 adversarial SDK suite | 975818c | SDK_E2E=4 |
| P5 nodejs binding (`mercury-utexo-sdkd` daemon + @mercury/utexo-sdk) | 769fd34 | stdio protocol driven live |
| P8 final verification 13/13 | 439e2f4 | SDK1-4 + RGB1-8 + upstream in one pass (sdk03 has since been retired; its LN legs live in sdk63-68) |

## Parity audit + new methods (2026-07-04)

Fresh 3-way audit (SDK surface, protocol/proto/SIP, docs.spark.money) vs our impl. ~80% parity;
remaining gaps are either N/A-by-design (leaf renewal, token freeze, preimage VSS, operator
discovery, wallet-privacy, connector-tx atomicity, instant/SSP-liquidity deposits, webhooks->events)
or ecosystem/partner (Privy, Grid, bridges, LNURL). Buildable user-facing gaps CLOSED:

| Method | Status | Where |
|---|---|---|
| signMessage/validate (P-B) | **DONE** | sign_message_with_identity_key / validate_… (stable identity key m/1000h/0h/0h); unit + sdk11 |
| Spark `transferV2` multi-recipient (P-D) | **DONE** | transfer_many(recipients) — one split -> N pieces + change; laddered parents take a multi-child in-ladder split (B1-safe); sdk11, sdk69 |
| Utexo invoices (P-E) | **DONE** | create_sats_invoice/create_tokens_invoice/fulfill_utexo_invoice (+ decode/expiry); unit + sdk11 |
| get_transfers/get_transfer, list_coins (P-A) | **DONE** | wallet activity + coin inventory; sdk11 |
| getWithdrawalFeeQuote (P-F) | **DONE** | get_withdrawal_fee_quote via electrum estimatefee; sdk11 |
| getTokenL1Address, queryTokenTransactions (P-C) | **DONE** | get_token_l1_address / query_token_transactions; sdk11 |

SPEC §13 (REQ-26..30, ERR-11) + PARITY rows flipped to DONE. Unit tests 17 pass. E2E: sdk11.

## Spec, tests, traced run (2026-07-04)

| Item | Status | Notes |
|---|---|---|
| SPEC.md | **DONE** (512fa4b) | normative spec: roles/trust, data model, full SE API + state transitions, deposit/transfer/split/combine, tokens (RGB, consignment amount, IFA/mint/burn/batch), LN swaps (atomicity), exits+cost, invalidation invariants, error semantics; numbered REQ/INV/ERR with a traceability table mapping each to a test. |
| Unit tests | **DONE** (512fa4b) | mercury-utexo-sdk: 13 pass + doctest — exit-cost math, terminal predicate, error semantics, select invariants (INV-9), split fee/change (INV-10), envelope serde + amount-hint (REQ-21), preimage-hash (INV-14). `cargo +stable test -p mercury-utexo-sdk`. |
| New E2E | **DONE** | sdk09 (IFA issue+mint+batch, G3), sdk10 (terminal-parent verify honest+adversarial, G1) — sdk10 has since been retired; the receiver-side guard it held is now sdk58 (11 adversarial `verify_child_bundle` census cases) plus sdk54/sdk55, and the un-laddered ancestor check rides along in every colored transfer (see the G1 row below). Runner auto-discovers whatever `main.rs` dispatches (today: the live SDK set through 68 + RGB 1-14). |
| Traced launch | **DONE** | clients/tests/run_all_suites.sh (TRACE=1 -> client reqwest/rgb-lib logs; per-test docker-log snapshots of server/lockbox/electrs). Server RUST_LOG enabled in compose (rocket request logs) + fixed a pre-existing web-block YAML quoting bug so compose can recreate services. Full traced run: TRACE=1 LOGDIR=/tmp/utexo_suite_logs ./clients/tests/run_all_suites.sh |
| Adversarial log review | **DONE** | performed and documented in [REVIEW.md](REVIEW.md) (per-dimension finders over the real code + full-suite trace logs: 13 candidates → 10 confirmed → fixes incl. MuSig2 nonce reuse, budget monotonicity, branch value inflation; new tests sdk08/09/10/12; SPEC §14 assumptions) — sdk08 and sdk10 have since been retired: the exit/terminality properties they held are now sdk50 (unilateral exit down the ladder) and sdk58 (11 adversarial cases); sdk12 stays live and is where the protocol-agnostic SE guards (MuSig2 secnonce single-use, unlock authorization) are tested and NOWHERE else. Superseded by the 2026-07 mainnet audit ([AUDIT-2026-07.md](AUDIT-2026-07.md)). |

## Gap closure (2026-07-04) — all three remaining gaps CLOSED

| Gap | Status | Proof |
|---|---|---|
| G1 receiver-side terminal-parent verification | **DONE** (f8ede8a) | TransferMsg.terminal_parents; receiver GET /statechain/spend_budget per ancestor requires terminal; SDK records ancestor chain per sub-coin. sdk10 (its original proof) is retired, the **claim and the code are not**: `transfer_receiver::verify_terminal_parents` still gates every un-broadcast-funding claim, one terminal ancestor per structural INPUT (`required_terminal_ancestors`, Σ inputs), and it is the UN-LADDERED shape's receiver-side guarantee — exercised end-to-end by the colored flows (sdk02, sdk29, sdk31 multi-input combine, sdk39, sdk52) and adversarially by sdk37 [3]/[4] (the SSP pre-payment value gate runs the same branch + terminal-ancestor checks and fails closed). On the LADDERED shape the equivalent receiver-side guard is the `verify_child_bundle` census: sdk58 (child accepted; 11 attacks REJECTED, incl. non-terminal parent and hidden lower-CSV state), sdk54/sdk55. |
| G2 consignment-derived token amounts | **DONE** (d9e043a/c337354) | rgb-lib offchain_assigned_amount + bridge accept_offchain_amount; receiver books consignment amount, envelope amount is a cross-checked hint. sdk02 GREEN. |
| G3 IFA + mint + burn + batch | **DONE** (f8ede8a) | issue_inflatable_token / mint_tokens (on-chain inflate bound to coin) / burn_tokens / batch_transfer_tokens (N recipients, one split). sdk09 GREEN. |

## Historical — RLN + SSP directive (2026-07-03)

Requirements: real Lightning flows over UTEXO-Protocol/rgb-lightning-node (both directions,
tested on regtest); an actual **SSP service**; practical unilateral exit (tests + cost
calculation); old-state invalidation Spark-grade or better (review Ark/Second/SuperScalar);
single-use+epoch inside trees, decrementing backups on flat coins.

| Step | Status | Notes |
|---|---|---|
| RLN-1 harness: RLN API survey + 2-node regtest LN setup + BOLT11 smoke | **DONE** | rln.rs harness; LN_SMOKE=1 green (real channel + payment). RLN bin: build UTEXO fork w/ `git submodule update --init` first. Hodl invoices: /lninvoice payment_hash param + /claimhodlinvoice. Latch facts: preimage release gated on locked=false (confirm-first) ✓ receive-flow atomic as-is; batch EXPIRY kills the claim (no auto-unlock) → pay-flow needs SE unlock-by-preimage extension. |
| RLN-2 SSP service + both LN directions | **DONE** | the external-hash SE latch (migration 0004: external payment_hash + /transfer/paymenthash/external + /transfer/unlock/preimage — unlock by presenting the LN preimage); SspService (sdk ssp.rs) + mercury-ssp HTTP bin; SDK pay_lightning_invoice / create_lightning_invoice; SDK_E2E=5 GREEN (Mercury→LN: coin latched to invoice hash, SSP pays over real channel, preimage unlocks coin + proves payment), SDK_E2E=6 GREEN (LN→Mercury: zero-on-chain wallet receives real LN payment as a coin; preimage release gated on coin release). **Repointed (one protocol):** sdk05/sdk06 are retired. Both directions now run over the LADDER through the HODL latch ([LIGHTNING.md](LIGHTNING.md)) — sdk63 (coin → LN pay; the SSP's pre-pay census, `verify_bundle` over the conveyed ladder with `num_sigs` read from the enclave sig-count, runs before `send_payment`), sdk64 (LN → coin; the SSP fronts its own coin under a HODL invoice and the SE reveals the preimage only once the payee's coin is claimable), sdk67 (in-ladder receive), sdk65 (non-exact pay through the in-ladder split), sdk66/sdk68 (payment-failure rollback / reclaim). The external-hash SE latch endpoints described here are unchanged and still live (`/transfer/paymenthash/external`, `/transfer/unlock/preimage`). The LN-latched piece is the one case that stays terminalized. Defect fixed in the sweep: **D3** — the one-call Lightning PAY API minted through `ensure_exact_coin` and so refused every laddered coin, i.e. every coin. Deploy gotcha: crashed container → docker cp works while stopped, exec/touch needs it running; recreate resets code (recopy ALL changed files incl. lib/). |
| RLN-3 practical unilateral exit | **DONE** | estimate_exit_cost(coin) -> {branch_txs, vbytes, fee_sats_at(rate), wait_blocks}; unilateral_exit returns per-coin ExitStatus (branch instant, backup waits locktime) instead of erroring non-final. SDK_E2E=7 GREEN: sub-coin exit = 2 txs, 267 vB, 534 sats @2sat/vB, 990-block wait mined through; zero SE involvement. Ladder facts: create_tx1 qt=0 -> h+initlock fresh ladder per coin (deposits AND sub-coins); transfers decrement by interval; branch txs locktime-free. **Repointed (one protocol):** sdk07 is retired → **sdk50** proves the public `unilateral_exit()` walking the TES-R chain (trigger → extension → state) as each relative CSV matures, reporting `wait_blocks` between tiers, until the funds land at the wallet's own seed-derived backup address — no absolute-locktime backup is broadcast; the 11 adversarial cases around a split child's exit are sdk58, and a token carrier's depth-2 exit is sdk39. The branch/backup figures and the "decrement by interval" fact above describe the UN-LADDERED shape's signed-once backup chain, which is still exactly how RGB carriers and un-broadcast sub-coins exit; a laddered coin exits down its tiers instead and never ages while idle. Defect fixed in the sweep: **D4** — a child routed to a UNILATERAL exit was marked WITHDRAWING although it has no withdrawal tx, so status polling errored forever. |
| RLN-4 invalidation: Spark-grade or better | **DONE** | learn/invalidation.md (Spark/Ark/Second/SuperScalar comparison + our layered model + measured exit-cost table). NEW SE enforcement: spend-budget (migration 0005, /statechain/spend_budget POST owner-signed + public GET) — SDK sets budget=+1 on every split parent (plain + colored) → SE refuses ANY later co-sign on it; SDK_E2E=8 GREEN (post-split parent withdraw REFUSED; terminal state publicly queryable). Fresh per-sub-coin initlock ladders (depth doesn't consume lifetime — better than Spark's shared decrement); epoch deadline = Ark-style bound WITHOUT expiry-sweep; exact-amount splits > Spark's fixed leaves + SSP swaps. Regression: SDK_E2E=1,2 green with the budget guard. **Repointed (one protocol):** sdk08 is retired → the spend-budget/terminality enforcement it proved is unchanged and live, covered by sdk58 (after an in-ladder split the parent IS terminal — budget consumed by SP — and a bundle claiming a non-terminal parent is REJECTED) and read directly in sdk04, sdk31, sdk32, sdk60. **Superseded reasoning:** TES-R replaces the initlock-decrement + epoch-deadline model — tiers are RELATIVE CSV and un-broadcast, so idle coins never age, rent is 0 vB, and renewal is unbounded off-chain (sdk43, sdk42 lifecycle). The standalone invalidation E2Es (sdk26/sdk27) are retired as **obsolete, not repointed**: there is no aging left to invalidate, and the ladder plus terminality subsume the property. Likewise sdk28 (sats granularity — the in-ladder split carries its own fee model: `tier_out_total` / `committed_fee_for_outputs` / `min_child_value`) and sdk33 (auto-refresh — no ladder floor to approach). Defects fixed in the sweep, both in that fee model: **D1** the in-ladder split admission guard used the old backup-fee floor (442 sat) although a child funds its OWN two tiers + dust (`min_child_value` = 1306 sat at 2 sat/vB), so admitting below it terminalized the parent and THEN failed, stranding it; **D2** `refresh_sponsored` sized its rebate into that same dead window, so a sponsored refresh failed after the user had already paid the on-chain fee (sdk30). |

## How to resume

1. Read this file (start with **Status** above — the tables under it are history) + docs/utexo/PLAN.md
   + PARITY.md, then the shipping protocol: PROTOCOL.md, CHILDREN.md, LIGHTNING.md.
2. Stack: `rgb-lightning-node/regtest.sh start` + `docker compose -f docker-compose-lockbox.yml up -d`
   (colima: `export DOCKER_HOST=unix://$HOME/.colima/default/docker.sock`, binaries in `~/bin`).
3. Suites from `clients/tests/rust`: `ML_NETWORK=regtest [SDK_E2E=n|RGB_E2E=n] cargo +stable run`.
4. Server code changes deploy via docker cp + in-container touch + restart (compose build caches).
