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

## Why not now

The deletion is **all-or-nothing at the lane level** (one shared code path), and its prerequisites
(porting/retiring invalidation, refresh, granularity, stale/race for V2) are a large program in their
own right — starting the lane removal before them would break 29 tests and could silently drop a feature
that lacks a V2 equivalent (guardrail: never delete an assertion to make things pass). LN — the piece
that was actually blocking and the immediate goal — is done and verified. This plan is the entry point
for the remaining program; step 1 (feature parity) is the next unit.
