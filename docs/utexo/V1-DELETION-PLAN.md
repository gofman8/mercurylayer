# V1 deletion plan — the road to "no V1"

**Status (2026-07-23): SCOPED. The Lightning blocker is CLEARED (LN works fully on V2 — see
`V2-LN-HODL.md`); the remaining blocker is non-LN V1-only feature parity (invalidation / refresh /
granularity). This doc sequences the deletion so it never leaves the suite broken.**

The standing goal is "V2 default, V1 completely deleted, docs match current design." V2 is the default
(`config.rs deposit_protocol_default → 2`) and LN now works end-to-end on V2. But **29 tests still pin
V1** (`UTEXO_PROTOCOL_DEFAULT=1` / `deposit_protocol_version = 1`), and the V1 lane is a single set of
`protocol_version < 2` branches (`transfer_receiver.rs:409/667/693/721/1019`, the flat
`num_sigs == backup_transactions.len()` check at :693, `split_coin`, the V1 backup ladder) serving
**all** of those tests at once. So V1 cannot be removed piecemeal — every feature that still rides the
V1 lane must first be on V2, or be retired.

## The 29 V1-pinned tests, by category

| Category | Tests | V2 status | Action |
|---|---|---|---|
| **LN swap/pay/receive (core)** | sdk03, sdk05, sdk06 | ✅ sdk63/64/65/67 | migrate or drop (V2 covers) |
| **LN failure/cancel/delayed/remote/rgb** | sdk18, sdk19, sdk21, sdk23, sdk24, sdk25 | ⚠️ partial (sdk66 = pay-fail) | **port to V2** (receive-fail, cancel, delayed-claim, remote SSP, RGB-LN) |
| **Adversarial** | sdk04, sdk12, sdk20 | ✅ sdk54/55/57/58 | audit each attack has a V2 analogue, then drop |
| **Terminal / exit / OOR** | sdk07, sdk08, sdk10, sdk17 | ✅ sdk50 (exit), sdk08* | confirm V2 exit/terminal parity, then drop |
| **Stale-state / race / double-sign** | sdk13, sdk14, sdk15 | ⚠️ needs V2 analogues | **port to V2** (the ladder has its own stale/race surface) |
| **Invalidation** | sdk26, sdk27 | ❌ V1-only | **decide: obsolete under TES-R, or port** |
| **Refresh / auto-refresh** | sdk30, sdk33 | ❌ V1-only (re-anchor exists in tesr) | **port to V2** (re-anchor a ladder) or retire |
| **Granularity** | sdk28 | ❌ V1-only | **decide: obsolete under TES-R, or port** |
| **Tokens over time / trust / derived** | sdk32, sdk35, sdk36 | ⚠️ mixed | audit vs V2 token model |
| **SSP value gate** | sdk37 | ✅ (peek census is V2-aware) | re-pin V2 or drop |
| **Chaos** | chaos22 | ⚠️ V1 concurrency model | port to V2 or retire |

\* sdk08 pins V1 but tests terminal-node enforcement, which V2 also enforces (spend-budget); verify.

## Prerequisite (the real blocker): non-LN V1-only feature parity

Before the V1 lane can be removed, resolve each ❌/⚠️ feature — for each, **either** it is genuinely
obsolete under the TES-R (laddered) model (then delete the feature + its test), **or** it needs a V2
implementation + test (then port it). Specifically decide:

1. **Invalidation** (sdk26/27) — V1 invalidated stale off-chain sub-coin states. Under TES-R the ladder
   + spend-budget already make stale states unspendable (INV-19, terminality). LIKELY OBSOLETE — confirm
   the ladder subsumes it, then delete.
2. **Granularity** (sdk28) — V1 minimum-piece economics for off-chain splits. The in-ladder split has its
   own fee model (`tier_out_total`, `committed_fee_for_outputs`). LIKELY OBSOLETE — confirm, then delete.
3. **Refresh** (sdk30/33) — re-anchor a coin to reset its ladder. TES-R re-anchor exists conceptually;
   PORT a V2 refresh test or confirm `transfer()`-driven re-anchor covers it.
4. **Stale/race** (sdk13/14/15) — PORT: the ladder's own stale-state + watchtower race surface (sdk51
   watchtower is a start; add stale-broadcast + fresh-double-sign V2 analogues).

## Sequenced deletion (never red)

1. **Port the ⚠️/❌ features to V2** (or retire with justification), each with a passing V2 test, committed
   individually. This is the bulk of the work and its own program.
2. **Migrate/retire the 29 V1-pinned tests** to their V2 equivalents, one file at a time, keeping the
   suite green after each.
3. **Remove the V1 lane code** once no test pins V1: delete the `protocol_version < 2` branches, the flat
   backup-count check, `split_coin`, and the V1 backup-ladder builders; make `protocol_version` V2-only.
4. **Drop the `UTEXO_PROTOCOL_DEFAULT` escape hatch** and the `deposit_protocol_version` field.
5. **Docs sweep**: remove every "V1"/"V2" distinction from `docs/utexo/**` and the learn/build guides —
   there is one protocol.

## Progress (2026-07-24)

- **9 of 29 V1-pinned tests removed** (suite compiles green after each): LN core sdk03/05/06 (V2:
  sdk63/64/65/67/68), invalidation sdk26/27 + sats-granularity sdk28 (obsolete under TES-R),
  exit/terminal sdk07/08/10 (V2: sdk50/58). Commits 617f1af, 7059c72, 33e0c12. **20 tests remain pinned.**
- **CORRECTION — the "colored in-ladder split" blocker was a MISDIAGNOSIS.** Re-reading the code: the
  `[B1]` "cannot be split" guard lives ONLY in the plain-BTC self-split `split_coin`
  (`clients/libs/rust-sdk/src/transfer.rs:487-509`). The COLORED (RGB) path is entirely separate —
  `colored_transfer` → `create_colored_split_tx` (`tokens.rs:453` / `rgb.rs:201`) — never calls
  `split_coin`, so it never hits B1. It already uses free derived tokens + a spend-budget=1 terminal
  guard. **A colored in-ladder split does not need to be built.** The real remaining work is per-test
  migration, of three kinds:
  1. **sats `split_coin` users** (sdk36 derived-deposit-tokens — a *sats* onboarding-token test, not
     colored): rewrite the `split_coin` + `transfer(piece)` two-step to a single V2 `in_ladder_pay`
     (`transfer.rs:625`), which already uses derived tokens (so the sentinel-pool assertion survives).
  2. **V1-aging assertions** (sdk32 tokens-over-time): its premise — a carrier's backup ladder FLOORS
     after a "year" of idling, creating clawback danger — is exactly what TES-R eliminates (idle coins
     never age). This needs a V2 REWRITE asserting the OPPOSITE property (idle carrier never ages /
     never lost), not an unpin.
  3. **colored path on V2** (sdk23 RGB-LN, colored parts of sdk32): `create_colored_split_tx` +
     `latch_tokens`/`latch_tokens_se_preimage` already exist; needs a live run to confirm behavior and
     one open soundness check (splitting a CONVEYED carrier has no B1-style guard — safe for an
     issuer's own carrier, unverified for a received one).
- **Migration is per-test, not a bulk unpin:** each test needs adaptation + a live verification run —
  a large, careful effort, not a sweep. E2E runs cannot be parallelized (one bitcoind/mercury-server,
  shared `wallet.db` CWD).

## Remaining work, grouped by what it needs

| Group | Tests | Needs |
|---|---|---|
| sats split_coin → in_ladder_pay | sdk36 | rewrite to `in_ladder_pay` (feature EXISTS; no port) |
| V1-aging → V2 "never ages" | sdk32 | rewrite assertions to TES-R semantics (no flooring/clawback) |
| colored path on V2 | sdk23 | live-run `latch_tokens` colored LN on V2; confirm colored split soundness |
| LN scenarios | sdk18, sdk19, sdk21, sdk24, sdk25 | port receive-fail/cancel/delayed/remote to V2 (sdk18 also = the sole batch-expiry test) |
| Stale / race (security) | sdk13, sdk14, sdk15 | port the ladder's stale-state + double-sign race to V2 |
| Adversarial | sdk04, sdk12, sdk20 | adapt (keep agnostic guards, drop V1-split cases covered by sdk58) |
| Refresh / trust / value-gate | sdk30, sdk33, sdk35, sdk37 | migrate to V2 re-anchor / peek census |
| OOR / chaos | sdk17, chaos22 | assess / port |

Then: remove the `protocol_version < 2` branches + the `UTEXO_PROTOCOL_DEFAULT` / `deposit_protocol_version`
escape hatch, and the docs sweep.

## Why not all at once

The deletion is **all-or-nothing at the lane level** (one shared code path), and its prerequisites
(porting/retiring invalidation, refresh, granularity, stale/race for V2) are a large program in their
own right — starting the lane removal before them would break 29 tests and could silently drop a feature
that lacks a V2 equivalent (guardrail: never delete an assertion to make things pass). LN — the piece
that was actually blocking and the immediate goal — is done and verified. This plan is the entry point
for the remaining program; step 1 (feature parity) is the next unit.
