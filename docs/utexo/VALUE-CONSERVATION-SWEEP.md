# Value-conservation sweep of the TES-R verifiers

**Trigger:** commit `4e165e6` ("Bind a split child's tier chain to value conservation — a live skim
was possible") and its follow-up `a063e3f`. That defect was proven by running it. This document is
the sweep for the REST of its class, over `clients/libs/rust/src/tesr.rs`,
`clients/libs/rust/src/transfer_receiver.rs`, `clients/libs/rust-sdk/src/{ssp,transfer}.rs` and
`lib/src/{tesr.rs,transfer/receiver.rs}`.

**Method, stated plainly:** source-level verification, read-only. **No test was run and no test
result is claimed anywhere in this document.** Every line reference below was opened and read; the
attacks are entailed by the code as written, not demonstrated by execution. That is a weaker
standard than `4e165e6` met, and the difference matters when you decide what to land.

---

## 1. The class

> **A signature over a tier binds that tier's INPUT amount and nothing else.**
> `verify_tier_cosigned` (tesr.rs:6176) constructs its prevout as
> `TxOut { value: prevout_value, script_pubkey: agg_spk }` and checks one Schnorr signature; the SE
> co-signs blind (`cosign_tier_request`, lib/src/tesr.rs:503-528, takes the prevout amount as a
> CALLER parameter and never deserialises outputs). So "this tier is genuinely co-signed" never
> implies "this tier pays the right key", "this tier forwards the right amount", or "this tier is
> even broadcastable". A tier legitimately has more than one output (`payload…, P2A`, plus an opret
> when coloured), so an extra output is not suspicious on its face; and declared-vs-signed agreement
> (`out_value == the parsed output`) makes a number honest, not correct. **Any quantity a verifier,
> a wallet booking, or a payment gate reads out of a tier chain is attacker-chosen unless some check
> walks it back, hop by hop, to an ON-CHAIN output — and the chain of hops is only as strong as its
> weakest hop, including the hops in the parent and ancestor segments.**

**Checklist for anyone adding or reviewing a tier check.** For every tier on every acceptance path,
four independent properties must be pinned, and pinning three of them is worth nothing:

1. **Where it pays** — `payload_out(tx).script_pubkey` == the aggregate (or the owner/payee) it is
   supposed to pay. A co-sign proves what it SPENDS, never what it PAYS.
2. **How much it forwards** — `Σ payload outputs == tier_out_total(prev, n_payload, rate)`
   (`colored_tier_out_total` when coloured), where `prev` is the PARSED value of the output this
   tier spends.
3. **That nothing else leaves** — the residual: exactly one P2A anchor of `P2A_VALUE`, at most one
   zero-value opret, and no other output. Pinning only `out[payload_vout]` leaves a window exactly
   `committed_fee(rate)` wide (and `rate` is itself sender-declared — see V5).
4. **That the yardstick is ours** — `prev`, `fee_rate` and `params` must be receiver-derived or
   chain-derived. A law measured against a sender-declared constant proves only that the sender was
   self-consistent.

**Direction matters, and it is opposite on the two lanes.** On the whole-coin (flat) lane the
booked/gated amount comes from the CHAIN (`get_amount_from_tx0`, transfer_receiver.rs:670), so
inflation is harmless and a **downward skim** is the attack. On the child lane the amount comes from
the BUNDLE (`SP.out[j].value`, un-broadcast), so a downward skim lowers the reported number too and
**inflation — or anything that silently makes the chain unconfirmable — is the attack**. A fix that
only bounds one direction closes half the class.

---

## 2. Surviving findings, ranked

Ranking is by: does the victim lose money, do they have a fallback, how many lanes reach it, and can
it be planted without a counterparty's cooperation.

---

### V1 — `verify_bundle_ex` has NO value law at all. **Theft. Every lane.**
`clients/libs/rust/src/tesr.rs:5212-5415`

`verify_bundle_ex` checks: ladder shape, colour shape, trigger spends `F`, trigger's payload pays
`A` (`:5253` — script only), each tier spends its parent's payload outpoint, CSV bounds,
declared-vs-signed CSV, payee scriptPubKey (`:5322-5325`), one co-sign per tier (`:5359-5367`), the
superseded battery, and the exact-equality census (`:5405-5411`). **It never compares any tier's
output value to anything.** `TesrTier::out_value` is not read in the function at all; values are
read from the parsed transactions only to seed `prevout_value_of` for the NEXT tier's signature
check (`:5348-5358`), which makes a skimming or minting ladder internally self-consistent by
construction. `verify_colored_shape` (`:5437`) counts OP_RETURNs and places no bound on output
count, and an honest plain tier is already `[payload, P2A]` (lib/src/tesr.rs:212-215).

This one function is the parent of most of the rest of this list: it runs on the claim path
(transfer_receiver.rs:1168 via `verify_bundle_bound`), on the SSP pre-pay path
(transfer_receiver.rs:466), and on the **child** path over the parent segment
(tesr.rs:5609) — where its `final_is_split` arm explicitly skips even the payee check on `SP`
(`:5318-5320`) and no Σ-check replaces it.

**Construction (root lane, planted at `establish` by the coin's first laddering owner):** `T` spends
`F` = 198 530 and pays `A` 1 500 at `out[0]`, with `out[1]` = 197 030 to a key the attacker keeps;
`X` forwards `tier_out_value(1500)`; `S'` pays `owner_spk` honestly. The census is untouched — the
skim costs no extra co-sign — so `se_num_sigs == flat_backups + tiers + superseded` balances
exactly. `verify_bundle_bound` binds `f_txid/f_vout/f_value/agg_address/sid`, i.e. the trigger's
INPUT, and nothing downstream. Model A (transfer_receiver.rs:1179) checks the exit ADDRESS, never
the value. The receiver books the full funding value (`coin.amount = new_key_info.amount` =
`tx0_output.value`, lib/src/transfer/receiver.rs:1004 → transfer_receiver.rs:1486).

**ADJUDICATION — the justification comment at tesr.rs:5851-5861 is wrong, and it must be corrected
in the same commit that fixes this.** It argues a skimming root ladder is not theft because "a whole
coin always retains its flat backup chain … INV-5 makes the current owner's the first to mature".
Every cited fact is real but they govern the flat chain only, and the trigger destroys the flat
chain at the attacker's choosing:

* `TRIGGER_SEQUENCE = 0xFFFF_FFFD` (lib/src/tesr.rs:191) disables the relative lock and
  `build_tier_tx` sets `lock_time: 0` (lib/src/tesr.rs:205) — `T` is broadcastable the instant `F`
  confirms. Every flat backup is forced STRICTLY above the tip
  (`verify_if_locktime_is_reasonable_tx_version_and_output_size`, lib/src/transfer/receiver.rs:537).
* `T` spends the very outpoint every flat backup spends. The repo states the consequence itself:
  **"once `T` confirms, `F` is spent and every flat backup is dead forever"**
  (lib/src/transfer/receiver.rs:572-573).
* Every prior owner retains a broadcastable `T` — the repo states this too, at
  clients/libs/rust-sdk/src/transfer.rs:879-895 ([B1]), including "the spend-budget does NOT protect
  here — it bounds FUTURE co-signs, and `T` was co-signed long before `set_spend_budget`". The
  aggregate `A` is invariant across handover (`get_new_key_info`, lib/src/transfer/receiver.rs:990-992),
  so the retained `T` stays valid forever.
* INV-5 orders the flat backups **only among themselves**. The trigger is not in that chain.

So: a skim placed **in the trigger** is theft with no recovery — the theft and the destruction of
the "slow path" are the same transaction, and the cooperative de-trigger only reaches
`T.out[0]` = 1 500 (`build_detrigger`, lib/src/tesr.rs:479; `cosign_detrigger`, tesr.rs:4329). A skim
placed at `X` or `S'` is recoverable only by a de-trigger, which needs SE liveness, an online owner,
and a detection signal that **no code produces** — nothing anywhere computes a ladder's reachable
exit value and compares it to the booked amount. Neither is "degradation".

**Also live on the SSP pre-pay lane.** `prepay_flat_census` (transfer_receiver.rs:312-472) ends in
the same value-blind `verify_bundle_bound` and reports `ladder_census_ok = true`;
`PendingTransferInfo::amount` for a flat coin is the on-chain funding value
(transfer_receiver.rs:670, `child_amount` is `None` here so `:766` does not override it); the SSP's
gate (ssp.rs:458, 519) is satisfied and `send_payment` fires at ssp.rs:524. The attacker collects an
irreversible Lightning payment sized to `F`, then broadcasts the skimming trigger.

**FIX.** In `verify_bundle_ex`, after `prevout_value_of` is populated (`:5348-5358`) and before the
co-sign loop, add a per-tier law. `prev` for tier 0 is `bundle.f_value` (already bound to the chain
by `verify_bundle_bound:5002`); for tier `i>0` it is the PARSED
`tiers[i-1].payload_out(&txs[i-1], …)?.value`.

```rust
let colored = bundle.is_colored();
let rate = bundle.fee_rate;                       // and bind it — see V5
for i in 0..txs.len() {
    let tx   = &txs[i];
    let prev = if i == 0 { bundle.f_value }
               else { tiers[i-1].payload_out(&txs[i-1], &format!("tier {}", i-1))?.value };
    let is_final = i == txs.len() - 1;
    // SHAPE: [opret?] payload… P2A, and NOTHING else. (verify_colored_shape already pins
    // oprets to 0 or 1; this pins everything else.)
    let oprets   = tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
    let n_payload = tx.output.len().checked_sub(1 + oprets)
        .filter(|n| *n >= 1)
        .ok_or_else(|| anyhow::anyhow!("tier {i} has no payload output"))?;
    if !(is_final && final_is_split) && n_payload != 1 {
        return Err(anyhow::anyhow!("tier {i} carries {n_payload} payload outputs, expected 1"));
    }
    let anchor = tx.output.last().unwrap();
    if anchor.value != mercurylib::tesr::P2A_VALUE
        || anchor.script_pubkey != mercurylib::tesr::p2a_script() {
        return Err(anyhow::anyhow!("tier {i}'s last output is not the P2A anchor"));
    }
    // VALUE: the payload outputs carry exactly the funding minus this tier's committed fee.
    let payload_sum: u64 = tx.output[..tx.output.len()-1].iter()
        .filter(|o| !o.script_pubkey.is_op_return()).map(|o| o.value).sum();
    let expect = if colored {
        crate::rgb::colored_tier_out_total(prev, n_payload, rate)
    } else {
        mercurylib::tesr::tier_out_total(prev, n_payload, rate)
    }.ok_or_else(|| anyhow::anyhow!("tier {i}: {prev} sat cannot carry a tier at {rate} sat/vB"))?;
    if payload_sum != expect {
        return Err(anyhow::anyhow!(
            "tier {i} is funded with {prev} sat but its payload outputs carry {payload_sum} \
             (expected exactly {expect}) — the remainder leaves the exit chain"));
    }
    // DECLARED (see V7):
    if tiers[i].out_value != tiers[i].payload_out(tx, &format!("tier {i}"))?.value { … }
}
```

Honest bundles satisfy this with equality: `build_trigger`/`build_extension`/`build_state` are
`tier_out_value` (lib/src/tesr.rs:89) over `build_tier_tx`'s `[payload, P2A]`
(lib/src/tesr.rs:196-217), `build_split_state` already enforces
`Σ children == tier_out_total(x_out_value, n, rate)` at lib/src/tesr.rs:449-454, and the coloured
builders use `colored_tier_out_value`/`colored_tier_out_total` (rgb.rs:951-962). The law is not a
new invariant — it is the builders', never mirrored on the receive side.

---

### V2 — the ancestor loop pins neither value nor payee. **Theft. Child lane, depth ≥ 2.**
`clients/libs/rust/src/tesr.rs:5632-5762`

Two omissions in one loop, both of which the root lane already covers:

* **(a) No value law.** Over 130 lines the only reads of `.value` are prevout amounts feeding
  `verify_tier_cosigned(&ext_tx, fund_out.value, &seg_spk)` (`:5668`) and
  `verify_tier_cosigned(&st_tx, ext0.value, &seg_spk)` (`:5684`), plus two `prevouts.insert` seeds.
  There is no `Σ outputs == tier_out_total(…)` and not even the weaker `Σ outputs <= input`.
  `seg.state.out_value` is never read at all.
* **(b) No payee binding.** `ext0 = seg.extension.payload_out(&ext_tx, …)` (`:5670`) is a bare index
  (`payload_out`, tesr.rs:53-65); `ext0.script_pubkey` is compared to nothing. The root lane has
  exactly this check (`:5322-5325`, "tier {i} pays the wrong output"). The next line uses `ext0.value`
  as the prevout amount for the state's co-sign, i.e. it ASSUMES the spent output pays `A_seg`.

The loop then does `cur_tx = st_tx` (`:5761`) and treats that unvalidated transaction as the
authoritative funding tx: `sp_out = cur_tx.output[cb.sp_vout]` (`:5778`), `a_child` derived from it
(`:5787`), and **the whole newly-landed conservation chain is computed relative to `sp_out.value`**
(`:5898`, `:5973`) — so `4e165e6` certifies internal consistency with a number the attacker chose.

**Who loses.** The receiver books that number (`coin.amount = Some(sp_out.value as u32)`,
transfer_receiver.rs:1321 on v3; `= new_key_info.amount`, `:1389` on v4, which is
`tx0_output.value` of the same `SP` — lib/src/transfer/receiver.rs:1004). The child has **no flat
backup** (`CHILD_V2_BASELINE = 0`, tesr.rs:2952), the ancestor is required to be terminal
(`:5651-5655`) so the SE will never co-sign a repair, `child_exit_chain` only rebroadcasts the
conveyed tiers (`:3117-3131`), and no function anywhere builds a rival tier over `SP.out[j]` for an
already-owned child. Variant (a) leaves a consensus-invalid chain; variant (b) leaves the attacker
able to sweep the ancestor extension's payload output with her own key the moment it confirms, and
the victim's state committing to a prevout that does not exist. Both are total loss.

**Reachable without a counterparty.** `child_in_ladder_split` (tesr.rs:3408, driven from
rust-sdk/src/transfer.rs:1068 and :1201) pushes `{extension: cb.child_extension, state: CSP}` into
`ancestors` — the exact tiers `4e165e6` bound as a leaf lose that binding the moment they become an
ancestor. A splitter's own change child is persisted by `persist_child` with no verification, so the
attacker builds it herself.

**FIX.** Inside the loop, after `:5668` and after `:5670`:

```rust
if ext0.script_pubkey != seg_spk {
    return Err(anyhow::anyhow!("ancestor {i}: extension does not pay its own aggregate"));
}
```

and apply the V1 per-tier law twice per segment — `ext` against `fund_out.value` with
`n_payload == 1`, and `st` (the level's SPINE split state) against `ext0.value` with `n_payload`
derived from its output vector. Factor the V1 block into one `fn bind_tier_value(tx, prev,
n_payload_expected, colored, rate)` and call it from `verify_bundle_ex`, from the ancestor loop, and
from the leaf (replacing `rung_forward`), so the three can never drift again.

---

### V3 — on the child lane, `cb.parent.f_value` is never bound to the chain. **Theft. SSP + claim.**
`clients/libs/rust/src/tesr.rs:5609` and `:3213-3217`

`verify_child_bundle` calls `verify_bundle_ex(&cb.parent, …, true)` **directly** — not
`verify_bundle_bound`. The only place `f_value` is compared to the on-chain funding output is
`verify_bundle_bound:5002`, which never runs on this lane. `verify_conveyed_child` fetches the real
`f_tx`, takes `f_out.script_pubkey` (`:3217`) and **discards `f_out.value`**.

`verify_bundle_ex` seeds `prevout_value_of` with the declared `bundle.f_value` (`:5349-5352`) and
verifies the trigger's co-sign against it. So a sender declares `f_value = 10 BTC` over a
200 000-sat `F`, builds the whole parent ladder conserving perfectly from the lie (the blind SE
signs the sighash it is handed), and `SP.out[j]` is 10 BTC of fiction. **This variant survives a
naive per-tier Σ patch** — the ladder is internally conservative; only the root is false. The
attacker's own coin is untouched and their flat backups still redeem it in full.

The child's two hops then satisfy the `4e165e6` equality law exactly, `A_child` is a genuine server
registration, both censuses balance, Model A holds, `check_exit_headroom` passes.
`verify_conveyed_child` returns ≈ 10 BTC (`:3383`), `peek_pending_transfers` lets that OVERRIDE the
branch-derived amount (transfer_receiver.rs:766), and `check_latched_coins` (ssp.rs:292, called at
:519) pays any invoice it covers — `send_payment` at ssp.rs:524. `SP` is un-broadcastable and the
child has no backup.

**FIX.** One line, the child-lane analogue of `:5002`. Pass the value in beside the spk:

```rust
// verify_conveyed_child, at :3217
let f_spk_hex = hex::encode(f_out.script_pubkey.as_bytes());
if cb.parent.f_value != f_out.value {
    return Err(anyhow::anyhow!(
        "conveyed child's parent ladder declares F value {} but the funding output carries {}",
        cb.parent.f_value, f_out.value));
}
```

Do it in `verify_conveyed_child` (which holds `f_out`) rather than passing another scalar into
`verify_child_bundle`, and add the same guard to `prepay_child_census`'s path so the pre-pay lane
inherits it.

---

### V4 — the landed law pins `out[payload_vout]` only; the rest of the output vector is free. **Theft (strand). Child lane.**
`clients/libs/rust/src/tesr.rs:5896-5909` and `:5973-5985`

`ext_out0`/`st_out0` come from `payload_out`, a bare `tx.output.get(payload_vout)` with no
constraint on `tx.output.len()`; `verify_colored_child_shape` (`:6053`) counts OP_RETURNs only. So a
sender satisfies both new equalities **exactly** and appends `output[2] = X`: for
`X > committed_fee(rate)` the tier is consensus-invalid forever (outputs exceed input), while every
number the receiver books and the SSP reads stays honest-looking. Under-filling instead (eat the
committed fee, drop the anchor to zero) leaves a 0-fee tier whose only child sits behind a CSV and
therefore cannot package-CPFP it.

The extension hop is the unrecoverable one: `child_retransfer` (`:3610`) and `child_in_ladder_split`
(`:3408`) both spend `cb.child_extension`'s payload output and inherit its death, and the code
states the gap itself at `:3432` ("A received child cannot renew … there is no `ChildTesrBundle`
analogue"). The sender can also terminalize the child before conveying (`verify_child_bundle`
deliberately TOLERATES a terminal child — `[F2]` at `:5560-5566`), which removes even the
in-principle re-cosign.

The single Σ/fee identity that exists in the tree — "coloured tier {txid} spends more than its
parent holds", rgb.rs:1333 — is in the BUILDER `build_colored_tier` and is unreachable from any
verifier.

**FIX.** Subsumed by V1's shape+Σ block: pin the anchor, forbid extra outputs, and bind the payload
SUM rather than `out[payload_vout]`. Replace the two `rung_forward` comparisons with the shared
`bind_tier_value` helper.

---

### V5 — the yardstick itself is sender-declared: `fee_rate` (and `params`). **Theft. Child lane; mis-booking elsewhere.**
`clients/libs/rust/src/tesr.rs:5869-5881`, field at `:84`

`rung_forward` measures against `cb.parent.fee_rate`, a plain serde `f64`. No acceptance path
compares it to anything: `verify_bundle_ex`, `verify_bundle_bound` and `verify_child_bundle` never
mention `fee_rate`; `verify_conveyed_child` DOES fetch a live rate (`:3284-3287`) but spends it only
on the parent's flat backups (`:3294-3302`). So the law proves "the tiers forward exactly what the
SENDER'S OWN declared rate says", not "the tiers forward what the coin is worth".

Declare `fee_rate: 700.0` on a 198 530-sat child: each rung consumes `committed_fee(700) + 240`
= 87 740, both equalities hold with exact equality, the receiver books 198 530 (claim path) and the
chain delivers 23 050. Combined with V4 the freed room is not burned but POCKETED — the window is
identically `committed_fee(rate)` = `ceil(125·rate)` per tier, sized by the attacker.

`fee_rate` also drives `tier_out_total` in `in_ladder_split` (`:2652`) and `child_in_ladder_split`
(`:3443`) and the SDK's admission floors (`ParentShape { fee_rate: cb.parent.fee_rate }`,
rust-sdk/src/transfer.rs:1037-1040) — every floor scales with the lie, so nothing downstream catches
it.

**The binding is legitimate and free:** both establish paths use the per-network constant —
`establish_auto` passes `p.committed_fee_rate` (tesr.rs:1402-1403) and `build_colored_ladder_auto`
does the same (tesr.rs:812-820) — and every other site propagates `bundle.fee_rate` unchanged. So
honest bundles pass with equality.

**FIX.** In `verify_bundle_ex` (root lane) and `verify_child_bundle` (child lane), before any value
law:

```rust
let want = mercurylib::tesr::TesrParams::for_network(&bundle.network);
if bundle.fee_rate != want.committed_fee_rate {
    return Err(anyhow::anyhow!(
        "conveyed ladder declares fee_rate {} but this network's committed rate is {}",
        bundle.fee_rate, want.committed_fee_rate));
}
if bundle.params != want {                      // TesrParams derives PartialEq (lib/src/tesr.rs:121)
    return Err(anyhow::anyhow!("conveyed ladder declares a non-standard schedule"));
}
```

**Secondary, same site:** `committed_fee(r) + P2A_VALUE` (lib/src/tesr.rs:90) and
`committed_fee_for_outputs(..) + P2A_VALUE` (`:413`) are UNCHECKED adds over a saturating
`f64 as u64` cast. The workspace sets no `[profile]` overrides, so debug/test builds have
overflow-checks on and a large declared rate is a remote panic on the claim path, reachable straight
from `rung_forward`. Make both adds `checked_add` and reject non-finite / `<= 0` rates. (Binding the
rate as above also closes this, but do both — the arithmetic is public API.)

---

### V6 — the claim path books the FUNDING value, and it already had the reachable one in hand.
`clients/libs/rust/src/transfer_receiver.rs:1321` (v3) and `:1389` (v4)

`verify_conveyed_child` computes and RETURNS the reachable exit value (`cb.child_state.out_value`,
tesr.rs:3383) and the claim site **discards it** (transfer_receiver.rs:979), then books
`sp_out.value` — the top of the same un-broadcast chain. On v4 the same number arrives via
`get_new_key_info(&sp_hex, sp_outpoint, …)` → `amount: tx0_output.value as u32`
(lib/src/transfer/receiver.rs:1004).

This is the amplifier that turns V1-V5 into credited money, and it is also wrong on HONEST bundles:
the SSP pre-pay lane reports `child_state.out_value` (transfer_receiver.rs:766) while the claim path
books `sp_out.value`, so the two disagree by exactly two rungs on every child, forever, with no
reconciliation pass.

The branch lane already learned this lesson — `validate_branch` rejects any branch tx where
`out_value > in_value`: "creates value … not broadcastable, so the branch is unexitable"
(transfer_receiver.rs:1754-1756). The TES-R lanes have no equivalent.

**FIX.** Book the reachable value on both child arms:

```rust
// :1321 (v3) and :1389 (v4) — the value verify_conveyed_child already returned.
coin.amount = Some(cb.child_state.out_value as u32);   // and the matching Activity rows :1325 / :1403
```

Keep `get_new_key_info`'s call unchanged (its job on that lane is proving `A_child` is invariant);
override only the amount it reports. Consider the same treatment for the flat lane
(`:1486`) once V1 lands, so a wallet can never display more than its ladder can deliver.

---

### V7 — declared `TesrTier::out_value` is unbound on the plain ROOT lane. **Stranding, after terminalization.**
`clients/libs/rust/src/tesr.rs:4304`, `:2763`, `:4202`, `:4258`, `:4330`

`a063e3f` bound the declared field on the CHILD's extension precisely because the receiver's next
split signs against it. The root lane has the identical shape and no such check: `verify_bundle_ex`
never reads `TesrTier::out_value`. Only the COLOURED builders route through `tier_payload_prevout`
(tesr.rs:975-996), which does bind declared-vs-signed; every PLAIN path reads the field raw:

| site | what it signs against |
|---|---|
| `presign_receiver_state` tesr.rs:4301, 4304 | `ext.out_value` — the LIVE whole-coin transfer path (`execute_ex`) |
| `in_ladder_split` tesr.rs:2652, 2671-2673, 2763 | `x_m.out_value`, two lines AFTER `set_spend_budget(…, 1)` at `:2761` |
| `child_in_ladder_split` tesr.rs:3443, 3464, 3525 | `cb.child_extension.out_value`, after `set_spend_budget` at `:3519` |
| `renew` tesr.rs:4201-4202 / `rollover` tesr.rs:4257-4258 | `current_parent().out_value` / `cur_ext.out_value` |
| `cosign_detrigger` tesr.rs:4329-4330 | `bundle.trigger.out_value` — the emergency path |

A conveyed bundle whose extension signs 500 forward but declares `out_value: 198_000` is accepted;
the receiver's own next operation then co-signs against a sighash committing to an amount the
transaction does not carry. `create_signature` verifies the aggregate signature against that same
wrong message and SUCCEEDS, so the failure is silent — a dead transaction, discovered after the coin
has been terminalized.

**FIX.** Add the declared/signed binding for every tier in `verify_bundle_ex` (the last line of the
V1 block above) and for both tiers of every ancestor segment. Separately, `cosign_detrigger` returns
a hex string and records nothing, so its co-sign appears in neither `exit_tiers()` nor
`superseded_*` while the SE's `sig_count` rises by one — that is a census drift bug independent of
any lie, and it should either persist the de-trigger into the bundle or be counted.

---

### V8 — declared `payload_vout` vs builders that hardcode vout 0. **Stranding, after terminalization.**
`lib/src/tesr.rs:466` (`build_split_state`), `:338`/`:358` (`build_extension`/`build_state`), `:491` (`build_detrigger`)

Those builders hardcode `previous_output.vout = UNCOLORED_PAYLOAD_VOUT` (0) and never take a
`prev_vout`. The verifiers, by contrast, honour the DECLARED `payload_vout` everywhere: the child
state links to `(ext_tx.txid, cb.child_extension.payload_vout)` (`:5917`), the new conservation law
measures at that index (`:5829`, `:5898`), and `verify_colored_child_shape` constrains a PLAIN
child's index not at all (`(false, 0) => {}`, `:6100`).

So a sender conveys an otherwise honest child whose extension has its two outputs SWAPPED —
`out[0]` = the 240-sat P2A anchor, `out[1]` = `P2TR(A_child)` — and declares `payload_vout: 1`.
Everything passes. When the receiver later pays out of that child, `child_in_ladder_split`
terminalizes it (`:3519`) and then co-signs a CSP whose input is `ext:0` (240 sat) while the sighash
commits to `out[1].value`. Same lever on the root lane via `in_ladder_split`/`presign_receiver_state`.

**FIX (cheap, verifier side).** For a PLAIN bundle require `payload_vout == UNCOLORED_PAYLOAD_VOUT`
on every tier; for a coloured one keep the existing opret-index refusal and require the builder's
derived index. **FIX (durable, builder side).** Give `build_split_state`, `build_state`,
`build_extension` and `build_detrigger` an explicit `prev_vout` parameter, as
`build_extension_from`/`build_state_from` already have.

---

### V9 — `prepay_flat_census` is weaker than the claim path it stands in for. **SSP pays, then cannot claim.**
`clients/libs/rust/src/transfer_receiver.rs:312-472`

The pre-pay census runs the version floor, the single-funding-group rule, non-empty backups, the
on-chain-tx0 requirement, Model-A owner-exit, `F` unspent+confirmed, `validate_backup_chain_v2` and
`verify_bundle_bound`. It does NOT run `verify_latest_backup_tx_pays_to_user_pubkey`
(claim path: `:1122`) or `verify_transfer_signature` (claim path: `:1101`), and
`validate_backup_chain_v2` (lib/src/transfer/receiver.rs:303-340) does not check the payee either.
So the flat backup chain that the `4e165e6` rationale treats as the fallback is never checked to pay
the SSP at all before the Lightning leg fires. A pre-payment predicate weaker than the claim
predicate is a pay-out hole — the file says so itself about the RGB gate (ssp.rs, F1 note).

**FIX.** Call both checks from `prepay_flat_census` with the SSP as the prospective owner, exactly
as `[D2]` already does for `owner_exit_address`.

---

### V10 — `Coin::amount` is `u32`; every receive-path booking truncates above ≈ 42.95 BTC.
`lib/src/wallet/mod.rs:96` (field), `lib/src/transfer/receiver.rs:1004`, `clients/libs/rust/src/transfer_receiver.rs:1321, 1325, 1389, 1486, 155, 207`

`amount: Option<u32>` and `Activity::amount: u32`. Every site casts `as u32` with no check. A coin
funded with more than 4 294 967 295 sat wraps: a 43 BTC deposit books as ≈ 0.05 BTC. Same class —
a booked number that does not match the reachable one — reached by arithmetic instead of by an
attacker.

**FIX.** Widen the field to `u64` (it is `u64` everywhere else in this tree —
`PendingTransferInfo::amount`, `TesrTier::out_value`, `TesrBundle::f_value`), or at minimum reject
`value > u32::MAX` at deposit and at every `as u32` site.

---

### V11 — unchecked `u64` subtraction on an attacker-supplied backup tx.
`lib/src/transfer/receiver.rs:468`

`let fee = tx0_output.value - total_output_value;` runs BEFORE anything about the transaction is
verified, over the summed outputs of a SENDER-SUPPLIED backup tx. Reached from the claim path
(`validate_backup_chain_v2:319`), from `prepay_flat_census` (transfer_receiver.rs:442) and from
`verify_conveyed_child`'s parent-backup validation (tesr.rs:3293). A backup whose outputs exceed the
funding output underflows: wrapping in release, panic in debug/test (no `[profile]` overrides in the
workspace). Same attacker-controlled-input hazard the file already fixed once as [R3].

**FIX.** `let fee = tx0_output.value.checked_sub(total_output_value).ok_or(MercuryError::FeeTooLow)?;`

---

## 3. Refuted — the map of what is already load-bearing

Do not re-raise these. Each names the check that stops it.

| Claim | Stopped by |
|---|---|
| Sender conveys a ladder over a decoy funding UTXO / decoy sid | `verify_bundle_bound` tesr.rs:4986-5039 — sid, `(f_txid,f_vout)`, `f_value`, `agg_address`→on-chain key, coordinator aggregate. On the CHILD lane the key half is `[1]`/`[2]` tesr.rs:5686-5707 (`A_parent` derived from the fetched `F.spk`, `UNIQUE(aggregate_xonly)`); the VALUE half is missing — that is V3, and it is the only hole here. |
| Trigger *inflation* (declare `f_value` high) on the whole-coin lane | `verify_bundle_bound` tesr.rs:5002 — `bundle.f_value != coin.f_value`. Real, and it is exactly why V3 (the child lane, which skips this function) is severe. |
| The SSP can be paid for an *inflated* flat coin | Flat-lane `amount` is chain-rooted: `get_amount_from_tx0` transfer_receiver.rs:670, and a branch-funded coin must first clear `validate_branch`'s `out_value > in_value` refusal at `:1754-1756`. Only the DOWNWARD direction survives (V1). |
| `child_state.out_value` spoofed to inflate the SSP's value gate | `[value-gate spoof]` tesr.rs:5962 — `st_out0.value != cb.child_state.out_value`, plus `4e165e6`'s second hop at `:5973`. The declared leaf value is honest; the fiction has to be planted higher up (V2/V3). |
| Pad `superseded_*` with junk to buy census slots and hide a co-signed rival state | `verify_superseded_segment` tesr.rs:5058-5210 — every entry parsed, ladder-linked, signature-verified under `A`, raced per-OUTPOINT against the live tier, and proven transitively dead; then exact-equality census at `:5405-5411`, `:5754`, `:6033`. `live_txids` (`:5389`) stops a live tier being re-declared. |
| Pad the flat-backup count on the SSP pre-pay lane | `prepay_flat_census` runs `validate_backup_chain_v2` and counts the VALIDATED length (transfer_receiver.rs:437-465); INV-5 `ladder_decrements_by_interval` (lib/src/transfer/receiver.rs:332) makes duplicate padding structurally impossible. |
| Declare a soft CSV schedule so the exit looks cheap | `bind_declared_csv` at tesr.rs:5260, 5312, 5712, 5819, 5934, and `child_exit_chain_bound` (`:3077`) reads every timelock from the SIGNED `nSequence`. (The `params` the bounds are checked AGAINST are still sender-declared — folded into V5.) |
| Elect the loose SPINE `[0,0]` CSV window for an ordinary state tier | `final_is_split` is a code-path constant chosen by the RECEIVER, never a bundle field — tesr.rs:5292-5300; the ancestor loop derives `kind` from a literal array (`:5688-5705`). |
| Convey a coloured ladder as plain (or a plain child over a coloured `SP` output) | `verify_colored_shape` tesr.rs:5437-5510 and `verify_colored_child_shape` tesr.rs:6056-6106 — the biconditional, one opret per coloured tier, no opret on a plain one, payload index never the opret, consignment count == tier count. |
| Point `sp_vout` at `SP`'s opret or before its payload | tesr.rs:5772-5777 (`sp_vout >= funding_payload_vout`), tesr.rs:6153-6166 (opret refusal), and `[5]` tesr.rs:5787-5793 (`A_child` must equal the coordinator's registered aggregate for the child sid). |
| The last ancestor's declared `state.payload_vout` is unbound, so the `sp_vout` floor is vacuous | Real but inert: `[5]` tesr.rs:5787-5793 pins `A_child` to the parsed output's key, `:6153` refuses an opret, and a coloured child with ancestors is refused outright (`colored_child_txids`, tesr.rs:248-255). Nothing exploitable is left once V1/V2 land. |
| A conveyed bundle can skip terminalization and let the SE co-sign a rival | `[F2]` tesr.rs:5566-5570 (parent) and `:5651-5655` (every ancestor) — fail-closed on `terminal == false`. The child is deliberately exempt (the handover rotates the share instead), which is correct and is NOT a hole. |
| A prevout map keyed by txid would mis-value split children | Already keyed per-OUTPUT: `prevout_value_of` tesr.rs:5348, `live_csv_by_outpoint` tesr.rs:5376-5384, and both ancestor/child seedings (`:5726-5740`, `:5990-6004`). |
| The coloured builders trust a declared `out_value` | `tier_payload_prevout` tesr.rs:975-996 — declared vs parsed, at every coloured call site (`:1052`, `:1227`, `:1590`, `:3814`). It is the PLAIN builders that do not (V7). |
| The SE could refuse a value-violating tier | It cannot, and it must not be asked to: `PartialSignatureRequestPayload` (lib/src/transaction.rs:44-50) carries no transaction, `calculate_musig_session` uses `MusigSession::new_blinded_without_key_agg_cache`, and `cosign_tier_request` (lib/src/tesr.rs:503-528) takes the prevout amount as a caller parameter. Every fix in this document belongs on the RECEIVER. |

---

## 4. What this sweep did NOT cover

* **Nothing was executed.** Read-only by constraint: no SDK_E2E, no RGB_E2E, no probe bundle, no
  `cargo test`. `4e165e6` was proven by running it; nothing here is. Before landing, each of V1-V4
  deserves the same treatment — a hand-built bundle through the real verifier — because a
  source-level walk cannot rule out an ordering effect it did not model.
* **The colour half.** Only the STRUCTURAL colour checks were read (`verify_colored_shape`,
  `verify_colored_child_shape`). Whether a consignment actually assigns what the bundle claims —
  the engine-side predicate at accept time, `tokens::verify_consignment_assignment` and
  `validate_pending_token` — was not audited for this class. A coloured ladder's ASSET value has its
  own conservation question that this sweep did not ask.
* **The `params` schedule as an attack surface.** V5 notes that `bundle.params` is unbound and gives
  the one-line fix, but the consequences of a tampered schedule (CSV bounds, `check_exit_headroom`,
  `m_max`, floors) were not traced. That is a separate sweep with the same generative rule applied
  to TIME instead of VALUE.
* **The SGX lane.** All reasoning about the signing side is from `lib/` and `lockbox/`. `enclave/App`
  was not read. Every fix here is client-side so the lane split should not bite, but that is an
  assumption, not a verification.
* **The server/coordinator.** `num_sigs`, `aggregate_pubkey` uniqueness, `set_spend_budget`
  monotonicity and the pending-transfer lock were taken as stated by the client-side comments; the
  server code was not audited.
* **Watchtower and LN latch.** `watch_child_pass` / `exit_child_pass` were read only far enough to
  confirm they rebroadcast the conveyed chain and therefore provide no fallback. The keyless watch
  bundle's own value handling, and the HODL-latch lanes, were not swept.
* **Concurrency and crash recovery.** The journal / `crash_point` interaction with terminalization
  (`set_spend_budget` lands before the co-sign in both split lanes) was noted in V7/V8 only as the
  point of no return; replay behaviour was not analysed.
* **Non-value invariants generally.** Dust floors on the final exit leg, relay/TRUC package limits,
  and fee-bumping economics were out of scope even where they bound the same attacks.

---

## 8. Status after the fixes (2026-08-03, added by the orchestrator)

Eight commits landed against this document: `4e165e6`, `a063e3f`, `9c00140`, `deed25c`, `d692c07`,
`2ad2b2d`, `37d8bba` (plus `1160525`, journal). All four properties of §1's checklist are now pinned
on the child, ancestor and root lanes.

### Findings 4, 6 and 7 are closed TRANSITIVELY — the reasoning, so it is not re-derived

All three read a value out of a tier chain. That chain is now anchored end to end:

1. `37d8bba` binds the trigger's payload outputs to **`f_out.value`**, the on-chain funding output —
   the one number in the structure Bitcoin has already agreed to.
2. `deed25c` binds every root tier to the one above it (Σ payloads, so an extra output cannot hide).
3. `d692c07` binds the ancestor and leaf extension hops, and *where* each pays.
4. `4e165e6` / `a063e3f` bind the leaf's two hops and both `out_value` fields.
5. `2ad2b2d` pins `fee_rate`, the yardstick all of the above measure against, to the receiver's own
   network preset — without which every one of them is satisfiable by an inflated rate.

**Findings 4 and 6 (SSP pre-pay).** The gated `amount` is `child_amount` from `prepay_child_census`
(`transfer_receiver.rs:766`), which is `verify_conveyed_child`'s return, which is
`cb.child_state.out_value` (`tesr.rs:3383`). That field is bound to its signed output, that output to
the conservation law, its funding up the chain to the trigger, and the trigger to `f_out.value`. The
SSP now pays against a number provably reachable by the exit chain. **Closed.**

**Finding 7 (claim books `SP.out[j].value`).** Still true as written, and no longer a defect. The
booked value is the piece's funding; what the payee can reach is that minus the leaf's own two rungs,
and both hops are now bound — so the gap is exactly `2 × rung` (980 sat plain, 1 152 coloured at
2 sat/vB) rather than arbitrary. That is the intended accounting: you are credited the piece and its
exit costs come out of it. **Downgraded from theft to a bounded convention** — worth documenting for
wallet UI, not worth a verifier change.

### What is genuinely NOT covered

* ~~**No test proves the attacks are now refused.**~~ **Closed by `6777528` and `3bb6845`.** Four
  adversarial modules in `clients/libs/rust/src/tesr.rs` now build the skims and assert the refusal:
  `skim_leaf_attack_tests`, `skim_root_attack_tests`, `forged_yardstick_attack_tests`,
  `wrong_payee_attack_tests`. Each ships a non-vacuity control and an
  `assert_not_an_unrelated_refusal` guard, and each holds both halves of the aggregate key so no
  result turns on a forged signature. They found live holes on day one.
* ~~**`verify_bundle_ex`'s trigger-to-`F` anchor exists only on the CHILD lane.**~~ **Closed — see
  §8.1 below.**
* **The combine lane** (multi-input tiers) was not swept at all.

---

## 8.1 The trigger-to-`F` anchor, on both lanes (2026-08-03)

The tripwire `skim_root_attack_tests::gap_a_skimming_trigger_on_the_root_lane_is_still_accepted`
pinned the last gap this document names: `verify_bundle_ex` bound every tier to the one above it, so
the root ladder conserved value RELATIVE to the trigger's payload output, and nothing bound that
output to `F`. A sender whose trigger paid far less than `F` held got a ladder that conserved
perfectly against a fiction while the receiver booked the real on-chain value
(`amount: tx0_output.value`). `37d8bba` had closed this on the child lane only, inside the async
`verify_conveyed_child`, because `verify_bundle_ex` is synchronous and has no chain access.

**The shape of the fix, and why the shape mattered more than the arithmetic.** `TesrBundle` carries a
declared `f_value`, and using it inside `verify_bundle_ex` would have measured the chain against
another sender-declared number — a check that never fails, for honest and skimming ladders alike,
while reading in review like an anchor. Per `ADMISSION-INPUTS.md` a term with no provenance must be
left out of the calculation rather than dressed up. So the value is **passed in**, and each caller's
provenance is visible at its call site:

| entry point | what it supplies | provenance |
|---|---|---|
| `verify_bundle_bound` | `Some(coin.f_value)` | **chain** — `coin_authority_from_tx0` reads `tx0.output[vout].value` from the funding transaction, and the function has already refused any bundle whose `f_value` disagrees. Both production callers (claim, `transfer_receiver.rs`; SSP pre-pay, `prepay_flat_census`) fetch `tx0` over Electrum and refuse branch funding outright. |
| `verify_child_bundle` | `Some(parent_f_onchain_value)`, a new REQUIRED parameter | **chain** — beside the `parent_f_onchain_spk_hex` it already took. `verify_conveyed_child` passes `f_out.value`; `37d8bba`'s duplicate law moved down here, so every caller of the synchronous verifier inherits the anchor instead of one caller of one wrapper. |
| `verify_bundle` | `None` | **none, stated as such.** No chain access, so the trigger's hop is skipped. This entry point is documented as self-verification-only; it is not an acceptance path. |

`verify_bundle_ex`'s parameter is therefore `Option<u64>`, and `None` means *no anchor*, never *fall
back to the bundle*. The residue is asserted rather than left implicit: the last assertion of
`skim_root_attack_tests::a_skimming_trigger_is_refused_against_the_on_chain_funding_value` pins that
`verify_bundle` still accepts the skimming trigger, with the reason written down.

The law itself is the loop's existing one, run from `i = 0` with `F` as the trigger's funding, and it
branches on colour (`colored_tier_out_total` vs `tier_out_total`) — a coloured rung is 576 sat at
2 sat/vB against the plain 490, and omitting that branch bricks every coloured coin (`sdk77` caught
exactly that in `37d8bba`).

**Refusal ordering moved, and one adversarial control was updated to match.** A ladder built at a
forged fee rate now breaks at the trigger rather than at tier 1, because the trigger is the first hop
with an absolute yardstick. `forged_yardstick_attack_tests::a_forged_yardstick_ladder_is_refused_when_the_rate_is_declared_honestly`
now asserts the refusal **twice**: through `verify_root` (anchored — names `F` and the trigger) and
through `verify_bundle` (unanchored — falls to tier 1's relative law, with the numbers the control
was written with). Neither check is load-bearing alone, and the test now says so.

### Still open after this pass

* **GAP A / GAP B — the `fee_rate` yardstick is unbound on both synchronous verifiers.** `2ad2b2d`
  pinned it in `verify_conveyed_child` only. Both tripwires
  (`forged_yardstick_attack_tests::gap_*`) are still passing, i.e. still holes. The anchor added here
  does **not** close them: a ladder built *and* declared at 2 662 sat/vB satisfies the trigger law
  against the real `F` and still delivers 1 030 sat of a 1 000 000-sat coin.
* **`gap_an_ancestor_split_state_may_mint_value_out_of_nothing`** (`wrong_payee_attack_tests`) — an
  ancestor segment's split state is not held to a Σ law.
* **The combine lane**, as above.
