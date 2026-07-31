# CTES-R gate — go/no-go

> **VERDICT: PROCEED WITH CONDITIONS.** All three gating experiments (E1, E2, E7) were run against
> the live regtest stack. None of them blocks CTES-R. Two of the three predictions were partly
> wrong, and in both cases the correction changes the design — not the verdict. Four design rules
> below are now **mandatory and must be decided before the first line of CTES-R code is written**:
> per-tier seal blinding, the `payload_vout` migration as its own commit, a code-enforced
> carrier-stash resolver invariant, and the coloured-tier fee arithmetic.

Status: gate decision, 2026-07-30. Gates §5 stage 6 and §6 items 5/6/7 of
[`COLORED-FORWARDING.md`](COLORED-FORWARDING.md). All `file:line` references are against the
working tree at the time of writing.

**This document corrects four claims in `COLORED-FORWARDING.md`.** They were predictions; they are
now measurements, and they did not all reproduce. See §2.4.

---

## 1. What was run

| | Experiment | Ran against | Reproduced? | Blocks CTES-R? |
|---|---|---|---|---|
| **E1** | v3 colouring smoke test — real TES-R tier txs (P2TR payload + 240-sat P2A, nVersion 3, BIP-68 nSequence) pushed through `RgbWallet::color` | live regtest, bitcoind Core 30.2 @ 77621, electrs, RGB proxy, mercury-server, lockbox | **partial** | **no** |
| **E2** | rival-tier collapse — N rival coloured EXTENSIONs over one T output, same blinding; then both candidate fixes | same stack, rgb-lib fork `feat/spark` @ 1d6fec8 | **partial** | **no** |
| **E7** | `update_witnesses` self-destruct on a permanently-Tentative coloured ladder | same stack | **yes** | **no** |

All three ran from isolated scratch directories. **No file in `/Users/gofman/Claude/mercurylayer`
or `/Users/gofman/Claude/utexo-rgb-lib` was modified**; `clients/libs/rust/src/tesr.rs`,
`clients/libs/rust/src/transfer_receiver.rs` and `clients/libs/rust-sdk/src/ssp.rs` were untouched,
as required by the concurrent security fix that owns them.

Harnesses (read-only exploration + standalone crates):

* E1 — `…/scratchpad/e1/{Cargo.toml,src/main.rs}`
* E2 — `…/scratchpad/e2/{Cargo.toml,src/main.rs,src/bin/e2b.rs}`
* E7 — `…/scratchpad/e7/harness/src/main.rs`, logs `…/scratchpad/e7/run3.log`, `run4.log`

(prefix `/private/tmp/claude-501/-Users-gofman-Claude/85a64326-ebcb-45a8-b89c-cc4f91129508/`)

---

## 2. What each experiment showed

### 2.1 E1 — a TES-R tier CAN be coloured. The core CTES-R premise holds.

**Reproduced.** Two phases, each with a fresh RGB wallet and NIA issuance: a 1-payload tier and a
3-payload tier shaped exactly like `build_split_state`. Both coloured successfully and
`validate_offchain` returned `valid=true` against their **un-broadcast** txids.

**(a) The opret lands at index 0 — CONFIRMED.** Outputs go 2 → 3: `[opret(0), P2TR payload(1),
P2A(2)]`. The 3-payload tier goes 4 → 5: payloads `0,1,2 → 1,2,3`, P2A `3 → 4`. Every vout in the
ladder shifts by one. The trigger for `opreturn_first` is the P2TR **payload** output — the P2A
anchor is *not* P2TR (`is_v1_p2tr=false`, `is_witness_program=true`), so it does not cause the
insertion. Mechanism at `/Users/gofman/Claude/utexo-rgb-lib/src/wallet/rust_only.rs:310-312,
324-326, 374-376`.

**(a′) The "silent fund loss" prediction DID NOT REPRODUCE.** This is a correction, and it is good
news. Every ladder-chaining site inspected is **fail-closed**, because the vout index is always
cross-checked against transaction *content* rather than used bare:

* `verify_bundle_ex` rejects a child spending `out[1]` (`vout != 0`) and requires
  `tx.output[0].script_pubkey == agg_spk` — `out[0]` is now an OP_RETURN
  (`clients/libs/rust/src/tesr.rs:1393, 1403, 1423`);
* `verify_child_bundle` derives `A_child` from the same index it link-checks, and
  `taproot_key_hex` on an OP_RETURN spk fails (`:1729-1741, 1755, 1773, 1779-1780`);
* `lib/src/tesr.rs` `encode()` would read the opret's value `0`, so the child's `tier_out_value(0)`
  returns `None` → `FeeTooHigh` (`lib/src/tesr.rs:200-204`);
* `verify_tier_cosigned` is handed the parent's `out[0]` **value**, and a wrong prevout amount
  breaks the taproot sighash.

So the `payload_vout` migration is an **"everything breaks at once" rewrite across ≥8 sites**, not
an off-by-one that silently loses money. It still needs its own commit and adversarial tests (§3.2)
— but for coordination reasons, not because a missed site drains a coin.

**(b) `Psbt::from_unsigned_tx` accepts nVersion 3 at BOTH layers — CONFIRMED.** Mercury side
(bitcoin 0.30): version preserved = 3. rgb-lib's internal
`Psbt::from_unsigned_tx(transaction).unwrap()` (`rust_only.rs:329`) definitely executed — it sits in
the no-existing-OP_RETURN branch, which is our case — and did not panic. `version=3`,
`nSequence=0x00000024` and `locktime=0` all survive both PSBT round-trips and the base64 re-parse
back into bitcoin 0.30. No PSBT path rejected v3.

**Bonus: the coloured v3 tier is STANDARD.** `testmempoolaccept` (Core 30.2) on the coloured tx with
a dummy 64-byte witness: `reject-reason: "non-BIP68-final"` — i.e. every shape and standardness
check passed and it stopped only at the unmatured CSV. Re-probed at `TRIGGER_SEQUENCE` (no relative
lock): `"mempool-script-verify-flag-failed (Invalid Schnorr signature)"` — the placeholder signature
is the only remaining objection. **Identical results at nVersion=2**, so nothing TRUC-specific
objects to the `(opret, P2TR, P2A)` shape.

**(c) The opret is a stable 43 vB — CONFIRMED — but the fee arithmetic BREAKS.** The opret spk is
`6a20` + a 32-byte MPC commitment = 34 bytes → a 43-byte serialized output, byte-identical in both
runs. `43 == P2TR_OUT_VBYTES` exactly (`lib/src/tesr.rs:337`) — both are 8 value + 1 len + 34 spk.

| | uncoloured | coloured | committed fee | effective rate | shortfall |
|---|---|---|---|---|---|
| 1-payload tier | 124 vB (`= TIER_VBYTES` exactly) | 167 vB | 248 sat | **1.485** sat/vB (target 2.0) | **86 sat** |
| 3-payload split-state | 210 vB (`committed_fee_for_outputs(3, 2.0)` = 420 → **2.000** sat/vB exactly) | 253 vB | 420 sat | **1.660** sat/vB | **86 sat** |

The shortfall is a constant `43 × rate`, **independent of `n`**. The fix is exact and needs no new
constant: `committed_fee_for_outputs(n_payload + 1, rate)` gives 506 sat over 253 vB = **2.000
sat/vB exactly** on the coloured tx. `TIER_VBYTES = 124` and `P2TR_OUT_VBYTES = 43` are both already
correct and need no change.

**(d) `create_colored_split_tx` cannot be reused for tiers — CONFIRMED, and it fails LOUD.** The
P2A spk is `51024e73`: `is_op_return = false`. Its `!is_op_return` vout filter
(`clients/libs/rust/src/rgb.rs:284-298`) therefore counts the anchor as a payload — the 1-payload
tier yields vouts `[1,2]` (len 2) vs `splits.len() == 1`; the 3-payload tier yields `[1,2,3,4]`
(len 4) vs 3. Both trip the `output_vouts.len() != splits.len()` guard, which **errors** rather than
mis-mapping. Note the asymmetry: `create_colored_backup_tx`'s
`position(|o| !o.script_pubkey.is_op_return())` (`rgb.rs:126-131`) *does* still return the right
vout for a 1-payload tier, because the payload precedes the P2A — **correct by accident**, and only
for exactly one payload output.

### 2.2 E2 — the rival-tier collapse is REAL and WORSE than predicted. Two independent fixes both work.

**Reproduced, with the predicted *mechanism* partly wrong.**

**1) Collapse confirmed.** Five rivals `X_0..X_4` over `T:2` with the same blinding (777, no nonce)
produced **one** OpId and **one** BundleId: `all bundle_ids identical? true / all opids identical?
true`, every rival reporting `bundle_id=4f957e4f…cd9918, opid=80158f70…c534df`.

**2) The predicted reason is WRONG in one part.** `GraphSeal` **does** carry a blinding, and that
blinding **is** committed into the OpId (`Assign::conceal → SecretSeal → AssignmentCommitment.seal`,
`rgb-consensus-0.11.1-rc.10/src/operation/commit.rs:334-349`;
`…/src/seals/txout/blind.rs:67`). "`RevealedValue` carries no blinding" is true
(`…/src/operation/fungible.rs:99`) but is not what causes the collapse. **The collapse happens
because the SDK uses ONE GLOBAL blinding constant** — `TOKEN_BLINDING: u64 = 777`
(`clients/libs/rust-sdk/src/tokens.rs:20`) — for every colouring. The BundleId half is structural
and unconditional: `TransitionBundle::commit_encode` commits **only** `input_map`
(`…/src/operation/bundle.rs:145`), so equal `(parent Opout → OpId)` implies an equal BundleId.

**3) The predicted tie-break is WRONG, and the truth is worse.** It is **not** "first-inserted
wins". `bundle_witness_index` is `LargeOrdMap<BundleId, LargeOrdSet<Txid>>` — an **ordered set**
(`rgb-ops-0.11.1-rc.10/src/persistence/memory.rs:921`) — so `bundle_info` iterates rivals in
ascending consensus-serialized (little-endian) txid order and `select_valid_witness` (strict `<`,
`…/src/persistence/state.rs:116-139`) keeps the first on a Tentative tie. **The winner is the rival
with the numerically smallest internal txid: an arbitrary hash lottery, uncorrelated with recency.**
Verified across three independent runs — winner was the 2nd-created, then the 3rd-created, then the
1st-created rival, and in every run it was exactly the internal-txid minimum.

**4) The consequence is not "hop 2 fails opaquely" — it is "hop 2 CANNOT SUCCEED AT ALL".** In the
5-rival run `S` was built over `X_4` but the `S` consignment embedded `X_0`. The receiver then has
**no** branch that validates:

```
TRUE branch     [T, X_4, S] -> valid=false
  "bundle 8deb60bc… public witness 823c60fc… is not known to the resolver."
EMBEDDED branch [T, X_0, S] -> valid=false
  "the provided witness transaction does not closes seal 823c60fc…:1."
```

The allocation is **unclaimable off-chain**. In the 2-rival run the min-txid rival happened to *be*
the current tier, so validation passed — i.e. **the defect is an intermittent ~1/n coin flip that a
small test suite would miss.** That is the dangerous part, and it dictates the test rule in §3.1.

**5) Fix A — unique per-tier seal blinding: WORKS, and needs no fork or bridge change.** Rivals at
blinding 777 vs 888 over the same parent gave distinct OpIds (`1815b5a2…` vs `c78e9eef…`) and
distinct BundleIds (`2c1893b9…` vs `e2787435…`), and the `S` consignment embedded the **true**
parent `X_b` even though `X_a` had the *smaller* internal txid — a clean control, since under
collapse `X_a` would have won. `RgbWallet::color` already takes a per-call `blinding: u64`
(`clients/libs/rust-rgb/src/lib.rs:370`); the only change is replacing the global constant with a
per-tier derived value.

**6) Fix B — `ColoringInfo.nonce`: present in the fork and effective, but not reachable through the
bridge.** `nonce: Option<u64>` exists (`utexo-rgb-lib/src/wallet/rust_only.rs:29,31`) and reaches
`transition_builder.set_nonce()` (`:459`); observed landing as nonce 0/1/2 vs the `u64::MAX`
default; distinct nonces gave distinct OpIds/BundleIds and the correct embedded parent. But
mercury-rgb hardcodes `nonce: None` at two call sites (`clients/libs/rust-rgb/src/lib.rs:391, 439`).
A one-parameter signature change unblocks it.

**Scope caveat, stated plainly:** E2's tiers were nVersion 2, no P2A anchor, unsigned, never
broadcast. The RGB identity question is orthogonal to TRUC/v3/P2A, which is E1's scope. Also note
`static_blinding` doubles as the opret MPC entropy (`set_mpc_entropy`, `rust_only.rs:479`), so
per-tier blinding also varies the opret commitment entropy; no failure was observed from this.

### 2.3 E7 — one routine rgb-lib call silently and irreversibly destroys a coloured ladder.

**Reproduced**, with two corrections that make it *worse*, not better.

The harness built exactly the CTES-R shape against the pinned fork: NIA(1000) issued on a confirmed
on-chain ROOT, then two **un-broadcast** coloured tiers `T1` (spends ROOT) and `T2` (spends `T1:1`),
each nVersion 3 with a CSV-shaped nSequence, each coloured through the same public API mercury uses
(`Wallet::color_psbt_and_consume`). Both were verified absent from bitcoind at every step. The
ladder was also booked into sqlite exactly as mercury does
(`register_statechain_utxo` ×2 + `mark_utxos_spent`).

The "permanently Tentative" premise is confirmed in code: `color_psbt_and_consume` calls
`consume_fascia(fascia, None)` (`rust_only.rs:515`) and `consume_fascia` defaults `witness_ord` to
`WitnessOrd::Tentative` (`utils.rs:790`). Nothing ever moves it, because the tier is never
broadcast.

**1) One call kills it — CONFIRMED.** `wallet.update_witnesses(0, vec![])` returned
`UpdateRes { succeeded: 2, failed: {} }` — it reports **success**, with no error, no warning and no
log of destruction. Immediately after, a read-only `color_psbt` probe over both the tip (`T2:1`) and
the middle rung (`T1:1`) went from OK to:

```
InvalidColoringInfo { details: "total amount in output_map (1000) greater than available (0)" }
```

The on-chain ROOT allocation survived — that is the recursive `set_bundles_as_invalid` walking down
from the first archived witness, and it is also the fail-closed property: nothing becomes
double-spendable, the owner simply loses every off-chain rung. Chain verified end to end: electrum
returns `WitnessStatus::Unresolved` for an unknown txid
(`rgb-ops/src/indexers/electrum_blocking.rs:135`) → `Unresolved` maps to `WitnessOrd::Archived`
(`rgb-consensus/src/validation/validator.rs:113-119`) → `update_witness_ord` records became-invalid
(`rgb-ops/src/persistence/stock.rs:1301-1335`) → `set_bundles_as_invalid` recurses over every child
bundle (`:1212-1232`) → every assignment read is filtered through `check_bundle(&invalid_bundles)`
(`rgb-ops/src/persistence/memory.rs:724, 741, 758, 823, 833, 843`). **The `invalid_bundles` set is
part of the persisted stock (FsBinStore), so it survives process restart.**

**2) "Permanently zeroes the balance" — DID NOT REPRODUCE AS STATED, and the truth is worse.**
`get_asset_balance` reported `settled=1000 future=1000 spendable=1000` **before and after, at every
checkpoint**, including after the mercury-faithful sqlite booking; `list_unspents` kept showing the
tip allocation with `settled=true`. rgb-lib's balance is computed purely from its sqlite tables
(`offline.rs:1315-1318`) and `update_witnesses` touches only the RGB stock. **The observable outcome
is not a zeroed balance but a silent DB↔stock divergence:** the wallet keeps advertising a full,
settled, spendable carrier balance that can no longer be coloured, spent or consigned. There is no
signal anywhere — no error, no balance drop, no transfer-status change. **A monitoring alarm built
on `get_asset_balance` would never fire.**

**3) No repair — CONFIRMED, and the only thing that works is the on-chain unwind.** Tried in order:
`upsert_witness(T1,T2 → Tentative)` (both `Ok`) — still dead; `refresh(online)` — still dead;
`update_witnesses(0, force_witnesses=[T1,T2])` (`succeeded=2`) — still dead. Source confirms why:
the only writer that clears `invalid_bundles` is `maybe_update_bundles_as_valid`
(`stock.rs:1277`), reachable only from step 3 of `update_witnesses`, which requires a witness to
transition Archived → non-Archived *via the resolver*; `upsert_witness` writes the ord directly and
never touches `invalid_bundles`. Then `T1` was signed and **broadcast**: after
`sendrawtransaction` + `update_witnesses`, the `T1:1` probe came back OK — but `T2:1` stayed dead,
because `T2` is still un-broadcast and is re-archived on the same pass. **The only repair rgb-lib
offers is: broadcast every tier you want back, bottom-up, one at a time.** For a CTES-R ladder that
is not a repair — it is the on-chain unwind the ladder exists to avoid, and `X_m`/`S_k` carry
relative CSVs so they cannot even be broadcast until their parent confirms. Recovery cost = a full
sequential on-chain unwind, gated by the CSV deltas.

**4) Is any current code path exposed? NO — verified both ways.** Static:
`grep -rn "update_witnesses|upsert_witness"` over all of `/Users/gofman/Claude/mercurylayer`
(excluding `target/`) returns **zero code hits** — only two prose mentions, in
`docs/utexo/COLORED-FORWARDING.md:516,518`. In the fork, `RgbRuntime::update_witnesses`
(`utils.rs:954-963`) has exactly one caller, the public "special usage" `Wallet::update_witnesses`
(`rust_only.rs:729-741`), plus one unit test; no internal rgb-lib path (`refresh`, sync, `accept`,
`list_unspents`) reaches it, and it is not surfaced in `bindings/`. Dynamic: the routine paths were
run against the *intact* ladder first — `refresh(None)`, `refresh(Some(contract))`,
`list_unspents(Some(online), .., true)`, then 3 blocks mined and refreshed again — and all three
probes stayed OK.

**This is therefore not a live bug today. It is a loaded gun with no trigger wired:** a public,
innocuously-named, doc-blessed-for-"special usage" method that silently and irreversibly destroys a
carrier stash, with no guard anywhere and no repair API.

### 2.4 Corrections to `COLORED-FORWARDING.md`

Four claims in that document were predictions and are now measured. Treat this file as
authoritative on all four:

| `COLORED-FORWARDING.md` | Claim | Measured |
|---|---|---|
| §4 stage 6 (:384-386) | "an off-by-one there is **silent fund loss**, not a red test" | **False.** Every chaining site cross-checks the index against tx content and fails closed (E1 a′). The commit is still separate — for coordination, not for silent loss. |
| §6.6 (:509-511) | collapse is because "`RevealedValue` carries no blinding and the seal commits to the vout only" | **Half false.** `GraphSeal` *does* carry a blinding and it *is* committed. The cause is the single global `TOKEN_BLINDING = 777`. |
| §6.6 (:512) | "`select_valid_witness` keeps the **first-inserted**" | **False.** It keeps the **numerically smallest internal (LE) txid** — a hash lottery. This is why a 2-rival test passes ~50% of the time. |
| §6.7 (:518) | one `update_witnesses` call "permanently **zeroes the balance**" | **False, and worse.** The balance stays at full `settled/future/spendable`; the *stock* goes to zero. Silent DB↔stock divergence with no observable signal. |

---

## 3. The design rules the results FORCE

These are decided here, before any code. They are not discoverable at hop 2.

### 3.1 Seal blinding — MANDATORY, cheap, no fork or bridge change

**CTES-R must colour every tier with a unique seal blinding, deterministically derivable by BOTH
parties, and must never rely on rgb-lib picking the right witness among rivals.**

* Replace the global `TOKEN_BLINDING = 777` (`clients/libs/rust-sdk/src/tokens.rs:20`) with a
  per-tier value, e.g. `blinding = u64::from_be_bytes(H(statechain_id ‖ tier_role ‖ tier_index ‖ rung)[..8])`.
* **Uniqueness must hold over the whole `(parent outpoint, role, index)` space**, because rival
  tiers over the *same* parent output are the NORMAL case in CTES-R: renewal replaces `X_m` over
  `T`'s output, a transfer replaces `S_k` over `X`'s output. Two tiers sharing a parent outpoint
  must **never** share a blinding.
* The receiver must derive the identical value — it needs it for `accept(txid, vout, blinding)` — so
  every derivation input must be data the receiver already has in the transfer message.
* **Optional belt-and-braces:** also set `ColoringInfo.nonce` per tier (monotone with rung/index).
  It works and independently separates OpId and BundleId, but needs a `nonce` parameter added to
  `color`/`color_blinded` (`clients/libs/rust-rgb/src/lib.rs:391, 439`). Do not make CTES-R depend
  on it alone.
* **Invariant to enforce in code, not comments:** after colouring, assert the returned consignment's
  leaf bundle carries `pub_witness == the tier's own txid`. If it does not, the blinding derivation
  has collided — fail loudly at build time rather than letting the receiver hit an unvalidatable
  consignment. Cheap, and it catches the whole class.
* **Test rule (non-negotiable):** any regression test for this MUST use **≥3 rivals** AND
  deliberately choose a **non-minimum-internal-txid** rival as the current tier. A 2-rival test
  passes ~50% of the time by luck — observed.

### 3.2 The `payload_vout` migration — MANDATORY, own commit, but budget it correctly

Colouring shifts every ladder vout by one: payload `0 → 1`, P2A `1 → 2`; for an N-payload split
state, `j → j+1` and P2A `N → N+1`.

* Introduce an explicit `payload_vout` (or `colored: bool`) on the tier/bundle types and **thread it
  through every chaining site** rather than flipping literals. Sites: `lib/src/tesr.rs:203`
  (`encode()` must read the payload output, not `output[0]`) and `:401`
  (`build_split_state`'s hardcoded `vout: 0`); `clients/libs/rust/src/tesr.rs:1393, 1403, 1423,
  1671, 1677, 1706, 1755, 1773, 1779`.
* **On the off-by-one risk, be accurate:** E1 found **no silent-loss path**. Every one of those
  sites cross-checks the index against transaction content (`spk == agg_spk`, `taproot_key_hex` on
  the spk, or the prevout amount feeding `verify_tier_cosigned`'s sighash), so a missed site fails
  closed with a verification error. Design the migration as a **loud, all-at-once cutover proved by
  a covering test** — do **not** gate CTES-R on an off-by-one audit that has no silent failure mode.
* **One site does warrant explicit care, because it is keyed rather than checked:** the
  `live_csv_by_outpoint` census map built at `clients/libs/rust/src/tesr.rs:1706`
  (`live.insert((ext_tx.txid(), 0), …)`). Its key must move to the payload vout **in lockstep** with
  the tiers it describes.
* **Do not hard-code "opret is index 0" anywhere.** `opreturn_first` is triggered by the presence of
  a P2TR output; the P2A anchor is not P2TR. Derive payload vouts from the builder's return value,
  never from a positional assumption.

### 3.3 The resolver invariant — MANDATORY, code-enforced, three places

"Never resolve a carrier's stash with the plain resolver" becomes an enforced invariant, not a
convention.

1. **Primary guard, in the fork** — `Wallet::update_witnesses`,
   `/Users/gofman/Claude/utexo-rgb-lib/src/wallet/rust_only.rs:729-741`. Do not pass the raw
   blockchain resolver. Wrap it so a witness whose *currently stored* ord is `Tentative` and which
   the indexer reports `Unresolved` is returned as `Resolved(tx, Tentative)` from the stash, or is
   skipped entirely. Rationale: *"the indexer has never seen a tx I deliberately never broadcast"*
   is not evidence of invalidity, and the `Unresolved → Archived` mapping
   (`rgb-consensus/src/validation/validator.rs:113-119`) is only correct for witnesses that were
   once broadcast. Only an explicit entry in `force_witnesses` may archive such a witness. **This is
   the single highest-leverage line in the system** — the one place mercury controls where the plain
   resolver meets the stock.
2. **Secondary guard** — `mercury_rgb::RgbWallet` (`clients/libs/rust-rgb/src/lib.rs`) must never
   expose `update_witnesses`/`upsert_witness`. It does not today; pin that with a **CI grep-deny**
   on `update_witnesses|upsert_witness` across `clients/` and `lib/`. Any future carrier-stash
   resolution must go through the fork's `OffchainResolver` (`utexo-rgb-lib/src/utils.rs:1040-1075`)
   with the full ladder txid list in `offchain_witness_ids`, exactly as
   `validate_consignment_offchain_chain` / `save_new_asset(consignment, txids)` already do.
3. **Health-check rule** — never assert carrier liveness on `get_asset_balance` or `list_unspents`.
   The observed failure leaves both reporting a full, settled, spendable balance while the stock is
   at zero. Every CTES-R invariant test, watchtower check and ops alarm must probe the **stock** —
   e.g. a read-only `color_psbt` dry-run over the tier tip, which fails loudly with
   `InvalidColoringInfo { available (0) }` — and the SDK should surface a DB-vs-stock reconciliation
   check.

### 3.4 Fee arithmetic — MANDATORY, exact, no new constant

**Every coloured tier must use `committed_fee_for_outputs(n_payload + 1, rate)`.** The opret
serializes to exactly 43 bytes = `P2TR_OUT_VBYTES`, so "+1 payload" is exactly right, and it was
measured exact (2.000 sat/vB) at both n=1 and n=3. Leaving the current call sites unchanged
underpays every coloured tier by a constant `43 × rate` sats (86 sat at 2 sat/vB) regardless of `n`,
which breaks the self-funding/standalone-relay property that lets a pre-signed tier confirm without
the P2A. `TIER_VBYTES = 124` and `P2TR_OUT_VBYTES = 43` themselves are correct and need no change.

### 3.5 A new tier builder — MANDATORY; do NOT reuse `create_colored_split_tx`

Its `!is_op_return` vout filter counts the P2A anchor as a payload and trips its own length guard
(observed at n=1 and n=3). The new builder must derive payload vouts by **explicitly excluding both
the opret and the P2A script** (`P2A_SCRIPT_BYTES`), and must **return** them rather than letting
callers infer. `create_colored_backup_tx`'s single-output derivation happens to still return the
right vout for a 1-payload tier, but that is accidental — do not build on it.

### 3.6 What is explicitly NOT a risk — stop spending design effort on it

**No PSBT, TRUC or standardness contingency is needed.** nVersion 3 survives both
`Psbt::from_unsigned_tx` layers and the cross-crate base64 round-trip; nSequence and locktime are
preserved; the coloured `(opret, P2TR, P2A)` v3 shape is accepted by Core 30.2 policy down to the
signature check, identically to v2. Delete this line item from the plan.

---

## 4. Live bugs found in current code

Fix status is independent of whether CTES-R ever ships.

### 4.1 ~~LIVE~~ **CLOSED for coloured carriers** — RGB carriers have no unilateral exit at all

> **RESOLVED 2026-07-31, measured, `SDK_E2E=75` (`clients/tests/rust/src/sdk75_colored_exit.rs`),
> two consecutive green runs.** A coloured laddered carrier now walks `T → X_0 → S_0` on chain and
> the RGB allocation survives. `unilateral_exit` opens for exactly one class — a carrier whose
> ladder is COLOURED — and refuses every other carrier, including one whose `tesr-` row is missing
> or unreadable (`wallet.rs`, `colored_ladder_sids` + the fail-closed refusal at the flat fallback).
> `withdraw` and `refresh` still refuse carriers outright; only the exit opened (§7 stands).
>
> The walk itself is driven by `tesr::exit_pass(&electrum_client, &bundle)`, whose signature is the
> proof of "no SE cooperation": a chain backend and stored material, no `ClientConfig`, no
> coordinator URL, no key.
>
> Proof of survival, three parts, and (c) is the one stash state cannot fake:
> (a) all three tiers MINED (height > 0), each spending its parent's DECLARED `payload_vout`, each
> with one 32-byte opret at vout 0, `F` consumed;
> (b) the §3.3 read-only `color_psbt` stock probe over `(S_0.txid, S_0.payload_vout)` spends the
> full allocation and REFUSES `amount + 1` (`RgbWallet::probe_spendable` /
> `mercuryrustlib::rgb::probe_allocation`, never `color_psbt_and_consume`);
> (c) `validate_consignment_offchain_chain(leaf, offchain_txids = &[])` — the EMPTY set, so every
> witness falls through to the plain indexer — FAILS before the walk and SUCCEEDS after.
>
> **Two gaps the run exposed, both open:**
> 1. After the exit, rgb-lib's UTXO-driven views are **stale, not merely blind**: measured twice,
>    `list_token_allocations` / `get_asset_balance` / `token_carrier_allocations` still report the
>    full allocation at the **SPENT** funding outpoint `F` and nothing at the exit tip. The tip pays
>    a Mercury seed-derived key that is not in the engine's descriptor. `token_carrier_allocations`
>    is the input to the CTES-R decision site in `claim()`, so it would offer a spent outpoint as a
>    colourable carrier. The E7 rule stands and hardens: never read liveness OR location off the
>    balance.
> 2. The exited allocation is therefore not spendable **through the SDK**: the stash binds it and
>    the owner holds the Bitcoin key, but nothing registers the tip outpoint
>    (`register_statechain_utxo`) after an exit, so the engine cannot build an onward RGB spend.
> 3. `auto_exit_due`'s deadline loop still filters out **every** carrier (`wallet.rs`), coloured
>    included — the coloured exit is manual-only, with no automatic near-deadline protection.
>
> Robustness note recorded while running it: rgb-lib's electrum witness resolver derives a mined
> witness's height as `block_headers_subscribe().height − confirmations` and looks up the merkle
> proof at that height ± 2 (`rgb-ops-0.11.1-rc.10/src/indexers/electrum_blocking.rs:168-178`). The
> tip is electrs's and the confirmation count is bitcoind's, so while electrs trails by ≥ 2 blocks a
> well-mined tier is reported "transaction can't be located in the blockchain". Observed on a
> 3-block burst; `sdk75` mines block-by-block waiting for the indexer and retries the proof only on
> that exact string.

**Original finding — confirmed present as described**, `clients/libs/rust-sdk/src/wallet.rs:1039-1050`:
`unilateral_exit` refuses a carrier outright, because the only pre-signed spend of a carrier is
RGB-unaware and broadcasting it burns the asset. This is not a hypothetical CTES-R property — it is
the **current** state of the product: a carrier holder has no exit-in-the-absence-of-the-coordinator
path for the asset, only for the sats they no longer want.

**This needs an owner regardless of the CTES-R decision.** The nearest available mitigation is
`COLORED-FORWARDING.md` §4 stage 5 (colored settle — wire `create_colored_backup_tx(..,
is_withdrawal = true)` as an SDK `settle_carrier`), which is *not* unilateral (it needs the blind
SE) but at least preserves the allocation. **CTES-R is the only design on the table that produces a
genuine unilateral coloured exit.** That is its single largest product justification.

### 4.2 LATENT, arm-on-contact — the global `TOKEN_BLINDING = 777`

`clients/libs/rust-sdk/src/tokens.rs:20`. E2 proves that any two RGB transitions over the **same
parent outpoint** with the same amounts and the same blinding collapse to one OpId and one BundleId,
and that the surviving witness is chosen by an internal-txid lottery.

**Stated plainly: the experiments did not demonstrate a live occurrence in the current split lane**
— a carrier is normally split once and then marked spent, so rivals over one outpoint are not the
normal path today. But the constant's own doc comment ("a fixed value is fine") is now known to be
conditionally false, and any flow that produces two distinct transitions over one carrier outpoint
(a retried or rolled-back split with different parameters, a colored split racing a colored
withdrawal) inherits the lottery. **Recommendation: derive the blinding per-transfer now**, before
CTES-R — `RgbWallet::color` already accepts it per call (`clients/libs/rust-rgb/src/lib.rs:370`), so
the change is SDK-side derivation only, and it retires a whole failure class cheaply.

### 4.3 LATENT — `create_colored_backup_tx`'s vout derivation is correct by accident

`clients/libs/rust/src/rgb.rs:126-131` finds the payload with
`position(|o| !o.script_pubkey.is_op_return())`. It returns the right answer today only because
every current caller has exactly one non-opret output. It silently returns the wrong vout for any
tx with a second non-opret output (a P2A anchor, a change output, a multi-payload split). Harden it
to an explicit, asserted derivation before anyone adds a second output.

### 4.4 NOT LIVE — `update_witnesses` (E7). Do not file as a bug; file as a prerequisite.

Zero call sites in mercurylayer, no internal rgb-lib path reaches it, and `refresh()` /
`list_unspents(online)` on an intact un-broadcast ladder were empirically verified harmless. **But**
`COLORED-FORWARDING.md:516-518` currently blesses it in prose as the thing to test, and the moment
CTES-R exists this public method becomes a one-call, silent, irreversible carrier-destruction
primitive. It must be guarded (§3.3) **before** any coloured-ladder code lands, not after.

---

## 5. Revised cost estimate

**Prior estimate: 11–16 engineer-weeks. Revised: 14–19 engineer-weeks.**

The experiments both *removed* work and *added* work. They removed more certainty-cost than they
added scope, but the net is up, and the entire increase lives in the fork.

| Line item | Weeks | Source |
|---|---|---|
| `payload_vout` migration + adversarial covering tests (own commit) | 1.5–2.0 | E1(a) |
| Per-tier blinding derivation (both sides) + consignment-witness pinning assert + ≥3-rival tests | 1.0–1.5 | E2 |
| **NEW: fork work — resolver guard, `revalidate_offchain_bundles`, `invalid_bundles` backup/restore, CI grep-deny** | **1.5–2.5** | **E7 — not in the prior estimate** |
| New coloured tier builder (excludes opret + P2A, returns vouts) + fee arithmetic | 1.0–1.5 | E1(c)(d) |
| Colouring T/X/S through the tier lifecycle: build, co-sign, verify, census | 3.0–4.0 | prior |
| Coloured unilateral exit / `exit_pass` through the ladder | 1.5–2.0 | prior |
| In-ladder coloured split | 1.5–2.0 | prior |
| Retire terminal-freeze, delete the branch-coin lane, migrate existing carriers | 1.5–2.0 | prior |
| E2E + adversarial suite (rival tiers, DB↔stock reconciliation, exit under CSV) | 1.5–2.0 | E2, E7 |
| **Total** | **14.0–19.5** | |

Removed from the prior estimate (E1): the v3/PSBT/TRUC compatibility contingency (§3.6) and the
"audit every ladder index for silent fund loss" line — the first does not exist, the second has no
silent failure mode.

**Added (E7, the honest delta):** rgb-lib has **no repair API**. `upsert_witness`, `refresh` and
forced `update_witnesses` all fail to restore an invalidated bundle, and the only working recovery
observed was broadcasting the tier — which for `X_m`/`S_k` is CSV-gated and therefore a full
sequential on-chain unwind. CTES-R must ship a fork primitive that re-validates a bundle from the
ladder's own bundled witnesses (a `revalidate_offchain_bundles(txids)` calling
`maybe_update_bundles_as_valid` after upserting the tier ords back to `Tentative`), plus a
backup/restore of the stock's `invalid_bundles` set. **That was not in the 11–16.**

The costs already known and unchanged still stand and should be re-read before committing:
CTES-R **raises** the minimum splittable carrier ~2.6× (≈4,800 sats at 2 sat/vB vs 1,800 today) and
**removes** off-chain colored combine (`COLORED-FORWARDING.md` §7). It trades better
whole-allocation forwarding and a real unilateral exit for worse granularity. Cost it on that basis.

---

## 6. The ordered first three commits

Each is independently landable, independently testable, and lands **before any colouring is wired
into the live ladder**.

### Commit 1 — `payload_vout` migration, no colouring, no behaviour change

Introduce an explicit `payload_vout` on the tier/bundle types and thread it through every chaining
site, keeping the value at `0` everywhere so the on-the-wire behaviour is byte-identical. Move the
`live_csv_by_outpoint` census key (`clients/libs/rust/src/tesr.rs:1706`) to the same accessor.

*Sites:* `lib/src/tesr.rs:203, 401`; `clients/libs/rust/src/tesr.rs:1393, 1403, 1423, 1671, 1677,
1706, 1755, 1773, 1779`.

*Adversarial tests, required in this commit:* for each site, construct a bundle whose `payload_vout`
is deliberately wrong and assert it fails **closed** with a named error — `verify_bundle_ex`
rejecting `vout != 0` / `spk != agg_spk`, `verify_child_bundle`'s `taproot_key_hex` failing on a
non-taproot spk, `tier_out_value` returning `None` → `FeeTooHigh`, and `verify_tier_cosigned`'s
sighash failing on a wrong prevout amount. Plus one census test proving a `live_csv_by_outpoint` key
that disagrees with its tier is detected rather than silently ignored.

**Blocked on:** the concurrent security fix that owns `clients/libs/rust/src/tesr.rs`,
`transfer_receiver.rs` and `ssp.rs`. Land after it, not alongside it.

### Commit 2 — carrier-stash resolver invariant (fork + CI)

Independent of commit 1; may land in parallel or first.

* `utexo-rgb-lib/src/wallet/rust_only.rs:729-741` — wrap the resolver so a stored-`Tentative`
  witness reported `Unresolved` by the indexer is returned as `Resolved(tx, Tentative)` or skipped,
  never `Unresolved`. Only an explicit `force_witnesses` entry may archive it.
* `utexo-rgb-lib` — add `revalidate_offchain_bundles(txids)` and `invalid_bundles` backup/restore.
* mercurylayer — CI grep-deny on `update_witnesses|upsert_witness` across `clients/` and `lib/`.
* Delete the "test it" framing from `COLORED-FORWARDING.md:516-518` and point it here.

*Test:* promote the E7 harness (`…/scratchpad/e7/harness/src/main.rs`) into a fork test. It must go
from "one call → both rungs dead, `succeeded=2`, no error, balance unchanged at 1000" to "one call →
both rungs alive", and the repair primitive must restore a deliberately-invalidated ladder without
broadcasting anything. Add the stock-probe health check (read-only `color_psbt` dry-run) as the
assertion, **not** `get_asset_balance` — E7 proved the balance is blind to this.

### Commit 3 — coloured tier builder + per-tier blinding, unit-level only, not wired into the ladder

* New builder that colours a tier and returns explicit payload vouts, excluding **both** the opret
  and `P2A_SCRIPT_BYTES`. Does not reuse `create_colored_split_tx`.
* Fee: `committed_fee_for_outputs(n_payload + 1, rate)` on every coloured tier.
* Replace `TOKEN_BLINDING` with the per-tier derivation `H(statechain_id ‖ tier_role ‖ tier_index ‖ rung)`,
  with the receiver deriving the identical value from data already in the transfer message.
* Build-time assert: the returned consignment's leaf bundle carries `pub_witness == this tier's own
  txid`; fail loudly on collision.
* Harden `create_colored_backup_tx`'s vout derivation (§4.3) while here.

*Test:* promote the E1 and E2 harnesses. Assert the coloured 1-payload and N-payload tiers hit
**exactly 2.000 sat/vB**, that `validate_offchain` is `valid=true` against the un-broadcast txid,
and — the decisive one — a **≥3-rival** test that deliberately selects a **non-minimum-internal-txid**
rival as the current tier and asserts the consignment embeds *that* one. A 2-rival test is not
acceptable evidence; it passes half the time by luck.

Only after these three does the first commit that colours a live `T` over a real `F` become
reviewable.

### Commit 4 — colour the ladder at establish (LANDED)

`claim()` establishes a COLOURED ladder over an RGB carrier: `mercuryrustlib::tesr::`
`build_colored_ladder` (engine only, synchronous) + `cosign_colored_ladder` (three ordinary
`cosign_tier` round-trips). The split is not cosmetic — the RGB engine's resolver is `!Sync`, so
holding its guard across an `await` makes `claim()` non-`Send` and it cannot run in the background
watcher. Payload lands at vout 1 (read from the builder, never assumed), fee is
`committed_fee_for_outputs(2, rate)` = 334 sat at 2 sat/vB, and `TesrBundle.rgb`
(`#[serde(default)]`) carries the contract id, the amount and the three per-tier consignments.

**The census is unchanged, and this is the trap worth restating.** `flat_backups` STAYS the
deposit-anchored chain length for a coloured coin. `tx1` was co-signed at deposit-init, before the
coin had any RGB on it, and that co-sign is permanent — passing 0 makes `expected = 3` against a
live `num_sigs` of 4 and bricks every coloured coin at claim. Colouring adds zero SE co-signs (one
input, one sighash, one `cosign_tier`), so the equation is `4 = 1 + 3 + 0`, exactly as plain.

**Gated OFF by default (`SdkConfig::colored_ladder`), for one specific reason.** A coloured `T`
spends `F` with NO timelock; the legacy coloured split spends the same `F` as an absolute-locktime
backup maturing ~`initlock` blocks out. A carrier holding both would let its previous owner
broadcast `T` the instant after conveying a split — an immediate, cost-free clawback of sats *and*
asset, against a receiver who cannot race it. Neither the census (the piece is a fresh statechain
node) nor the terminal-parent check (`T` is already co-signed) catches it. So the lanes are made
mutually exclusive per coin, fail-closed, and the flag stays off until §7's coloured forwarding
retires the split lane. This is the same discipline that twice reverted the V2 default over B1.

What is refused on a coloured ladder, all before any co-sign: `renew`, `rollover`,
`presign_receiver_state`, `in_ladder_split`, `cosign_detrigger`
(`tesr::refuse_uncolored_over_colored`); conveyance (`transfer_sender::execute`, no receiver-side
consignment validation yet); and the colored split / combine / batch lanes
(`tokens::refuse_if_colored_ladder`). `unilateral_exit`, `withdraw` and `refresh` still refuse
carriers outright — §4.1 and §7 are unchanged by this commit.

*Test:* `SDK_E2E=74` (`sdk74_colored_ladder`). Note `73` was already taken by
`sdk73_structural_recovery`.

### Commit 5 — colour RENEWAL and TRANSFER: the rival-tier case (LANDED)

The case §2.2 is about. A renewal replaces `X_m` over `T`'s payload output; a transfer co-signs a
fresh `S_k` one δ lower over `X_m`'s. **Rivals over one parent outpoint are the normal case**, so
this is where a coloured ladder is actually hard — and where a shared blinding is fatal rather than
merely untidy.

**The seal derivation is now ONE function, `tesr::colored_tier_seal(sid, role, level, m, csv)`,**
and its rung is `m ‖ csv`. That pair is not a convention: `verify_bundle`'s per-prevout race check
ALREADY requires the live tier over an outpoint to sit at a strictly lower CSV than every superseded
rival over that same outpoint, so two rivals over one parent output can never share a CSV in a bundle
that verifies. The renewal counter is folded in as a second, independent separator. `level` is always
0, because `rollover` refuses a coloured bundle — and `TesrBundle::colored_tier_seals` refuses a
multi-level coloured bundle rather than guessing at per-level counters it cannot reconstruct.

**The receiver derives, never trusts.** Everything the derivation needs — the statechain id, each
tier's role, `bundle.m`, each tier's `csv` — is already in the conveyed `TesrBundle`, and both `m`
and `csv` are cross-checked against transaction CONTENT (`csv` against the tier's nSequence in
`verify_bundle_ex`, the whole ladder against the coin in `verify_bundle_bound`). No blinding is ever
transmitted.

**New API, all two-phase for the `!Sync`-resolver reason:** `build_colored_renewal{,_auto}` +
`cosign_colored_renewal`; `build_colored_receiver_state` + `cosign_colored_receiver_state`;
`transfer_sender::execute_colored` (a coloured ladder conveyed WITHOUT a pre-coloured `S'` is still
refused, and so is a coloured draft handed to a plain ladder). SDK entry points:
`renew_colored_ladder{,_with}`, `transfer_colored_carrier`, and the receive-side
`accept_colored_ladder`.

**Two structural gates were added, not relaxed.** `verify_colored_shape` runs inside
`verify_bundle_ex` — i.e. on the claim path AND the SSP's pre-payment census, colour-blind and with
no RGB engine — and refuses both ways a bundle can lie about colour: claiming colour its tiers do not
carry (opret-free tiers, a consignment list that does not line up with `exit_tiers()`), and carrying
colour it does not claim (opret-bearing tiers conveyed as `rgb: None`, whose asset half would be
validated by nobody). Separately, `consignment_bearing_outpoints` now quarantines a coloured-laddered
coin from plain-BTC selection between the claim and the booking; a conveyed coloured ladder carries
its RGB half in the `tesr-` row, not in a backup-row envelope, so it was invisible to that gate.

**One fork change, and it is load-bearing:** `Wallet::accept_offchain_ladder`
(`utexo-rgb-lib/src/wallet/rust_only.rs`), surfaced as `RgbWallet::accept_ladder`.
`accept_transfer` cannot accept a ladder for two independent reasons — it resolves a SINGLE off-chain
witness, so the leaf's un-broadcast ancestors fall through to the indexer and are archived; and it
reveals a SINGLE seal, whereas a ladder receiver must also be able to open the EXTENSION's payload
output, since that is the outpoint its own next-hop state spends. Without that second seal the coin
books fine and is then exit-only. The hop-2 assertion in `sdk74` is precisely the test of it.

*Test:* `SDK_E2E=74`, extended. It renews until there are **≥3 rivals** over the trigger's payload
output AND the live extension is deliberately **not** the internal-txid minimum — §3.1's rule, and
the loop is what makes it deterministic rather than a coin flip — then asserts the leaf consignment
embeds the live extension and none of the superseded ones. Then the coin moves alice → bob → carol
entirely off-chain (chain height unchanged, every tier still absent from the backend), with the
census re-verified at the far end by the receiver's own `verify_bundle_bound`.

**Still refused, and still true:** `unilateral_exit`, `withdraw` and `refresh` refuse carriers
outright (§4.1 and §7 are unchanged); coloured `rollover`, `in_ladder_split` and `cosign_detrigger`
refuse; the legacy colored split/combine lanes remain mutually exclusive with a coloured ladder; and
`SdkConfig::colored_ladder` still defaults OFF for the Commit-4 reason.

---

## 7. What CTES-R removes, and what it does not

**It removes, completely:**

* the **un-laddered branch-coin lane** — the "branch coin + absolute-locktime backup" shape is
  deleted, not deprecated;
* the **absolute deadline on carriers** — because a coloured carrier is laddered like every other
  coin, and TES-R tiers carry only *relative* locks that do not tick until the parent confirms;
* the **monitor-or-delegate burden** on the receiver — an idle laddered coin never ages, so there is
  nothing to watch and nothing to delegate;
* the **unbound terminal parent IDs** — terminal parents are no longer a separately conveyed list
  that is not cryptographically bound to the branch inputs; the ladder's own co-signed tier chain
  is the binding;
* **terminal freeze itself** — an RGB carrier becomes ladderable, which is what makes the first
  genuine **unilateral coloured exit** possible (today `unilateral_exit` refuses carriers outright,
  §4.1).

**It does NOT remove the ~6.9-day root epoch.** Every off-chain coloured subtree still hangs off one
confirmed on-chain root, and the original depositor still holds a backup maturing at
`h_deposit + lockheight_init` — 1,000 blocks ≈ 6.9 days on the deployed profile
(`server/Settings.toml:2-3`). That is a property of **on-chain state**: an unspent root plus a
maturing absolute-locktime transaction held by the original depositor. Only an on-chain transaction
can change an on-chain fact. The single escape is an **on-chain coloured re-anchor**, which **does
not exist today and is not scheduled** — `refresh` refuses carriers
(`clients/libs/rust-sdk/src/refresh.rs:150-157`) and `refresh_rgb_anchor_self_transfer`
(`clients/libs/rust/src/rgb.rs:494-660`) has zero callers and *consumes* a rung rather than
restoring one. CTES-R does not change this, no design in `COLORED-FORWARDING.md` changes this, and
shipping CTES-R must not be described as if it did.

The honest ceiling remains: **unlimited off-chain hops within a ~7-day epoch, then one on-chain
transaction per carrier tree per epoch.** Amortised across a wide tree the per-user-visible-transfer
on-chain cost goes to zero, which is the strongest true claim available. The colored re-anchor
(`COLORED-FORWARDING.md` §6.8) is the largest remaining gap in RGB support and it still has no
owner. It should get one, independently of this gate.
