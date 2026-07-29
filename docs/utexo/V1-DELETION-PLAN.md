# V1 deletion plan — the road to "no V1"

**Status (2026-07-28): 18 of the 20 remaining V1-pinned tests are cleared. TWO remain — `sdk17`
(needs first-class children, the last feature gap) and `chaos22` (the only concurrency test). Once
those land, the `protocol_version < 2` lane and the `UTEXO_PROTOCOL_DEFAULT` /
`deposit_protocol_version` escape hatch can be deleted, followed by the docs sweep.**

The standing goal is "V2 default, V1 completely deleted, docs match current design." V2 is the default
(`config.rs deposit_protocol_default → 2`). The V1 lane is a small set of `protocol_version` branches
(`transfer_receiver.rs:409/667/721/1019`, `transfer_sender.rs`, `split_coin`, the V1 backup-ladder
builders) plus the config escape hatch — it can only be removed once NO test pins V1.

## How this migration is run (the discipline)

1. **Never delete a security assertion.** A test is retired only when its surviving property is
   covered by a V2 test that has been re-verified GREEN *in the same session*; if a property is unique
   to the dying test, back-fill it into the covering test FIRST (as was done for sdk35 → sdk45).
2. **Every migration is verified on the live stack** before commit — compiling is not evidence.
3. **Rewrites are per-test, not a bulk unpin.** Most V1-pinned tests assert a V1 MECHANISM (absolute
   locktime aging, `split_coin` sub-coins, backup-chain clawback) that TES-R structurally removes, so
   the assertions must be re-expressed in V2 terms, not merely re-run.
4. **Repoint doc coverage claims** for every test id deleted (see below).

## Full migration map (2026-07-28)

A 21-agent classification pass produced a per-test verdict + concrete migration plan for all 20
remaining V1-pinned tests, saved verbatim at `V1-MIGRATION-MAP.json` (read the `migration_plan` and
`risk` fields before touching a test). Headline: **19 of 20 need NO feature work** — they are test
rewrites or retirements. Exactly ONE genuine feature gap exists:

- **sdk17 (out-of-round chain)** — V2 cannot re-transfer a RECEIVED in-ladder split child off-chain
  (`ctesr-` child bundles are exit-only and excluded from re-conveyance). This is the known
  child-re-transfer design item. **RESOLVED — not a decision:** `V2-CHILD-FIRSTCLASS.md` records the
  design as SETTLED after four adversarial cycles ("Implement"), and its enabling primitive (the
  pending-transfer lock) already landed in commit 19e6668. The reopen approach is explicitly OUT; the
  sound design is to convey the child WITH the standard Mercury key-handover, so the sender's share is
  rotated out and the child becomes first-class. **The V1-lane removal is gated on implementing it.**

## Status (2026-07-28) — 18 of 20 cleared, 2 remain

Every migration is verified GREEN on the live V2 + RLN regtest stack before commit; every retirement is
gated on its covering V2 test being verified GREEN **first** (never drop a security assertion).

**Migrated + GREEN (13):** sdk36 (derived tokens → `in_ladder_pay`), sdk23 (RGB-over-LN, pure unpin),
sdk19 (receive-failure), sdk20 (adversarial gate), sdk24 (receive-cancel), sdk25 (delayed-claim),
sdk21 (remote SspClient, both directions over HTTP), sdk37 (SSP value gate → census-bound amount),
sdk04 + sdk12 (adversarial), sdk15 (fresh double-sign → conflicting V2 triggers), sdk30 (re-anchor +
both fee models), sdk32 (tokens over time → terminal-freeze semantics).

**Retired (5)** — each only after its covering V2 test was re-verified GREEN in the same session:
sdk13 + sdk14 (← sdk51 watchtower defends a hostile trigger; sdk40 PART 2; sdk50; sdk45; sdk58 H/I),
sdk33 (← sdk43 unbounded off-chain renew→rollover→renew), sdk18 (← sdk68 + sdk66),
sdk35 (← sdk45, **after** back-filling its two unique assertions into sdk45: the watch bundle carries
no key material, and a second independent tower is idempotent).

**Remaining (2):**

| Test | Needs |
|---|---|
| `sdk17_oor_chain` | needs **Commit C** (child-level in-ladder split). Commits A + B LANDED and VERIFIED: a received child is now first-class and re-transferable off-chain whole (`sdk60`: alice→bob→carol, `F` unspent throughout, +1 co-sign and +1 disclosed superseded state per hop, carol exits). sdk17's hop 2 is a NON-EXACT split of a child (10k of bob's 20k), which needs a depth-2 `ancestors` chain in `ChildTesrBundle` — plan §C1-C6. Un-pinned today it fails CLEANLY ("insufficient balance"): the planner refuses to split a child rather than fall through to the B1-unsafe path. |
| `chaos22_concurrent_users` | the ONLY concurrency test — retiring is invalid. Swap `split_coin`→`in_ladder_pay` in the DAG actions and retarget the `steal_after_split`/`steal_after_send` cheats to the V2 clawback vector (a superseded ladder tier / hidden low-CSV state, the sdk54/55 shape), keeping the oracle's outpoint-single-spender + value-conservation + double-custody invariants. |

### Doc coverage claims must be repointed when a test dies

TRUST-MODEL.md cited `sdk10`, `sdk13`, `sdk14`, `SDK_E2E=35` and `sdk03/05/06` as **evidence** for
security properties. 13 such claims were repointed to their live V2 equivalents (`sdk58`, `sdk51`,
`sdk40` PART 2, `sdk45`, `sdk63/64/67`, `sdk19/20/24`, `sdk66/68`). A retired test must never leave a
coverage claim behind it — re-grep `docs/` for every id you delete.

## Defects the migration exposed (fixed here, not test artifacts)

**D1 — in-ladder split admission guard was too weak, and could STRAND the parent.**
`in_ladder_pay` admitted a piece that merely cleared the V1 backup-fee floor
(`min_split_output` = dust + backup fee = 442 sat at 2 sat/vB). But an in-ladder CHILD is not a bare
output: `establish_child` hangs the child's OWN extension + state tiers off `SP.out[j]`, each burning
`committed_fee + P2A_VALUE` (488 sat each), and the final state output must still clear dust — a real
floor of **1306 sat**. Worse, `establish_child` runs *after* `set_spend_budget(parent, 1)` and the
`SP` co-sign, so a piece in the 442..1306 window terminalized the parent and *then* died with
`FeeTooHigh`, leaving the parent stranded to unilateral-exit-only.
Fix: new `mercurylib::tesr::min_child_value(fee_rate, dust)` and `in_ladder_pay` now takes
`max(backup-fee floor, min_child_value)` as its admission guard — refusing BEFORE the parent is
touched, the same discipline `split_coin` already used.

**D2 — `refresh_sponsored` was broken on V2** (found by sdk30 part (c), which failed with
`re-anchor succeeded but the sponsor rebate failed: FeeTooHigh`). It sized the operator's rebate at
`fee + DUST_LIMIT` = 442 using V1 reasoning, which lands exactly inside the D1 window, so a sponsored
refresh could not complete — after the user had already paid the on-chain re-anchor fee. Fix: the
rebate is now `max(fee + DUST_LIMIT, min_child_value)`; the operator absorbs the difference and the
user still ends ≥ whole.

Both were latent in the shipped V2 default and are only reachable through small off-chain payments —
exactly what a V1-era test suite never exercised on the V2 lane.

### Run recipe (learned the hard way)

```
cd clients/tests/rust
SDK_E2E=<n> ML_NETWORK=regtest \
  RLN_REGTEST=$HOME/Claude/rgb-lightning-node/regtest.sh \
  COMPOSE_FILE=$HOME/Claude/rgb-lightning-node/compose.yaml \
  cargo +stable run
```

- **`cargo +stable` is mandatory** — the default `cargo` in a non-interactive shell resolves to 1.83,
  which cannot parse `utexo-rgb-lib`'s `edition2024` manifest.
- **`RLN_REGTEST` is mandatory** — without it the harness looks for `esplora-container` (absent) and
  dies with "No container found".
- If bitcoind has been idle/restarted it re-enters IBD and electrs stalls on
  "waiting for 0 blocks (IBD)"; mine 2 blocks (`generatetoaddress`, wallet `miner`) to clear it.

Then: remove the `protocol_version < 2` branches + the `UTEXO_PROTOCOL_DEFAULT` / `deposit_protocol_version`
escape hatch, and the docs sweep.
