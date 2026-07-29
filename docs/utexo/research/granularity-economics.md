# Granularity economics — the price of exact amounts

Companion to [invalidation-economics.md](invalidation-economics.md) (entry/exit/size/time pricing —
this page **extends** it and links into its tables instead of restating them),
[GRANULARITY-SPEC.md](../GRANULARITY-SPEC.md) (the normative GRN-REQ/GRN-INV/GRN-ERR boundaries
this page prices), [learn/granularity-deep-dive.md](../learn/granularity-deep-dive.md) (long-form
mechanics), [learn/transfers.md](../learn/transfers.md) (the amount-maker),
[learn/tokens.md](../learn/tokens.md), [learn/exits.md](../learn/exits.md),
[learn/invalidation-deep-dive.md](../learn/invalidation-deep-dive.md) (§3b split walkthrough, §4
ladder budget), [SPEC.md](../SPEC.md) (REQ/INV/ERR) and
[INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md) (IVL-*).

This page prices *partial amounts*: what it costs to pay 0.1 BTC out of a 1-BTC coin, or 0.1 TKN
out of a 1-TKN allocation, when the mechanism is a single SE-co-signed, un-broadcast split tx
minting {exact piece, change} as off-chain sub-coins (the SE is blind to all amounts; bounds are
enforced client-side — sender constructs, receiver independently validates, REQ-15/INV-9/INV-10).

**Scope — one protocol, two coin SHAPES.** Laddering is unconditional: `claim()` establishes a TES-R
ladder (trigger `T` → extension `X_m` → state `S`) for every fresh confirmed ROOT coin
(`clients/libs/rust-sdk/src/wallet.rs:444-505`). Both shapes below are current; they price
differently, and `transfer()` routes each to its own executor (`transfer.rs:130-170`):

- **LADDERED** — every plain deposit. A partial payment out of a laddered coin is the **in-ladder
  split** (`in_ladder_pay`, `transfer.rs:783`; out of a received child, `child_in_ladder_pay`,
  `transfer.rs:677`). Its fee model is *not* the one the tables below open with: there is no
  `clamp(parent/100, 300, 2000)` reserve and no 155-vB branch hop — the miner fee is pre-committed
  into each tier tx and the binding floor is `min_child_value`. Priced in **§1a**.
- **UN-LADDERED** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation — terminal-freeze, [PROTOCOL.md §5.10](../PROTOCOL.md); `wallet.rs:458-472`,
  asserted by sdk52), and a split sub-coin whose funding `F` is un-broadcast cannot root a trigger
  [B0] (`wallet.rs:490-503`). These keep the signed-once backup chain and transfer by backup-chain
  handover, and `split_coin`'s reserve + dust + backup-fee arithmetic is exactly their arithmetic.
  This is the load-bearing shape for **every RGB token flow** — §§2-3 and 6-8 price it.

`split_coin` hard-refuses a laddered coin ([B1], `transfer.rs:540-560`), so the two fee models never
mix on one coin.

**Anchors.** Figures are inherited from
[invalidation-economics.md](invalidation-economics.md) (its anchor table): backup tx **112 vB**,
split branch hop **155 vB**, depth-1 exit **267 vB**, 3-wide fan-out split **241 vB**, exit-fee
tables by depth (§3b there), reserve-as-%-of-value table (§5 there). ⚠️ **Evidence note:** the
plain-split anchors (155 / 241 / 267 vB and the plain depth table) were measured by sdk26 and
sdk28, both **retired** under TES-R — no live E2E measures a plain-split branch hop today, so read
them as *computed* from code constants plus the live unit model (`granularity_model.rs`), not as
*measured* (§8). Live measured evidence on this page is sdk29 (colored split 198 vB, depth-2
colored exit, leaf backup), sdk31 (2-input combine 255 vB) and sdk39 (depth-2 token exit). New
constants this page prices (all verified in code):

| Constant | Value | Source |
|---|---|---|
| Dust floor, every split output | **330 sats** | `clients/libs/rust-sdk/src/transfer.rs:1425` (`DUST_LIMIT`, shared by planner + split guard); enforced again in `lib/src/transaction.rs:357-360` before the SE co-signs — all **sender-side**; receiver-side `validate_branch` checks linkage/locktime/conservation/scripts but *not* output standardness (residual — §8) |
| Split admission (un-laddered) | `piece + reserve < parent` (strict) AND `piece ≥ 330` AND `change ≥ 330` | `transfer.rs:1449-1471` (`split_amounts_floored`; INV-10 / GRN-INV-1) |
| Dust floor / min splittable parent | **330 / 960 sats** (960 = 330 + 300 + 330, exact — see §2) | `transfer.rs:1449-1471` (GRN-INV-1), computed |
| Min **mintable** piece (backup-fee floor, un-laddered) | **`330 + ceil(112·fee_rate)` = 442 sats @1 sat/vB** (computed; unit-pinned by `granularity_model.rs::backup_fee_floor_is_the_true_mintable_minimum`) | `transfer.rs:1437` (`min_split_output`) + `lib/src/transaction.rs:122-132` (GRN-INV-1b) — the piece's own backup must sweep above dust |
| Fee reserve per split (un-laddered) | `clamp(parent/100, 300, 2000)` sats | `transfer.rs:1416` (`split_fee_reserve`) |
| **In-ladder** tier fee (laddered) | `committed_fee_for_outputs(n) = ceil((124 + 43·(n−1))·rate)` **+ 240-sat P2A anchor**, baked into every tier tx | `lib/src/tesr.rs:41,44,334-347` (`P2A_VALUE`, `TIER_VBYTES`, `P2TR_OUT_VBYTES`, `tier_out_total`) |
| **In-ladder** child floor `min_child_value` | `2·(committed_fee + 240) + 330` = **1,306 sats @2 sat/vB** (computed) — a child funds its OWN extension + state tier before clearing dust | `lib/src/tesr.rs:75-77`; enforced up-front at `transfer.rs:819-826` (root) and `:704-710` (child-of-child) |
| Token piece packaging | **`TOKEN_PIECE_SATS` = 1,500 sats** | `clients/libs/rust-sdk/src/tokens.rs:23` (GRN-REQ-11) |
| Min token carrier | **~2,242 sats** at 1 sat/vB (1,500 + 300 + 442 backup-fee floor; dust-only derivation is 2,130 — see §3/§8) | `tokens.rs` colored-split floor + `lib/src/transaction.rs` (GRN-INV-6) |
| Extra split output | **+43 vB** each (*model*, exactly consistent with the 241 = 155 + 2·43 fan-out figure) | P2TR output weight — now a named code constant on the ladder side too, `lib/src/tesr.rs:334` (`P2TR_OUT_VBYTES = 43`); the 241-vB fan-out that confirmed it was measured by the retired sdk26 (§8) |
| Colored split OP_RETURN | **+43 vB** (measured, sdk29: colored split = 198 vB vs plain 155) | one opret per colored tx, `clients/libs/rust/src/rgb.rs:246-271` |
| Coin amount cap | u32 ⇒ **4,294,967,295 sats ≈ 42.9 BTC** | [SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review); split path errors via `u32::try_from` (`transfer.rs:320,328`) |

**Units.** Sats everywhere; token amounts are raw u64 units (precision is contract metadata only —
the SDK never scales; "0.1 token" exists iff precision ≥ 1). USD parametric at **$100,000/BTC ⇒
1 sat = $0.001**. Labels: **measured** (from a real tx), **computed** (exact arithmetic from code
constants), ***model*** (estimate pending measurement).

---

## 1. The price of exactness: split vs whole-coin handover

This section prices the **un-laddered** shape — `split_coin`'s reserve arithmetic, i.e. RGB carriers
and un-broadcast-funded sub-coins. The laddered equivalent (`in_ladder_pay`) is §1a; the two share
the *shape* of the trade (nothing on-chain now, weight and floors later) but not the numbers.

Both operations are off-chain and put **nothing on-chain now**. They are not equally free:

| | Whole-coin handover (exact subset) | Split (exact piece + change) |
|---|---|---|
| Sats deducted now | 0 | **reserve = `clamp(parent/100, 300, 2000)`** — deducted from the parent at split time, consumed as the branch tx's miner fee on any future exit path, cooperative or unilateral (`wallet.rs:517-565` materializes the branch via `broadcast_branch_if_any` at `:551-553` before withdrawing) |
| Deposit-token slots consumed | 0 (a key handover reuses the coin's slot) | **2** (piece + change) — but **DERIVED**, i.e. free SE vouchers against the parent rather than the paid pool (`take_derived_tokens`, `wallet.rs:352`, `transfer.rs:579`; REQ-35, sdk36); only if the parent is unknown does it fall back to a paid slot at `T` sats per [invalidation-economics §2a](invalidation-economics.md#2a-on-chain-deposit) (`SdkError::TokenPaymentRequired`). Fan-out/colored variants: `transfer_many` N+1 (`transfer.rs:380`), `transfer_tokens` 2 (`tokens.rs:583`), batch N+1 (`tokens.rs:1111`) |
| Future exit weight added | 0 (the coin's exit path is unchanged; only its ladder decrements — [invalidation-economics §4a](invalidation-economics.md#4a-ladder-capacity-hops-vs-wall-clock)) | **+155 vB shared branch hop** appearing in *both* the piece's and the change's exit paths (broadcast once by whoever exits first), plus one extra 112-vB backup (two sub-coins each need one where the parent needed one) |
| Depth added | 0 | +1 for piece **and** change (fresh leaf ladders; root deadline unchanged — IVL-INV-10) |
| Trust cost | 0 | 0 — the SE co-signs a hash, blind to amounts |

**Granularity premium by parent size** (computed; the reserve-as-%-of-value column restates
[invalidation-economics §5](invalidation-economics.md#5-size-effects) for self-containedness —
the piece bounds are new). Payable piece range is `[330, parent − reserve − 330]` at **1-sat
resolution** (any integer in the range; nothing rounds — GRN-INV-2):

| Parent | Reserve | % of parent | Pre-paid branch feerate (reserve/155) | Piece range |
|---|---|---|---|---|
| 1,000 | 300 | 30% | 1.9 sat/vB | [330, 370] |
| 10,000 | 300 | 3% | 1.9 | [330, 9,370] |
| 100,000 | 1,000 | 1% | 6.5 | [330, 98,670] |
| 1,000,000 | 2,000 | 0.2% | 12.9 | [330, 997,670] |
| 10,000,000 | 2,000 | 0.02% | 12.9 | [330, 9,997,670] |
| 100,000,000 | 2,000 | 0.002% | 12.9 | [330, 99,997,670] |

Integrator rule (extends [invalidation-economics §4c(iii)](invalidation-economics.md#4c-worked-one-year-tco)):
a whole-coin hop costs nothing; every split is a *reserve-sats-now + two-derived-slots +
155-vB-later* event, charged once and inherited by two coins. `transfer()` already prefers exact subsets (REQ-15,
`select::plan_with_floor` tries subset-sum before planning a split, `select.rs:89-137`); §5 shows how
to make exact subsets common.

## 1a. The price of exactness on a LADDERED coin (in-ladder split)

A laddered coin cannot be split as plain BTC ([B1] — a prior owner's no-timelock trigger could void
the split; `transfer.rs:540-560` refuses it). Its partial payment is the **in-ladder split**: `SP` is
a STATE tier spending `X_m.out[0]` (a descendant of the trigger, never a rival for `F`) paying two
children — the PIECE to the recipient (conveyed with the standard key handover) and the CHANGE back
to us (`in_ladder_pay`, `transfer.rs:783-900`; sdk59 end-to-end, sdk58 for the receiver's
`verify_child_bundle`). The pricing dials change completely:

| | Un-laddered split (§1) | In-ladder split (laddered) |
|---|---|---|
| Sats taken now | `clamp(parent/100, 300, 2000)` reserve (~1% of parent) | `SP` costs `ceil(167·r) + 240` = 574 sats @2 sat/vB, but 488 of that is what the coin's own state tier costs anyway ⇒ **marginal +86 sats** for the second payload output (`tier_out_total`, `lib/src/tesr.rs:346`) |
| Per-child standing cost | one 112-vB backup each | each child's OWN extension + state tier: `2·(committed_fee(r) + 240)` = **976 sats @2 sat/vB**, pre-committed, spent only on a unilateral exit. Two children ⇒ **2,038 sats all-in** for the split |
| Minimum piece **and** change | `min_split_output(r)` = 442 @1 sat/vB | `max(min_split_output(r_backup), min_child_value(r))` = **1,306 @2 sat/vB** — `min_child_value` binds (`transfer.rs:819-826`) |
| Minimum splittable coin | 960 sats (§2) | `X_m.out[0] ≥ 2·1,306 + 574` = **3,186 sats @2 sat/vB**, i.e. a deposit `F ≥ 4,162` once `T` and `X_m` have taken their own 488 each (computed) |
| Slots consumed | 2 derived | 2 derived child slots (`take_derived_tokens`, `transfer.rs:830`) |
| Future exit weight | +155 vB shared branch hop | the shared `SP` (167 vB at 2 payload outputs) + each child's two 124-vB tiers, all fee-pre-committed at `r` and fee-bumpable through the 240-sat P2A anchor |
| Depth added | +1 (fresh leaf ladders under the root deadline) | +1 ladder level; a split **of a received child** adds a depth-2 `ancestors` chain (`child_in_ladder_pay`, `transfer.rs:677-770`; sdk17) |
| Received piece's onward life | sub-coin, moves by backup-chain handover | **first-class child**: paid onward WHOLE (`child_retransfer`) or SPLIT (`child_in_ladder_pay`), one co-signature and one disclosed superseded state per hop, counted by the receiver's census — sdk60 (alice→bob→carol, funding outpoint unspent throughout), sdk17 (partial second hop) |

**Shape of the premium.** The in-ladder fee is **flat**, not proportional: 86 marginal sats on `SP`
plus 976 per child at the default 2 sat/vB committed rate ⇒ **2,038 sats for a two-way split**,
whatever the coin is worth (computed). Against §1's reserve on the same parent:

| Parent | Un-laddered reserve | In-ladder all-in (2 children) |
|---|---|---|
| 10,000 | 300 (3%) | 2,038 (**20.4%**) |
| 100,000 | 1,000 (1%) | 2,038 (2.0%) |
| 1,000,000 | 2,000 (0.2%) | 2,038 (0.2%) |
| 10,000,000 | 2,000 (0.02%) | 2,038 (0.02%) |

So the in-ladder split is several times dearer on small coins, breaks even around ~1M sats (where
the reserve hits its 2,000-sat cap) and is flat above that — regressive at the bottom exactly like
the reserve floor of §1, but with a much higher absolute floor and no cap in the other direction.
The dial is the ladder's `committed_fee_rate` (`TesrParams`, default 2 sat/vB), not the reserve's
implied 1.9-12.9 sat/vB band (§7). What the extra sats buy is real: the tiers are pre-signed,
self-relaying and P2A-bumpable, and the coin has no calendar deadline (§5).

**Whole-coin sends stay free on this shape too** — and now on the receiving end as well, because a
received child re-transfers whole for one co-signature with nothing on-chain (sdk60). §5's
denomination-spread argument therefore applies unchanged, and applies *harder*: the laddered split
floor is ~3× the un-laddered one.

## 2. Sats bounds: the unpayable-amounts map

All bounds in this section are the **un-laddered** ones (`split_coin` on an RGB carrier or an
un-broadcast-funded sub-coin). On a laddered coin substitute the §1a floors at the default rates —
1,306 instead of 442 per output, and 3,186 on `X_m.out[0]` (a ~4,162-sat deposit) instead of a
960-sat parent — and read the band *structure* (greedy shadow, planner off-by-one) as unchanged,
because it comes from the planner, which both shapes share.

The exact executor boundary (`split_amounts_floored`, `transfer.rs:1449-1471`, verified):
`piece + reserve >= parent` **errors** (so the fit is strict), and `change = parent − piece −
reserve` must be `≥ 330`, as must the piece. Therefore **parent = 960 splits** (330 piece + 300
reserve + 330 change, all boundaries met exactly); 959 fails (change 329). A naive strict-headroom
derivation gives 961; the executor's inclusive dust bound (`change >= 330`) makes it **960
exactly** (GRN-INV-1; pinned by the unit boundary matrix
`granularity_model.rs::split_bounds_exact_boundary` — the *planner* still wants 961, below).

**The 330 dust floor is not the minimum *mintable* piece.** A 330-sat split output is valid, but
the sub-coin's own backup tx (112 vB) must sweep `330 − ceil(112·fee_rate)`, which is below dust —
so `create_tx1` rejects it (`FeeTooLow`, GRN-INV-1b). The true minimum mintable piece (and change)
is `330 + ceil(112·fee_rate)` = **442 sats at 1 sat/vB** (computed; unit-pinned by
`granularity_model.rs::backup_fee_floor_is_the_true_mintable_minimum`, which also pins 554 @2 and
1,450 @10), scaling with feerate.
The tables below label the low end "330 (dust floor)" for the executor's bare admission math, but
the smallest piece that yields a *usable* coin is 442+ at 1 sat/vB. The planner
(`plan_with_floor`) and every split path now enforce `min_split_output(fee_rate)` up-front (GRN-INV-1b,
fixed), so targets in `[330, 442)` are refused cleanly with the coin untouched — never stranded.
Read the "330" rows as "442 at 1 sat/vB" for real payability.

Given a wallet's coin set, `transfer(address, t)` resolves per `select::plan_with_floor`
(`select.rs:89-137`):

1. `t >` total balance ⇒ `Insufficient` (ERR-9).
2. An exact subset sums to `t` ⇒ payable at **zero premium** (DP subset-sum).
3. Otherwise: greedily take the largest coins `≤` the remaining target; let `rem` be the residue.
   Payable iff `rem ≥ 330` **and** some unused coin satisfies
   `coin > rem + reserve(coin) + 330` (strictly, `select.rs:104-112`, audit [29] / GRN-REQ-5).

The refusal surface this creates (all computed):

- **`t < 330` is unpayable unless an exact subset exists.** A sub-dust residue can never be minted
  (unit test `sub_dust_remainder_is_refused`, `select.rs:178-183`): with coins {500, 300},
  target 600 is refused despite an 800-sat balance.
- **A near-total band is unpayable.** Wallet {50,000; 20,000; 1,200} (reserves 500/300/300):
  targets 70,570–71,199 are refused — after handing over 50k + 20k the residue exceeds what the
  1,200-sat coin can mint — while 71,200 (the exact triple) works, as does the payable window
  **70,330–70,569** just below the band. The refusal surface below 70,570 is *not* empty either:
  see the greedy-shadow bands next.
- **The planner is 1 sat more conservative than the executor.** `plan` demands
  `coin > rem + reserve + 330` where `split_amounts` admits `=`: target 70,570 above is refused
  by `plan` even though `split_coin(1,200 → 570)` is admissible (change exactly 330). Same at the
  global floor: `plan` needs a 961-sat coin for a 330-sat residue; `split_coin` accepts 960.
- **On a LADDERED coin the planner errs the other way — it is too PERMISSIVE.**
  `plan_with_floor` is called with `min_split_output(fee_rate)` (442 @1 sat/vB) on every path
  (`transfer.rs:97`), but the in-ladder executor demands `max(min_split_output, min_child_value)` =
  **1,306 @2 sat/vB** (§1a). A residue in `[442, 1,306)` therefore gets planned and then refused by
  `in_ladder_pay`'s admission guard. The refusal is **clean and up-front — the parent is untouched
  and still fully spendable** (`transfer.rs:819-826`), which is the whole point of that guard: it
  previously reused the un-laddered 442 floor, admitted the child, terminalized the parent and only
  then died with `FeeTooHigh`, stranding the parent to unilateral-exit-only. Fixed; the same
  discipline as `split_coin`'s (see §8).
- **Greedy shadow: some fundable targets are refused.** Coins {1,000; 970}, target 1,300: greedy
  takes 1,000 first, residue 300 < 330 ⇒ `Insufficient{available: 1970}` — yet handing over 970
  and splitting 1,000 for a 330 piece is admissible (330+300 < 1,000, change 370). `plan` explores
  one whole-coin ordering (largest-first) and never backtracks (`select.rs:98-112`). The general
  shape: after every whole-coin prefix greedy can reach, the next **329 targets** strand a
  1–329-sat sub-dust residue and are refused, even when a different decomposition is
  executor-admissible. Manual `split_coin` + `transfer` works around it.

Worked map for wallet {50,000; 20,000; 1,200} (balance 71,200; computed by simulating `plan`'s
exact algorithm over every target in [330, 71,200] — **2,604 targets are refused**, in the seven
bands below; everything else is payable):

| Target | Outcome | Path / reason |
|---|---|---|
| 329 | REFUSED | sub-dust residue, no exact subset |
| 330–441 | REFUSED (below the mintable floor) | clears the 330 dust floor but not the backup-fee floor (442 at 1 sat/vB, GRN-INV-1b) — the piece could not fund its own backup; refused up-front, coin untouched |
| 442 | payable | split 1,200 → piece 442, change 458 (the true minimum mintable piece at 1 sat/vB) |
| 1,200 / 21,200 / 71,200 | payable, zero premium | exact subsets |
| 1,201–1,529 | REFUSED (greedy shadow) | greedy takes 1,200 whole, strands a 1–329-sat residue — though splitting 50,000 directly for the same piece is executor-admissible |
| 10,000 | payable | 1,200 whole + piece 8,800 from 20,000 |
| 20,001–20,329 / 21,201–21,529 / 50,001–50,329 / 51,201–51,529 | REFUSED (greedy shadow) | same 329-wide shadow after the whole-coin prefixes 20,000 / 21,200 / 50,000 / 51,200 (e.g. 20,001 → whole 20,000, residue 1 < 330; a 20,001-sat piece split straight out of 50,000 is executor-admissible) |
| 70,001–70,329 | REFUSED (greedy shadow) | shadow after the 70,000 prefix (50k + 20k); fundable by hand via 50k + 1,200 whole + an 18,801–19,129-sat piece from 20,000 |
| 70,330–70,569 | payable | 50k + 20k whole + piece 330–569 from 1,200 (change 570–331) |
| 70,570 | REFUSED (planner) | planner off-by-one; executor-admissible by hand |
| 70,571–71,199 | REFUSED | residue exceeds every remaining coin's max piece |
| 71,201+ | REFUSED | ERR-9, over balance |

The six greedy-shadow bands (329 targets each, 1,974 in total below 70,570) are all fundable by a
manual decomposition; only the last two bands are genuinely unpayable without new coins. An
integrator quoting "payable amounts" from balance alone will be wrong in exactly these bands.

The map is drawn at the un-laddered floors. Held as **laddered** coins the same wallet reads
differently in one direction only: the low bands widen (`330-441` becomes `330-1,305` at the default
2 sat/vB committed rate) and the 1,200-sat coin cannot be in-ladder split at all (< 3,186), so it
participates as a whole coin or not at all. The greedy-shadow bands, their 329-wide shape and the
planner off-by-one are identical — they are properties of `plan_with_floor`, which both shapes share.

**Boundary hazard (the boundary *arithmetic* is unit-pinned by `granularity_model.rs`; the
budget-burn *ordering* below is what remains untested — GRANULARITY-SPEC §11 item 7. The E2E that
exercised the sats boundary, sdk28, is retired: it described the plain-split lane as the default
payment path, which it no longer is):**
The window is narrower than it was: `transfer_many` now filters its parent on
`amount > total + reserve + min_output` and rejects any recipient below `min_output` before it
touches a coin (`transfer.rs:344-368`), and `transfer_tokens`/`batch_transfer_tokens` re-check the
backup-fee floor on every piece and the change after the fit guard (`tokens.rs:558-578`,
`tokens.rs:1091-1108`). What remains is the *ordering*: a parent that clears those guards but is
still rejected by the lib's own dust check has already had its spend budget clamped to 1
(`transfer.rs:406`, `tokens.rs:614`, `tokens.rs:1146` — all set **before**
`lib/src/transaction.rs:357-360` runs). The coin is
not lost — the budget's one remaining co-sign still allows one *corrected retry split* or a
cooperative withdraw (GRN-REQ-8 note), and `sign/first` alone does not consume it — but the
failed attempt irreversibly made the coin single-remaining-use (set_sig_budget is monotonic,
INV-24). A whole-coin *transfer* would also go through, but it spends the last co-sign on the
receiver's backup, so the coin arrives terminal at the SE — unilateral-exit-only for the
receiver; do not recommend it as a recovery.
`split_coin` itself is immune: it validates before touching the budget (`transfer.rs:570-572` vs
`:624`), and so is the in-ladder split, whose `min_child_value` guard runs before the parent is
terminalized (`transfer.rs:819-826`, §1a).

## 3. Token packaging economics

This whole section is the **un-laddered** shape, and that is by design, not by omission: an RGB
carrier is **never** laddered (terminal-freeze, [PROTOCOL.md §5.10](../PROTOCOL.md) rule 1 — a plain
T/X/S tier spend would close the seal without a transition and destroy the allocation), so `claim()`
skips it when it establishes ladders (`wallet.rs:458-472`) and the numbers below are the live ones
for every token flow. Proven on the live stack by **sdk52**: in one wallet the plain coin carries a
ladder, the token carrier carries none, and an off-chain RGB transfer still settles.

Every token piece is a sub-coin carrying **exactly 1,500 sats** (`TOKEN_PIECE_SATS`,
`tokens.rs:23`; GRN-REQ-11); the sats are packaging, the RGB allocation is the payload. The change keeps
`carrier_sats − 1,500 − reserve` and the token remainder (`tokens.rs:565-566`); a full-allocation
spend leaves the change **uncolored** — plain, freely splittable BTC (`rgb.rs:250-255`), asserted by
sdk29 part (c) (a plain `split_coin` on the spent-carrier change succeeds where it was refused
while the coin still carried the allocation).

**Minimum carrier = 2,130 sats** (GRN-INV-6), via two guards: the SDK's typed
`"carrier coin too small"` error fires for carriers ≤ 1,800 (`1,500 + reserve ≥ carrier`,
`tokens.rs:558-563`); carriers of 1,801–2,129 pass it but produce sub-330 change and are rejected
by the lib dust guard before the SE co-signs (`lib/src/transaction.rs:357-360`) — after the
spend-budget clamp of §2's hazard note. 2,130 is the clean floor (change exactly 330).

**What one token payment costs (computed):**

- **Now, sender:** 1,500 sats *transferred* to the receiver (packaging moves with the payload;
  `TransferResult.total_sats = 1,500`, `tokens.rs:661-665`) + reserve (300–2,000) *committed* to
  future exit fees + **two deposit-token slots** (piece + change) — **derived**, i.e. free vouchers
  against the carrier (`take_derived_tokens`, `tokens.rs:583`; REQ-35, sdk36), falling back to a
  paid slot at `T` sats only if the parent is unknown
  ([invalidation-economics §2a](invalidation-economics.md#2a-on-chain-deposit)). Sender's sats
  position drops by `1,500 + reserve` per payment regardless of token
  amount; the token amount itself is conserved exactly (INV-13, rgb-lib-enforced).
- **Batch (`batch_transfer_tokens`):** N×1,500 + **one** reserve + N+1 derived slots
  (`tokens.rs:1111`), one colored split, one shared consignment with per-piece amount-hint
  envelopes.
- **At exit, receiver:** the exit settles the **asset at full value, with no SE** — the branch
  txs are the RGB witnesses; their opret anchors confirm with them and the allocation becomes an
  ordinary on-chain holding (INV-16 / GRN-INV-14,
  [learn/tokens.md](../learn/tokens.md#exits-with-tokens)). The **1,500-sat packaging does not
  come back**: the piece's only pre-signed spend is its PLAIN backup (`create_tx1`,
  `transfer.rs:441-449`), which is RGB-unaware — broadcasting it would spend the seal outpoint
  without an RGB transition and **burn the allocation** — and the SDK hard-errors if you name the
  piece in an exit for exactly this reason (the claim registers it as a carrier,
  `tokens.rs:944-961`; `wallet.rs:725-746`). For a holder keeping the token, the 1,500 sats stay
  **parked on the exited 2-of-2 outpoint** (asserted by sdk29): recoverable only by destroying
  the token, or via an SE-co-signed colored spend that is not shipped (GRN-INV-14).

**What is the packaging worth at exit?** For a holder keeping the token: **zero at every
feerate** — it is a sunk cost from the moment the piece is minted (above). The table prices the
*salvage value of the burn option* (broadcast the plain backup, destroying the allocation —
equivalently, roughly what a future SDK colored exit could hope to recover): solve for the market
feerate r at which even that salvage vanishes:

| Regime | Equation | Salvage-zero r\* | Label |
|---|---|---|---|
| Backup weight alone (value/weight of the sweep) | `1,500 = 112·r` | **≈ 13.4 sat/vB** | computed |
| + CPFP child needed (backup fee frozen at 112; child ~111 vB) | net `= 1,500 − 223·r` | ≈ 6.7 sat/vB | *model* |
| + colored branch hop still unconfirmed (~198 vB carrying reserve ≥ 300) | net `= 1,800 − 421·r` | ≈ 4.3 sat/vB | *model* |

(The backup's own fee is frozen at co-sign time at `min(SE quote, max_fee_rate)`, default
1.0 sat/vB ⇒ 112 sats, so the burn option nets ≈ 1,388 sats in the default regime — and backup
*creation* fails outright once `1,500 − fee < 330`, r ≳ 10 sat/vB at co-sign; GRN-INV-14.)

**Carrier depletion (computed from code constants).** Sequential sends recompute the reserve on
the shrinking change (`change_{n+1} = c_n − 1,500 − clamp(c_n/100, 300, 2000)`), terminal when the
change drops below 2,130; a batch pays one reserve for N pieces:

| Carrier sats | Sequential sends | Stranded final change (sats) | Max batch pieces (one split) |
|---|---|---|---|
| 2,130 | 1 | 330 | 1 |
| 10,000 (the default issuance carrier, `tokens.rs:118`) | 5 | 1,000 | 6 |
| 50,000 | 26 | 2,072 | 32 |
| 100,000 | 49 | 646 | 65 |
| 1,000,000 | 311 | 748 | 665 |

An issuer on the default 10,000-sat carrier gets **five** sequential payments (or one 6-recipient
batch) before the carrier's sats — not its tokens — run out. The stranded change still carries the
remaining allocation and can always exit; it can no longer *pay* off-chain.

**Received pieces cannot be forwarded (derived, load-bearing).** A received token piece carries
exactly 1,500 sats — **below the 2,130 minimum carrier** — so the SDK receiver can hold or exit
the tokens but cannot re-send them off-chain (`transfer_tokens` would need
`1,500 > 1,500 + reserve + 330`, impossible). This holds for *any* fixed `TOKEN_PIECE_SATS = P`
(`P ≥ P + 630` never holds): fixed-size packaging makes SDK token rails structurally **one-hop**
(issuer/holder with a fat carrier fans out; receivers terminate). The chained-token tests
(rgb03/rgb06) chain at *lib* level with caller-chosen sats per output — the primitive supports
multi-hop; the SDK's fixed packaging does not. Fixes would be a colored combine/top-up at SDK
level (see §8) or pass-down packaging (piece inherits `carrier − reserve − 330`), each hop then
shedding one reserve (*model*).

**This one-hop limit is specific to fixed RGB packaging — it is not a property of received pieces in
general.** On the laddered side a received in-ladder split child is **first-class**: the claim
completes the SE key handover (the receiver co-owns `A_child`, which is invariant across the
rotation so every pre-signed child tier stays valid, and the sender is permanently locked out), and
the child pays onward WHOLE (`child_retransfer`) or SPLIT (`child_in_ladder_pay`) for one
co-signature and exactly one disclosed superseded state per hop — sdk60 (alice → bob → carol with
the funding outpoint unspent throughout) and sdk17 (partial second hop). Sats recirculate; RGB
pieces still terminate at the receiver.

## 4. Fragmentation economics: depth you make vs width you receive

**Paying k times from one coin** builds a change chain of depth k: the final change exits at
`112 + 155k` vB, and the whole family (k pieces at depths 1..k + final change), if everyone
exits, materializes k branch txs + (k+1) backups = `155k + 112(k+1)` vB (computed from the anchor
constants; the per-level plain-hop measurement retired with sdk26 — see the depth table in
[invalidation-economics §3b](invalidation-economics.md#3b-unilateral-exit--full-pricing). The
colored analogue *is* still measured per level, by sdk29 part (b) and sdk39).

**Receiving k pieces** (from k senders) yields k independent depth-1 coins: k unilateral exits of
267 vB (computed as 112 + 155), or k cooperative exits of branch 155 vB (fee **pre-paid by each
sender's reserve**) + withdraw ~111–140 vB at market. There is **no plain-sats combine**: exiting N
coins is N tx-chains today ([invalidation-economics §3a](invalidation-economics.md#3a-cooperative-withdraw));
the shipped combine is the *colored* multi-carrier one (§8, sdk31).

**Merchant scenario — 100 payments/day received as depth-1 pieces, swept daily** (computed;
withdraw 140 vB; the withdraw CPFPs its own branch when the market exceeds the ~1.9-sat/vB
reserve floor, shortfall `max(0, 155r − 300)` per coin):

| Market rate | Per coin (withdraw + branch shortfall) | Daily (100 coins) | USD @$100k |
|---|---|---|---|
| 1 sat/vB | 140 + 0 = 140 sats | 14,000 sats | ~$14 |
| 10 sat/vB | 1,400 + 1,250 = 2,650 | 265,000 sats | ~$265 |
| 30 sat/vB | 4,200 + 4,350 = 8,550 | 855,000 sats | ~$855 |

The economically sane merchant strategy is therefore **recirculation, not sweeping**: received
pieces re-spent as whole coins (exact subsets, §5) cost nothing; sweep only net flows — subject
to the ladder pace bound on each coin
([invalidation-economics §4a/§4c(i)](invalidation-economics.md#4a-ladder-capacity-hops-vs-wall-clock)).
On the **laddered** shape recirculation is strictly stronger, because a received in-ladder child is
first-class: paying it onward whole is one co-signature with nothing on-chain (sdk60), and the
receiving merchant is not forced to sweep to regain spendability.

**Width beats depth (rule of thumb).** One `transfer_many` fan-out to k recipients is a single
split of `155 + 43(k−1)` vB total (*model*; the k=3 = 241 vB figure that anchored it was measured by
the retired sdk26), i.e.
**~43–60 vB of future branch per piece, all pieces at depth 1** — vs **155 vB and +1 depth per
piece** for k sequential splits, plus one reserve instead of k and k+1 derived slots
instead of 2k (§1). Prefer `transfer_many`/batch whenever paying several parties from one coin
(~2.5–3.6× less future weight). The same 43-vB-per-extra-output arithmetic governs a wide in-ladder
split — it is literally the same constant there (`committed_fee_for_outputs`, `P2TR_OUT_VBYTES`,
`lib/src/tesr.rs:334-344`) — so "width beats depth" holds on both shapes, though on the laddered one
each extra child also brings its own 976-sat two-tier standing cost (§1a).

## 5. Whole-coin sends are free: the case for a denomination spread

An exact-subset payment (REQ-15 `Exact` arm) adds **no depth, no reserve, no future weight** — it
is the only genuinely free payment primitive. The wallet-design consequence: engineer the coin set
so exact subsets are common.

**Suggested ladder for a payments wallet (*model*, derived from the bounds):** binary multiples of
a 10,000-sat granule — {10k, 20k, 40k, 80k, 160k, 320k, 640k, 1.28M, …}:

- **Floor 10k:** hard floor is 330 (dust) but pieces below ~10k are exit-fragile —
  [invalidation-economics §5](invalidation-economics.md#5-size-effects) puts the economic floor
  for *independently exitable* pieces at ~10k sats at low fees (~27k at 10 sat/vB and ~80k at 30,
  by §7's 267·r rule of ten). 10k also sits at the reserve floor (300 = 3%, the regressive
  zone's edge).
- **Binary spacing:** any multiple of the granule up to the ladder sum is an exact subset using
  each denomination at most once; a sub-granule residue costs at most one split.
- **Ceiling:** stay well below the u32 cap (~42.9 BTC); one coin per denomination per expected
  concurrent payment.
- **Replenishment is the premium:** spent denominations are re-minted by splitting a larger coin —
  each re-mint is one §1 event on an un-laddered coin (reserve + two derived slots + 155 vB), or one
  §1a event on a laddered one (~2,038 sats all-in at 2 sat/vB, floor 1,306 per output). The
  denomination spread does not
  eliminate splits; it concentrates them into deliberate, batched maintenance
  (`transfer_many`-style wide re-mints at ~43–60 vB/denomination and k+1 slots, §4) instead of one
  per payment.

**Limits vs Spark's fixed denominations:** Spark leaves are fixed-size outputs; exact amounts are
made by swapping leaves with the SSP (`start_leaf_swap_v2`, adaptor-signature atomicity —
[protocol-notes.md](protocol-notes.md)); here the wallet
self-maintains — no third party, but the maintenance cost (reserves + branch weight +
deposit-token slots) is the holder's, and incoming amounts are arbitrary, so the
spread decays without periodic re-mints. Splitting grants fresh leaf ladders in both systems. On the
**un-laddered** shape the root deadline still bounds the tree's off-chain life (IVL-INV-10 — a
denomination spread does not extend it). On the **laddered** shape there is no such calendar
deadline: BIP-68 relative timelocks only start counting once `T` confirms, so an idle coin never
ages, and renewal is off-chain and unbounded (`X_m` replaced horizontally, forced rollover at
`m_max` — sdk43). Denomination maintenance there is about payment ergonomics, not about beating a
clock.

## 6. Token exit economics at depth

Colored pieces accumulate depth on the **sender's** side (each send deepens the sender's change
chain; the k-th piece minted sits at depth k), so multi-hop colored branches exist even though
received pieces don't forward (§3). Every colored split carries exactly one OP_RETURN opret
(`rgb.rs:246-271`), which is **+43 vB** of extra non-witness weight — **measured by sdk29: a
colored split is 198 vB vs the 155-vB plain split** (8-byte value + 1-byte len + 34-byte script,
the same arithmetic that reproduces the 241-vB fan-out figure). So the "same 112 + 155d as plain
splits" intuition is **wrong by 43 vB per colored hop**:

| Depth d | Plain-split exit (vB) | Colored exit `112 + 198d` (vB) |
|---|---|---|
| 1 | 267 (computed) | 310 (computed: 198 + 112) |
| 2 | 422 (computed) | 508 (**measured**, sdk29 part (b); the depth-2 token exit is also driven end-to-end by sdk39) |
| 3 | 577 (model-linear) | 706 (model-linear at 198/hop) |
| 4 | 732 (computed) | 904 (model-linear at 198/hop) |

(The plain column was measured by sdk26/sdk28, both retired; it now stands as `112 + 155d`
arithmetic over the anchor constants. The colored column is the one with live evidence.)

Depth changes only the sats arithmetic: the burn-option salvage of §3 shifts left by the
+43 vB colored hop, while the packaging remains a sunk cost for a held token at any depth
(GRN-INV-14) and the **asset value is depth-independent** — exit settles the full allocation
on-chain (INV-16) whatever the sats arithmetic. Exit material = branch rows + backup +
consignment; the consignment lives in `BackupTx.rgb_consignment` and is **not seed-derivable**.
The artifact-loss ledger ([invalidation-deep-dive](../learn/invalidation-deep-dive.md) H3
caveat): lose the *consignment alone* (branch + backups intact) and the token becomes unprovable
while the packaging sats survive (the plain backup still sweeps them — with the token proof gone
there is no allocation left worth preserving); lose the *whole recovery bundle* with only the mnemonic
and **both are gone** — the deep-dive's "total loss of every off-chain coin" applies to carriers
too. The consignment is an *additional* non-seed-derivable artifact on top of the branch/backup
rows that already gate the sats. Both exit paths refuse carriers in plain-BTC sweeps and
hard-error on explicit carrier ids (`wallet.rs:526-547,725-746`, audit [6][7]; GRN-REQ-14) — the
SDK cannot burn the allocation on the way out.

## 7. Parameter sensitivity

How the dials interact (`DUST_LIMIT = 330`, `TOKEN_PIECE_SATS = 1,500`,
reserve `clamp(·/100, 300, 2000)` on the un-laddered shape, and `committed_fee_rate` on the
laddered one):

- **330 is standardness-anchored, not tunable:** it is Bitcoin's P2TR dust bound — lowering it
  makes split txs unrelayable (the audit-[9] failure class); raising it raises the min piece, the
  min parent (960 = 2·dust + reserve floor) and the min carrier (2,130 = P + reserve floor +
  dust) linearly.
- **Reserve clamp = a pre-paid feerate band of 1.9–12.9 sat/vB** (300/155 to 2000/155). The floor
  keeps small-parent splits regressive (30% at 1k, §1); the cap means even a whale cannot
  pre-commit more than 12.9 sat/vB to its branch — above that the CPFP regimes of
  [invalidation-economics §3b](invalidation-economics.md#3b-unilateral-exit--full-pricing) govern.
  Raising the cap trades sender-side sats now for exit robustness later; raising the floor
  steepens the small-coin premium.
- **`committed_fee_rate` is the laddered analogue of the reserve — and it is a hard, flat dial**
  (`TesrParams`, default 2 sat/vB). Everything in §1a is linear in it: the split's own
  `ceil(167·r) + 240`, the per-child `2·(ceil(124·r) + 240)`, and hence
  `min_child_value = 2·(ceil(124·r) + 240) + 330` — 1,306 at 2 sat/vB, 1,802 at 4, 3,290 at 10.
  Raising it buys standalone relay confidence for the pre-signed tiers (the P2A anchor is the
  top-up path, not the base case) and directly raises the smallest payable piece; the floor climbs
  ~248 sats per extra sat/vB. Unlike the reserve it has **no cap** and does not scale with coin
  size, so it is the dominant regressive term for small laddered coins.
- **`TOKEN_PIECE_SATS` trades packaging cost against exit economics — but not forwardability.**
  At P sats: per-payment sats cost = P + reserve; the burn-option salvage-zero rate (backup
  alone, §3) = P/112 sat/vB (1,500 → 13.4; 5,000 → 44.6; 750 → 6.7); min carrier = P + 630. **No** value of P
  makes received pieces re-sendable (§3) — that requires variable packaging or combine, not a
  bigger constant. P must also stay ≥ 442 (= 330 + 112) or the piece's own backup output goes
  sub-dust after fee (`lib/src/transaction.rs:123-131`) and the packaging becomes unexitable at
  co-sign time.
- **A 10× feerate world (market ~10–30 sat/vB sustained):** the *dust-derived* floors (330 / 960 /
  2,130) do not move; the fee-derived ones do — `min_split_output` goes 442 → 1,450 and
  `min_child_value` 1,306 → 3,290 at 10 sat/vB — and the *economic* floors scale with r too: an
  independently exitable plain
  piece needs ≥ ~27k sats at 10 sat/vB and ≥ ~80k at 30 for exit ≤ 10% of value (267·r rule of
  ten); every reserve-floor branch (1.9 sat/vB) needs CPFP, so the deadline-pressure regimes bind
  by default. Token packaging is a sunk ~1,800-sat cost per payment at *any* feerate (1,500
  parked on the exited outpoint + ≥ 300 reserve, §3); what the 10× world removes is the residual
  burn-option salvage — gone past both CPFP rows of §3's table at 10 sat/vB, past all three above
  ~13.4. Sunk ~1,800 sats/payment is negligible iff the asset is worth far more than
  $1.80/payment (@$100k), prohibitive for micro-value tokens.

## 8. Known limitations (honesty section)

- **Multi-carrier combine — SHIPPED, and its cost** (GRN-INV-11). `transfer_tokens` now combines
  carriers when no single one suffices: two carriers holding 60 + 50 pay 100 in ONE SE-co-signed
  colored combine (N inputs → piece + change), measured **255 vB for 2 inputs** (`SDK_E2E=31`).
  This is strictly cheaper than the old workaround (two 60 + 40 transfers = 2×1,500 packaging + 2
  reserves, leaving the receiver two pieces ≈ 620 vB of future exit): one combine gives the receiver
  ONE piece (~310 vB future exit) and merges the sender's carriers. The combine input adds ~50 vB
  per extra P2TR input over a plain split; a 2-input combine's exit branch is a single ~255 vB tx.
  The receiver-side safety guards (tree branch, N terminal ancestors, confirmed roots) add no
  on-chain cost.
- **Received token pieces are terminal at the SDK layer** (1,500 < 2,130, §3): hold or exit; no
  re-send. It is a *derived* consequence of the fixed packaging, not a documented decision. The
  equivalent question on the sats side has been **answered**: received in-ladder children are
  first-class and re-transfer whole or split (sdk60, sdk17, §1a/§3), so RGB packaging is now the
  only place where a received piece terminates.
- **In-ladder split admission floor — was a stranding bug, now fixed.** The in-ladder guard
  originally reused the un-laddered backup-fee floor (442 at 1 sat/vB), but an in-ladder child
  funds its OWN extension + state tier before it can clear dust — `min_child_value`, 1,306 at the
  default 2 sat/vB. A child admitted between the two floors passed the guard, the parent was
  terminalized and `SP` co-signed, and only then did `establish_child` die with `FeeTooHigh`,
  leaving the parent stranded at unilateral-exit-only. Both in-ladder paths now take
  `max(min_split_output, min_child_value)` **before** the parent's spend budget is touched
  (`transfer.rs:819-826` root, `:704-710` child-of-child), so the refusal is clean and the parent
  stays fully spendable — the same discipline `split_coin` already had. Two consequences priced
  above: the smallest laddered payable piece is ~3× the un-laddered one (§1a), and the shared
  planner is now the permissive side of the pair (§2).
- **A wallet CAN now receive the same asset twice (fixed).** The accept path was re-importing the
  asset genesis on every receive, so a second separate allocation of an already-held asset failed
  to book (`UNIQUE constraint failed: asset.id`) and the retriable-booking watcher spun on the
  permanent error. `save_new_asset` is now idempotent on a known asset, so balances sum — the
  "pay 60 + 40" workaround for the one-carrier limit works at a single receiver, and a merchant
  can take repeated payments in one asset (verified by sdk29: bob 10 → 11 → 9,996).
- **The minimum *mintable* piece on the un-laddered shape is 442 sats (at 1 sat/vB), not 330 (fixed
  floor)** (GRN-INV-1b, §2). The split guard enforces the backup-fee floor
  `min_split_output(fee_rate) = 330 + ceil(112·fee_rate)`
  on every path before the terminal-guard, so a piece in `[330, 442)` is refused cleanly with the
  parent untouched (previously it was admitted, the parent made terminal, then the backup failed —
  stranding the parent). This slightly raises the effective minimum token carrier too: change must
  clear 442, so the live floor is ~2,242 sats at 1 sat/vB (the 2,130 rows above are the dust-only
  derivation). Unit-pinned by `granularity_model.rs::backup_fee_floor_is_the_true_mintable_minimum`
  (the E2E that measured it, sdk28, is retired — see the evidence note below).
- **Planner conservatism, both directions** (§2): greedy whole-coin selection refuses some fundable
  targets and is 1 sat stricter than the un-laddered executor at every boundary — while being
  ~864 sats *looser* than the in-ladder executor, since it plans against `min_split_output` and the
  in-ladder guard demands `min_child_value`. Both are cheap to fix and neither can strand a coin;
  currently the honest answer to "why did my transfer fail with sufficient balance" may be
  "ordering" or "that piece is below the child floor".
- **Boundary attempts burn spend budget** (§2 hazard, GRN-REQ-8 note): a failed
  `transfer_many`/token split at the dust boundary leaves the parent single-remaining-use
  (monotonic budget) — benign but irreversible.
- **No receiver-side dust re-check.** `validate_branch`
  (`clients/libs/rust/src/transfer_receiver.rs:754-864`) verifies linkage, root confirmation,
  locktimes (INV-4), value conservation and scripts — but **not** the 330-sat floor. The SE is
  amount-blind, so a malicious sender bypassing the lib builder could hand over a branch
  containing a sub-dust (unrelayable) output — the audit-[9]/[11] failure class — and the
  receiver would book it. Today the sender-side guards (`transfer.rs:1425,1449-1471`,
  `lib/src/transaction.rs:357-360`) are the only dust enforcement points; closing this is a code
  change to `validate_branch`, not a doc fact. (The laddered analogue is tighter, though not on dust:
  `build_split_state` enforces `Σ children == tier_out_total(X_m.out[0], n)` exactly — no mint, no
  burn (`lib/src/tesr.rs:369-390`) — and `verify_child_bundle` re-derives every tier's prevout value
  from the decoded txs and checks the co-signature against it, so a fabricated amount fails the
  sighash (`clients/libs/rust/src/tesr.rs:1530+`); sdk58 for acceptance, sdk54 for the adversarial
  cases. The 330-sat floor there is still a sender-side guard, `min_child_value`.)
- **Fixed `TOKEN_BLINDING = 777`** (`tokens.rs:20`): seal blinding is a constant in SDK token
  flows — a design simplification (consignments travel inside owner-encrypted transfer messages,
  and receiver booking trusts only the consignment: REQ-21/22, ERR-8 on hint mismatch), not a
  privacy feature. Randomize when the bindings allow.
- **Consignments are not seed-derivable** (§6): granular token wealth adds a mandatory backup
  artifact beyond the seed.
- **u32 amount cap** (~42.9 BTC): the split path errors cleanly (`u32::try_from`), other paths
  truncate (`as u32`, [SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review)) — stay
  well below.
- **USD figures are parametric** at $100k/BTC throughout; nothing here is a price forecast.
- **Evidence ledger — what is measured, and what lost its measurement.** Now measured: colored-split
  vsize (198 vB, sdk29), token exit at depth 2 (sdk29 part (b), sdk39), the 2-input colored combine
  (255 vB, sdk31), carrier ⊥ ladder (sdk52), first-class children (sdk60, sdk17), in-ladder split
  acceptance and payment (sdk58, sdk59). **No longer measured:** the plain-split anchors — the
  155-vB branch hop, the 241-vB 3-wide fan-out and the plain depth column of §6 — were measured by
  sdk26 and sdk28, and both E2Es were retired under TES-R (sdk26/sdk27 modelled invalidation over
  time, which no longer describes the protocol: an idle laddered coin never ages and the ladder plus
  terminality subsume the old decrementing-locktime pressure; sdk28 exercised the plain-split lane
  as the default payment path, which it no longer is). Those numbers survive as arithmetic over the
  code constants and the live unit model (`granularity_model.rs`, 9 tests) — **stated here rather
  than quietly relabelled**: a fresh E2E measuring a plain split on the un-laddered lane (an RGB
  carrier is the natural subject) would close the gap. In-ladder tier vsizes (`TIER_VBYTES = 124`,
  `P2TR_OUT_VBYTES = 43`) are code constants that no test measures against a built tx either.
  Combine-hop sizes beyond 2 inputs remain unmeasured
  ([invalidation-economics §9](invalidation-economics.md#9-open-items-that-move-these-numbers)).

Mainnet caveat: the system as a whole is **NOT_READY** per
[AUDIT-2026-07.md](../AUDIT-2026-07.md). Post-UPDATE-3 state: all 11 HIGH fund-loss/theft
findings and the griefing/DoS MEDIUMs (LN-atomicity cluster included) are fixed + verified; still
open are the [17] conveyed-locktimes half
([INVALIDATION-SPEC §6.2](../INVALIDATION-SPEC.md)), [13] (bundled with the SGX enclave rebuild)
and [28] (conservative-safe, deferred by design) — plus the operational blockers: SGX rebuild,
full E2E re-run, independent re-audit. The granularity mechanics priced here carry no
*additional* trust cost — the SE never sees an amount.
