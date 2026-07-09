# Testing guide

All suites live in `clients/tests/rust` and run against the local regtest stack.

## Stack

```bash
cd rgb-lightning-node && ./regtest.sh start                       # bitcoind + electrs + RGB proxy
cd mercurylayer && docker compose -f docker-compose-lockbox.yml up -d   # SE + lockbox + token server
```

Environment (from `clients/tests/rust`):

```bash
export ML_NETWORK=regtest
export RLN_BITCOIND_CONTAINER=rgb-lightning-node-bitcoind-1   # for the test faucet
```

> The tests crate pins an old toolchain in `rust-toolchain`; run with `cargo +stable run`
> (rgb-lib is edition-2024). Each flow wipes `wallet.db*` and its rgb data dirs at start.

## Suites

### SDK E2E (`SDK_E2E=n cargo +stable run`)

| n | Flow | Proves |
|---|---|---|
| 1 | `sdk01_wallet_flow` | deposit → exact-subset transfer → auto-claim → **off-chain-split transfer** (branch-verified) → auto-claim → cooperative exit |
| 2 | `sdk02_token_flow` | issue 1000 TKN → **off-chain token transfer** (consignment in-message, verified contract) → balances 750/250 → exit |
| 3 | `sdk03_lightning_swap` | latch swap: hash verified with SE, claim locked pre-settlement, preimage settles the hash |
| 4 | `sdk04_adversarial` | SDK guard rails: typed refusals, split-parent double-spend refusal, honest branch accept, idempotent claims, double-withdraw refusal |
| 5 | `sdk05_lightning_pay` | Mercury → Lightning via SSP: pay a real BOLT11 from statechain balance |
| 6 | `sdk06_lightning_receive` | Lightning → Mercury via SSP: receive a real LN payment as a statechain coin |
| 7 | `sdk07_unilateral_exit` | practical unilateral exit: branch out instantly, mine past backup locktime, exit with zero SE involvement |
| 8 | `sdk08_terminal_node` | terminal-node enforcement: SE refuses any co-signature on a split parent |
| 9 | `sdk09_ifa_batch` | IFA issuance + mint + batch token transfer |
| 10 | `sdk10_terminal_parent_verify` | receiver terminal-parent verification (adversarial) |
| 11 | `sdk11_parity_methods` | parity methods: identity signing, multi-recipient sats, Spark invoices, queries |
| 12 | `sdk12_adversarial` | adversarial regressions: single_use sub-coins, honest branch accept, nonce reuse |
| 13 | `sdk13_stale_state` | stale-state broadcast rejected/defeated + watcher detects |
| 14 | `sdk14_watcher_race` | watcher race |
| 15 | `sdk15_fresh_doublesign` | fresh double-sign |
| 16 | `sdk16_onboarding` | onboarding |
| 17 | `sdk17_oor_chain` | out-of-round chain |
| 18 | `sdk18_pay_failure_reclaim` | Lightning PAY failure + reclaim: unroutable pay → SSP claims nothing → reclaim |
| 19 | `sdk19_receive_failure` | Lightning RECEIVE failure: never paid → no preimage, receiver can't claim |
| 20 | `sdk20_adversarial_gate` | adversarial SSP gate: wrong-recipient + undersized → SSP refuses to pay |
| 21 | `sdk21_remote_sspclient` | remote SspClient over HTTP: pay + receive against a deployed mercury-ssp server |
| 23 | `sdk23_rgb_ln_swap` | RGB assets over Lightning: issue → colored channel → asset invoice → pay |
| 24 | `sdk24_receive_cancel` | LN → Mercury receive aborted after payment: SSP cancels HODL invoice → payer refunded |
| 25 | `sdk25_receive_delayed_claim` | adversarial delayed-claim: receiver past the SE latch window gets nothing, payer refunded |
| 26 | `sdk26_invalidation_scale` | invalidation at scale: depth-4 off-chain split chain, 3-wide fan-out, deepest-leaf unilateral exit |
| 27 | `sdk27_invalidation_time` | invalidation over time: one-interval ladder decrement, exit-maturity boundary, audit-[17] deadline gap, SE epoch gate |
| 28 | `sdk28_granularity_sats` | granularity (plain sats): exact-subset payments, exact off-chain split, sub-dust refusal + 330-sat min piece, depth-2 re-split |
| 29 | `sdk29_granularity_tokens` | granularity (RGB tokens): raw-unit precision, depth-2 token exit, spent-carrier change → plain BTC, one-carrier-per-transfer limit |
| 30 | `sdk30_refresh` | refresh / re-anchor: reset a coin's ladder + root deadline in one on-chain tx; old backups dead, user pays the fee |
| 31 | `sdk31_token_combine` | multi-carrier token combine: pay an amount spanning several carriers via one colored combine; receiver requires ALL carriers terminal |
| 32 | `sdk32_token_over_time` | tokens over time: idle past every ladder horizon, then not-lost / cooperative send / unilateral materialization / received-token clawback window |

> `SDK_E2E=22` is the concurrent chaos test — see [its dedicated section](#concurrent-chaos--property-test-sdk_e2e22) below. The
> full dispatch lives in `clients/tests/rust/src/main.rs` (`SDK_E2E=1..32`).

### Off-chain DAG primitives (`RGB_E2E=1..14`)

The low-level suite under the SDK: off-chain split (1), 2-input combine (2), 2-deep un-broadcast
chain (3), SE single-use refusal (4), 3-input combine (5), 3-level DAG (6), epoch deadline (7),
wide combine (8), blinded/witness send-receive (9), history + self-transfer (10), UDA/CFA schemas
(11), validate-offchain negative (12), consignment integrity (13), metadata + IFA supply (14).

### Upstream Mercury suite (default `cargo +stable run`)

The vanilla protocol tests (tb01–tb05, tm01, ta01–ta03, tv01): simple transfer, address reuse,
atomic transfer, **lightning latch**, timelock, sender-double-spend, deposit edge cases. Run this
after any change to transfer/receiver code — the branch-transfer extension must stay
regression-clean against it.

### SDK unit tests

```bash
cargo +stable test -p mercury-utexo-sdk   # coin selection, config, doctest
```

## Adversarial coverage map (mirrors Spark)

| Spark test theme | Covered by |
|---|---|
| double-claim / duplicate leaf | ta02, ta03 (duplicate deposits), tm01 (sender double-spend) |
| conflicting off-chain spend | RGB_E2E=4 (SE single-use refusal) |
| wrong preimage / locked claim | SDK_E2E=3 (claim locked until settle; hash must match), tb04 |
| transfer interrupt / resume | tb01+tb02 paths; claim() is idempotent per message |
| exit-race ordering | timelock ladder (tb05) + branch-first materialization (SDK exits) |
| invalid consignment | receiver hook rejects (validate_offchain_chain) — sdk02 asserts the valid path; invalid-path unit lives in rgb-lib fork tests |

## Concurrent chaos / property test (SDK_E2E=22)

A soak test for the bugs that only appear under real parallel usage. `CHAOS_USERS` wallets
(per-user sqlite db) run weighted-random actions CONCURRENTLY against the live SE + lockbox —
enter (deposit), send, receive (claim), split, unilateral exit, cooperative withdraw — plus a
low-probability **cheat** (capture a coin's backup, legitimately send the coin away, then broadcast
the now-stale backup to claw it back). A background miner confirms deposits + matures exits; a
semaphore caps concurrent SE co-signing; all bitcoin-core shell-outs serialise through one mutex.
Every attempt+result is traced to `{run_dir}/chaos.jsonl`.

After a quiescent settle a **spec-invariant oracle** (`chaos22_oracle`) audits the trace + final
live state:

- **No value created** (INV-1/13/25): Σ SE-side balances + Σ exited-on-chain ≤ Σ deposited (tight:
  the residual is realised fees).
- **No cheat succeeded** (INV-5/18/19): every stale-backup broadcast was refused, and on-chain the
  funding outpoint was never spent by the cheater's stale tx (`spender_of` backstop).
- **All outcomes expected**: `classify()` separates spec-sanctioned contention (insufficient
  balance / no-coin / terminal-410 / nonce-409 / mempool-conflict / batch-lock / infra load-shed)
  from unclassified errors; any unclassified error is a breach.

```bash
# smoke (fast): 5 users, 20s
SDK_E2E=22 CHAOS_USERS=5 CHAOS_SECS=20 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run
# full: 100 users, 120s, 8 whales
SDK_E2E=22 CHAOS_USERS=100 CHAOS_SECS=120 CHAOS_WHALES=8 CHAOS_INFLIGHT=24 ... cargo run
```

Gated OUT of the default `run_all_suites` sweep (runs only when `SDK_E2E=22` is set) so CI stays
fast. **This test found three real robustness bugs** (SE worker panics on DB-pool exhaustion; a
client panic on an unexpected SE error body) — see the P2-1 fixes; the protocol stayed *safe*
(value conserved, cheats refused) throughout, only liveness failed. RGB-over-chaos is a follow-up
(the harness runs pure-sats today).
