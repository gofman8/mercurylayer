# CATS-B phase 1 — change 2 + V1 + V2, ordered for execution

> Scope: the sender's change leg becomes a one-cap **spine tip**; `ChildSegment` becomes one-or-two
> tiers; the ancestor census derives its addend from the verified tier count.
> Authority: `PARTIAL-PAYMENT-ECONOMICS.md` §4, including the 2026-08-03 CORRECTION to §4.5.
> Baseline: `feat/spark`, code at `5eddc0c` (docs at `9c00140`). Every line number below was read at
> that tree. **No test was run to produce this plan.**

---

## 0. Five decisions the research left open. Settled here, with reasons.

**D1 — `ChildTesrBundle.child_extension` becomes `Option<TesrTier>`, and `verify_child_bundle`
requires `Some` UNCONDITIONALLY.**
`scout:verifier` and `scout:blast-radius` both wrote an `Option`-branching leaf verifier. They are
wrong, and the §4.5 correction says so: at the leaf `[e_floor,e0] = [144,720]` is a *subset* of
`[d_floor,d0] = [144,1440]` (mainnet) and `[3,12]` overlaps `[6,24]` (regtest), so no CSV bound
separates a cap from an extension there — only the Model-A payee check at `tesr.rs:5869` would, and
that is one line carrying weight it was not designed for. `scout:builders` is right: the type is
`Option` (the tip needs it), the *conveyance* path demands `Some`, and the leaf's shape stays a
receiver-side code-path constant of the `final_is_split` kind. Consequence: `child_expected` at
`tesr.rs:5936` **keeps its literal `2`**.

**D2 — V4's new persisted key (`stesr-`) is NOT in this unit. The tip stays under `ctesr-`.**
A third prefix makes `parent_shape` (`transfer.rs:141-163`) fall through to `ParentShape::Unladdered`,
whose route is `split_coin` — the B1-unsafe plain split the whole design exists to replace. It also
silently disarms `wallet_is_provably_pre_sdk` (`transfer_sender.rs:492`), the `defend_ladders` child
loop (`wallet.rs:1879`) and `colored_child_sids` (`tokens.rs:1465`). `Option<TesrTier>` is a
**compile-checked** discriminator; a key prefix is a runtime string every reader must remember. V4's
stated purpose ("the tip must not be mistaken for a leaf") is served better by the type. Also note
`withdraw`'s `ctesr-`→`unilateral_exit` route is *correct* for a tip: its funding `SP.out[K]` is
un-broadcast, so there is no cooperative withdraw. §4.5's V4 row needs amending — see §7 Q3.

**D3 — whole-coin handover of a tip PROMOTES it to an ordinary two-tier child.**
`transfer()` sends an exact-amount payment out of any `ctesr-` coin to `child_retransfer`
(`transfer.rs:388`). Today the change leg is a two-tier child and that works. After change 2 it is a
tip with no extension, and `child_retransfer` (`tesr.rs:3635`) has nothing to hang the replacement
off. Building a lone lower-CSV state over the funding outpoint would convey a one-tier leaf, which
D1 forbids. Instead: build `ext` over the funding outpoint at `E0` and `state` over `ext.out[0]` at
`D0`, both co-signed; disclose the old cap as superseded. The new `ext` at 720 out-races the retained
cap at 1440 over the same outpoint. The receiver gets an ordinary two-tier leaf. Census `0 + 2 + 1 = 3`
= cap + ext + state. §4.5's "*a whole-coin handover of the tip adds exactly +1/+1*" is therefore
**wrong for the tip**: it is +2/+1. See §7 Q2.

**D4 — change 2 is PLAIN-LANE ONLY.** `colored_child_txids` (`tesr.rs:244`) already hard-refuses any
child with non-empty `ancestors`, and `colored_child_seals` (`:286`) emits a hard-coded five-entry
schedule. The coloured change leg keeps its extension in this unit. The coloured lane is therefore
already fail-closed against a spine; the refusal is made explicit rather than left to an accessor.
§4.5's RGB items 1–3 remain unbuilt.

**D5 — V5 (the 820-sat change floor) is NOT in this unit.** Leaving `min_child_value` = 1310 on both
legs is an **over-refusal**: a tip only needs 820, so every payment the current floor admits is still
buildable. Splitting `split_output_floor` / `inladder_amounts_floored` / `SplitPreflight` into two
named floors is a self-contained follow-up; doing it in the same commit as a shape change is how the
[B2] quote/executor disagreement gets reintroduced.

---

## 1. Commit order, and what breaks if you reorder

| # | commit | why it is its own commit |
|---|---|---|
| **C0** | Bind `TesrTier::out_value` to its own transaction on the plain child lanes | Independent of CATS, closes a **confirmed** griefing defect, and creates the exact "source of a split, parsed not declared" seam C2 needs. Unit-testable with no E2E. If it lands *after* C2, the `None` branch gets written against a declared field and the fix has to be redone. |
| **C1** | V1 type change + verifier + V2 census + exit chain + labels + depth cap + test compile fixes | **Must be one commit.** `child_exit_chain` (`:3117`) and `child_exit_labels` (`:3052`) are reconciled only by a length check in `child_exit_chain_bound` (`:3081`); split them and *every* conveyed child is refused with "internal: exit chain has N tiers but M labels" before any census runs. V1 without V2 is worse than either: the literal `2` against a one-tier segment is a **free census slot** that fails *open*, while honest one-tier bundles get rejected — so bring-up looks like a shape bug and the hole is invisible. The test crate is one binary (`sdk82:368` mutates `seg.extension.csv`), so the compile fixes cannot lag. |
| **C2** | Builders: `pieces`/`change` API, the `role` journal field, `establish_spine_tip`, `build_split_state_from`, the `None` split path, SDK call sites | This is where a `None` first EXISTS. C1 alone changes no behaviour — no producer emits `None` — so the whole existing suite must stay green across C1, which is the cheapest possible proof that the verifier rewrite is faithful. Landing C1+C2 together destroys that signal. |
| **C3** | `child_retransfer` promotes a tip (D3) | Depends on C2. **C2 must ship with a named refusal in `child_retransfer` for `child_extension.is_none()`** so the window between C2 and C3 is a legible refusal, not a wrong build. C2 and C3 must be in the same *release*, not necessarily the same commit. |
| — | V4 (key), V5 (floors), config.rs cost model, coloured spine, K>1 | Separate units. See §7. |

**Never split:** (a) `child_exit_chain` from `child_exit_labels`; (b) the type change from the census
expression; (c) `SplitJournalRecord::bundles()` (`:2273`) from `resume_in_ladder_split`'s completeness
test (`:2554`) — they encode one predicate twice, and a stale `resume` **co-signs an extension the tip
must not have**, permanently unbalancing its census (`num_sigs` is monotonic at the SE).

---

## 2. The exact type changes

```rust
// clients/libs/rust/src/tesr.rs:365-378 — ChildSegment
pub struct ChildSegment {
    pub statechain_id: String,
    pub funding_vout: u32,
    /// The segment's own ladder.
    ///
    /// `Some` — a PIECE segment: `extension` spends the funding outpoint, `state` spends
    /// `extension.out[payload_vout]`.
    /// `None` — a **CATS spine** segment: the single `state` (`SP_{i+1}`) spends the funding
    /// outpoint DIRECTLY at `SPINE_CSV`.
    ///
    /// ⚠️ `None` means STRUCTURALLY ABSENT. It is the opposite of `SplitJournalChild::extension`
    /// (`:2223`), where `None` means "not co-signed yet" — never copy a predicate between them.
    ///
    /// ⚠️ This field is NOT the source of truth for shape. serde deserialises a MISSING key as
    /// `None` for any `Option<T>`, with or without `#[serde(default)]`, so absence is free to an
    /// attacker. The shape is DERIVED from `state`'s signed input outpoint (which the taproot
    /// SIGHASH_ALL sighash commits to) and this field is then cross-checked against it.
    pub extension: Option<TesrTier>,
    pub state: TesrTier,
    #[serde(default)]
    pub superseded_states: Vec<TesrTier>,
    #[serde(default)]
    pub superseded_extensions: Vec<TesrTier>,
}
```

```rust
// clients/libs/rust/src/tesr.rs:150-176 — ChildTesrBundle
    /// Child ladder. `Some` on every CONVEYED piece — `verify_child_bundle` requires it
    /// unconditionally, so the leaf's shape is a receiver-side code-path constant, not a bundle
    /// field. `None` only on the sender's own **spine tip**, which is never conveyed and never runs
    /// through `verify_child_bundle`.
    pub child_extension: Option<TesrTier>,
    pub child_state: TesrTier,
```

Do **not** add `#[serde(default)]` to either (redundant), and do **not** add
`#[serde(skip_serializing_if = "Option::is_none")]` (omission and `null` must not be the same wire
form). `Some(t)` serialises byte-identically to a bare `t`, so a conveyed **piece** bundle is
unchanged on the wire for a pre-CATS reader; the only new wire form is `"extension": null` inside a
spine ancestor, which no pre-CATS verifier can admit anyway. Bump `TransferMsg.protocol_version`
(`lib/src/transfer/mod.rs:124`) to 5 and add `MIN_CATS_CHILD_PROTOCOL_VERSION` beside
`MIN_PREPAY_CHILD_PROTOCOL_VERSION` (`transfer_receiver.rs:302`) — see §7 Q4.

```rust
// clients/libs/rust/src/tesr.rs — ChildLadder (:1411)
pub struct ChildLadder {
    pub extension: Option<TesrTier>,
    pub state: TesrTier,
}

// clients/libs/rust/src/tesr.rs — SplitJournalChild (:2216-2237)
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SplitChildRole {
    /// A payee's piece: `extension` + `state`, two co-signs.
    #[default]
    Piece,
    /// The sender's own change: ONE cap over `SP.out[K]`, one co-sign, no extension.
    SpineTip,
}

pub struct SplitJournalChild {
    // ...
    /// Fixed at PLAN time by the code path that decided which leg is the change — so the tip's
    /// shape is a code-path constant, like `final_is_split`, never a nullable field read as a flag.
    /// `#[serde(default)]` ⇒ every pre-CATS `splitjrnl-` row replays as `Piece`, which is both
    /// correct (they are all pieces) and fail-closed (`Piece` demands two tiers).
    #[serde(default)]
    pub role: SplitChildRole,
    pub extension: Option<TesrTier>,   // unchanged type, unchanged meaning: "not co-signed yet"
    pub state: Option<TesrTier>,
    // ...
}
```

```rust
// lib/src/tesr.rs — NEW, beside build_split_state (:437)
/// [CATS] `build_split_state` rooted at an ARBITRARY outpoint. `build_split_state` hard-codes its
/// input vout to `UNCOLORED_PAYLOAD_VOUT` (`:466`); every spine batch after the first spends
/// `SP_i.out[K]` with `K = pieces.len() >= 1`.
pub fn build_split_state_from(
    prev_txid: &str,
    prev_vout: u32,
    prev_out_value: u64,
    children: &[(String, u64)],
    network: &str,
    csv_d: u16,
    fee_rate: f64,
) -> Result<TierTx, MercuryError>
```
Re-express `build_split_state` as `build_split_state_from(txid, UNCOLORED_PAYLOAD_VOUT, ...)` so
there is one body. Keep the `Σ children == tier_out_total(...)` conservation check.

---

## 3. The census arithmetic

Replace `clients/libs/rust/src/tesr.rs:5754`:

```rust
let expected = CHILD_V2_BASELINE + 2 + seg_superseded_ok;
```

with

```rust
// `seg_tiers` is set inside the shape branch, from the tiers this loop ACTUALLY verified — never a
// second read of `seg.extension`, never `live_ids.len()` (a HashSet silently collapses a duplicate
// txid and the ancestor loop has no repeat guard corresponding to `:5390`).
let expected = CHILD_V2_BASELINE + seg_tiers + seg_superseded_ok;
```

`CHILD_V2_BASELINE = 0` (a child slot is never funded on-chain, so `check_deposit`/`create_tx1`
never runs). `seg_tiers` is `1` on the `None` branch and `2` on the `Some` branch, assigned by the
same `match` that ran `verify_tier_cosigned`.

**Balance cases** (`num_sigs` is the SE's authoritative count for that segment's own statechain id;
each segment is a distinct id, so no level can compensate for another):

| # | segment | live tiers | superseded | `expected` | SE co-signs |
|---|---|---:|---:|---:|---|
| 1 | spine slot **at rest** (the tip's own record; never an ancestor, never verified) | 1 (`C_i`) | 0 | `0+1+0 = 1` | `C_i` = **1** ✓ |
| 2 | spine segment **after the next batch** | 1 (`SP_{i+1}`) | 1 (`C_i`) | `0+1+1 = 2` | `C_i`, `SP_{i+1}` = **2** ✓ |
| 3 | legacy **two-tier** piece segment (a received piece re-split) | 2 (`ext_child`, `CSP`) | 1 (`state_child`) | `0+2+1 = 3` | ext, state, CSP = **3** ✓ |
| 4 | a piece re-transferred **k** times, then re-split | 2 | `1+k` | `0+2+1+k = 3+k` | ext, state, k retransfer states, CSP = **3+k** ✓ |
| 5 | a tip **promoted by handover (D3)** and later re-split | 2 (`ext`, `CSP`) | 2 (`C_i`, promoted state) | `0+2+2 = 4` | cap, ext, state, CSP = **4** ✓ |

**The leaf is not touched.** `tesr.rs:5936` keeps `child_flat_backups + 2 + child_superseded_ok`,
because D1 makes a conveyed leaf unconditionally two-tier. Add a comment saying exactly that, and
that the `2` is provable from the `ok_or_else` guard at the top of the leaf section.

**The census is NOT what closes sender-declared shape.** It re-balances exactly under a demote (drop
one tier from the live list, add it to `superseded_extensions`: `0+1+1 == 0+2+0`). Three things close
it, and all three must be written deliberately:

1. **The prevout re-anchor** (the load-bearing one). On the `None` branch, `state` must spend
   `(fund_txid, seg.funding_vout)`. A genuine two-tier segment's state spends `ext.out[payload_vout]`
   and cannot be repointed — the outpoint is committed by the taproot SIGHASH_ALL key-spend sighash
   `verify_tier_cosigned` recomputes at `:6099-6101`.
2. **The `[SPINE_CSV, SPINE_CSV]` pin** at `:5702-5706`, unchanged and unconditional. `[0,0]` is the
   only interval disjoint from both `[e_floor,e0]` and `[d_floor,d0]`; it is what refuses the mirror
   attack (a real extension presented as the lone tier).
3. **The dead knob.** A spine segment has no honest writer of `superseded_extensions`. Refuse a
   non-empty list whenever `extension.is_none()`. Free, independent of (1), and it closes the
   re-declaration route directly.

---

## 4. Every edit, in dependency order

### C0 — bind `out_value` to its transaction (plain child lanes)

| file:line | edit |
|---|---|
| `clients/libs/rust/src/tesr.rs` (new, beside `tier_payload_prevout` `:973`) | `pub fn tier_payload_prevout_plain(t: &TesrTier, what: &str) -> Result<(String, u32, u64)>` — parse `t.signed_tx`, take `output[t.payload_vout]`, **refuse if `out.value != t.out_value`**, return `(txid, payload_vout, out.value)`. This is the plain twin of the coloured-only check at `:987`. |
| `clients/libs/rust/src/tesr.rs` (new) | `pub fn child_split_source(cb: &ChildTesrBundle) -> Result<(String, u32, u64)>` — the ONE derivation of "what a split of this child carves from". C0: always `tier_payload_prevout_plain(&cb.child_extension, "child extension")`. C2 adds the `None` arm. |
| `tesr.rs:3443` | `tier_out_total(cb.child_extension.out_value, ..)` → source value from `child_split_source`. |
| `tesr.rs:3464` | `build_split_state(&cb.child_extension.txid, cb.child_extension.out_value, ..)` → same source. |
| `tesr.rs:3525` | `cosign_tier(.., cb.child_extension.out_value, ..)` → same source value. |
| `tesr.rs:3635` / `:3669` | `child_retransfer`'s `build_state_from` + `cosign_tier` prevout value → same source. |
| `clients/libs/rust-sdk/src/transfer.rs:151`, `:1039`, `:1153` | `split_source_value: cb.child_extension.out_value` → `child_split_source(&cb)?.2`. Keeps quote and executor on one derivation ([B2]). |

Not in C0: `in_ladder_split`'s reads of `bundle.current().extension.out_value` (`:2624/:2652/:2672/:2763`)
and `renew`/`rollover`/`detrigger` (`:4257/:4261/:4329`). Those read a `tesr-` bundle this wallet
wrote; they are not attacker-supplied. Note it and move on.

### C1 — V1 + V2 (verifier, exit chain, depth cap, types)

**Types**
- `tesr.rs:372` `ChildSegment.extension` → `Option<TesrTier>` (§2).
- `tesr.rs:162` `ChildTesrBundle.child_extension` → `Option<TesrTier>` (§2).
- `tesr.rs:1411` `ChildLadder.extension` → `Option<TesrTier>`.
- `lib/src/transfer/mod.rs:124` protocol version bump; `transfer_receiver.rs:302` neighbourhood: add
  the CATS floor constant.
- `tesr.rs:73` `TesrLevel.extension` — **NO EDIT.** Making it optional inverts `is_extension = i % 2 == 1`
  (`:5283`) and leaves `final_is_split` as the only shape discriminator on the whole-coin lane. A
  spine tip physically cannot reach `verify_bundle_ex`: `TesrBundle` has a mandatory `trigger`,
  `exit_tiers()` emits `1 + 2·levels` (`:420-427`) and `:5226-5228` refuses anything else. Keep it so.

**The ancestor loop, `tesr.rs:5655-5760`** — rewrite as:

```rust
// Parse the state FIRST; its input outpoint is what derives the segment's shape.
let st_tx: Transaction = deserialize(&hex::decode(&seg.state.signed_tx)
    .map_err(|_| anyhow!("ancestor {i}: bad state hex"))?)
    .map_err(|_| anyhow!("ancestor {i}: state is not a transaction"))?;
if st_tx.input.len() != 1 {
    return Err(anyhow!("ancestor {i}: state must have exactly one input"));
}
let sin = st_tx.input[0].previous_output;

// [CATS V1] SHAPE IS DERIVED, NOT DECLARED. The input outpoint is committed by the taproot
// SIGHASH_ALL key-spend sighash `verify_tier_cosigned` recomputes, so it cannot be repointed
// without invalidating the SE's own signature. `seg.extension` is then a CROSS-CHECKED
// DECLARATION that must agree — never the source of truth. (serde deserialises a missing key as
// `None` for any Option, so absence is free to an attacker and must not decide anything.)
let is_spine = sin.txid == fund_txid && sin.vout == seg.funding_vout;
if is_spine != seg.extension.is_none() {
    return Err(anyhow!(
        "ancestor {i}: declared shape ({} tier(s)) disagrees with the signed state's input \
         outpoint — a segment's shape is derived from its signatures, not declared",
        1 + u32::from(seg.extension.is_some())
    ));
}

let ext_tx: Option<Transaction> = match &seg.extension { Some(x) => Some(parse(x)?), None => None };

let (seg_tiers, tier_txs, tier_list): (u32, Vec<&Transaction>, Vec<(&'static str, &Transaction, Option<u16>)>) =
  match (&seg.extension, &ext_tx) {
    (None, _) => {
        // [CATS V1] THE DEAD KNOB. A spine segment has no honest writer of superseded extensions;
        // admitting them is the exact route by which a demoted extension re-balances the census.
        if !seg.superseded_extensions.is_empty() {
            return Err(anyhow!("ancestor {i}: a one-tier spine segment discloses \
                {} superseded extension(s) — it has no extension to supersede", seg.superseded_extensions.len()));
        }
        // The prevout AMOUNT the sighash commits to is the FUNDING output's value, not `ext0.value`.
        verify_tier_cosigned(&st_tx, fund_out.value, &seg_spk)
            .map_err(|e| anyhow!("ancestor {i}: SPINE state not co-signed by its aggregate: {e}"))?;
        (1, vec![&st_tx], vec![("state", &st_tx, seg.state.csv)])
    }
    (Some(ext), Some(ext_tx)) => {
        let ein = ext_tx.input.first().ok_or_else(|| anyhow!("ancestor {i}: ext has no input"))?;
        if ext_tx.input.len() != 1
            || ein.previous_output.txid != fund_txid
            || ein.previous_output.vout != seg.funding_vout {
            return Err(anyhow!("ancestor {i}: extension does not spend its funding outpoint"));
        }
        verify_tier_cosigned(ext_tx, fund_out.value, &seg_spk)
            .map_err(|e| anyhow!("ancestor {i}: extension not co-signed by its aggregate: {e}"))?;
        let ext0 = ext.payload_out(ext_tx, &format!("ancestor {i} extension"))?.clone();
        if sin.txid != ext_tx.txid() || sin.vout != ext.payload_vout {
            return Err(anyhow!("ancestor {i}: state does not spend its extension's payload output"));
        }
        verify_tier_cosigned(&st_tx, ext0.value, &seg_spk)
            .map_err(|e| anyhow!("ancestor {i}: state not co-signed by its aggregate: {e}"))?;
        (2, vec![ext_tx, &st_tx], vec![("extension", ext_tx, ext.csv), ("state", &st_tx, seg.state.csv)])
    }
    _ => unreachable!(),
};
```

Then, unchanged from today except that it iterates `tier_list`:

- `:5688-5719` CSV bounds + `bind_declared_csv`. **`:5702` stays exactly as it is:**
  `if kind == "extension" { (p.e_floor, p.e0) } else { (SPINE_CSV, SPINE_CSV) }`. Both shapes' last
  tier is a spine state at `[0,0]`. **Do not add an `else if seg.extension.is_none()` arm.**
- `:5723-5730` `prevouts`: seed `(fund_txid, seg.funding_vout) → fund_out.value` in **both** branches,
  then iterate `tier_txs` (one or two entries).
- `:5736-5740` `live`:
  ```rust
  live.insert((fund_txid, seg.funding_vout), tier_txs[0].input[0].sequence.0 & 0xFFFF);
  if let Some(ext) = &seg.extension {
      live.insert((ext_tx.as_ref().unwrap().txid(), ext.payload_vout),
                  st_tx.input[0].sequence.0 & 0xFFFF);
  }
  ```
  **Only the second insert becomes conditional.**
- `:5741` `live_ids` from `tier_txs`, plus a new guard:
  ```rust
  if live_ids.len() as u32 != seg_tiers {
      return Err(anyhow!("ancestor {i}: the segment repeats a tier txid"));
  }
  ```
  (`verify_bundle_ex` has this at `:5390`; the ancestor loop does not, and a collapsing set
  under-reports the count by one — one free census slot.)
- `:5754` the census expression from §3.
- `:5761` `cur_tx = st_tx;` — **NO EDIT.** The state is the funding tier in both shapes.
- `:5765` `funding_payload_vout` — **NO EDIT-BUT-VERIFY.**

**The leaf, `tesr.rs:5798-5940`** — one new guard, then no branching at all:

```rust
// [CATS V1 / §4.5 CORRECTION] A CONVEYED piece is ALWAYS two-tier. This is a receiver-side
// code-path constant of the `final_is_split` kind, not a bundle field: at the leaf
// [e_floor,e0] ⊂ [d_floor,d0] on every profile, so no CSV bound separates a cap from an
// extension and only the Model-A check would — far more weight than that check was designed to
// carry. The one-tier SPINE TIP is the sender's own change; it is never conveyed and never runs
// through this function.
let child_extension = cb.child_extension.as_ref().ok_or_else(|| anyhow!(
    "conveyed child discloses no extension — a conveyed piece is always two-tier (fail-closed)"))?;
```
Then `cb.child_extension` → `child_extension` at `:5804`, `:5813`, `:5829`, `:5835`, `:5841`,
`:5903`, `:5914`, `:5976`. `:5936` `child_flat_backups + 2 + child_superseded_ok` — **NO EDIT**;
add a comment naming the guard above as the proof of the `2`.

**Exit chain + labels — same commit, no exceptions**

| file:line | edit |
|---|---|
| `tesr.rs:3125` | `if let Some(x) = &seg.extension { chain.push((x.signed_tx.clone(), x.csv)); }` before the state push. |
| `tesr.rs:3128` | `if let Some(x) = &cb.child_extension { chain.push(..) }`. |
| `tesr.rs:3058-3059` | push `"ancestor {i} extension"` only when `cb.ancestors[i].extension.is_some()`; same order. |
| `tesr.rs:3062` | push `"child extension"` only when `cb.child_extension.is_some()`. |
| `tesr.rs:3081-3088` | **NO EDIT-BUT-VERIFY.** Its length check is what catches a mis-pairing. If it fires on an honest CATS bundle, one of the two loops above is wrong — never relax the check. |
| `tesr.rs:3143`, `:3180`, `wallet.rs:4626` region | **NO EDIT.** `exit_child_pass`, `next_child_exit_tier`, `watch_child_pass_seen` iterate whatever the chain contains. |

**Coloured lane — explicit refusals (D4)**

| file:line | edit |
|---|---|
| `tesr.rs:257` | `colored_child_txids`: `self.child_extension.as_ref().ok_or_else(|| anyhow!("a coloured child cannot be a one-cap spine tip — the coloured spine (§4.5 RGB items 1-3) is unbuilt"))?`. |
| `tesr.rs:318-321` | `colored_child_seals`: same, through the `colored_child_txids()?` it already calls, plus the direct reads. |
| `tesr.rs:5976` | `let tiers = [("child_extension", child_extension), ("child_state", &cb.child_state)]` — uses the leaf guard's binding, so it stays two entries and `:6043`'s `consignments.len() != 2` stays right. |
| `tokens.rs:4278` | `cb.child_extension.txid.clone()` → through the same `ok_or_else`. |

**Depth cap — `tesr.rs:2880-2928`**

`per_level = vec![Some(p.ext_csv(0)), Some(SPINE_CSV)]` (`:2910`) is a constant that no longer
describes every level. Making it `vec![Some(SPINE_CSV)]` is **unsafe**: it would under-charge a chain
containing a re-split *piece* segment (two tiers), and this function is a P0-1-adjacent refusal.
Compute the real chain instead:

```rust
async fn enforce_split_depth_cap(
    cc: &ClientConfig,
    p: TesrParams,
    existing: &[ChildSegment],       // the ancestors the new children inherit ([] on the root lane)
    new_segment_has_extension: Option<bool>,  // None on the root lane (no new segment is created)
) -> Result<()>
```
Chain, broadcast order:
```
[None]                                    // T
[Some(p.ext_csv(0))]                      // X_m
[Some(SPINE_CSV)]                         // the parent's SP
for seg in existing:  if seg.extension.is_some() { [Some(<its ext csv>)] } ++ [Some(SPINE_CSV)]
if let Some(has_ext) = new_segment_has_extension:
    if has_ext { [Some(p.ext_csv(0))] } ++ [Some(SPINE_CSV)]
[Some(p.ext_csv(0)), Some(p.state_csv(0))]  // the leaf PIECE
```
Refuse when `exit_wait_blocks(&chain) > epoch_blocks`. Keep `SplitDepthCapExceeded`, filling
`max_depth` from `max_split_depth(&base, &per_level_of_the_new_segment, epoch)` for the message only.
`exit_wait_blocks` adds `+1` per tier, so a spine level costs `1`, never `0` — no divide-by-zero in
`max_split_depth` (`lib/src/transfer/receiver.rs:765`).
Callers: `tesr.rs:2650` → `(cc, p, &[], None)`; `tesr.rs:3425` → `(cc, p, &cb.ancestors, Some(cb.child_extension.is_some()))`.
The leaf tail is always a piece, because D1 forbids conveying a tip.

**Test-crate compile fixes (C1, mandatory — one binary)**

| file:line | edit |
|---|---|
| `clients/tests/rust/src/sdk82_exit_headroom_gate.rs:368-369` | `if let Some(e) = seg.extension.as_mut() { e.csv = Some(1); }` |
| `sdk82:371` | `if let Some(e) = forged.child_extension.as_mut() { e.csv = Some(1); }` |
| `sdk81_inladder_split_recovery.rs:200-203` | RE-DERIVE — see §5. |
| `sdk80_plain_child_split_watchtower.rs:320` | `cb0.child_extension.as_ref().expect("a conveyed piece is two-tier").txid` |
| `sdk70_verifier_binding_adversarial.rs:601`, `:667-669`, `:679`, `:731` | same `as_ref().expect(..)` / `as_mut()` form. |
| `sdk58_inladder_split.rs:174-176`, `:186`, `:295` | same. |
| `clients/libs/rust/src/tesr.rs:7503` | `extension: Some(tier("xc", 12))`. |
| `clients/tests/rust/src/chaos22_oracle.rs:230` | `(cb.ancestors.len() as u64) * 2 + 2 + ..` → `mercuryrustlib::tesr::child_exit_chain(&cb).len() as u64`. Feeds `report.max_branch_depth` only, so it fails no assertion — which is exactly why it would be missed. |

### C2 — the builders

| file:line | edit |
|---|---|
| `lib/src/tesr.rs:437-474` | add `build_split_state_from`; re-express `build_split_state` through it. |
| `tesr.rs:1424-1447` | add `establish_spine_tip(cc, tip_coin, sp_txid, sp_vout, sp_out_value, owner_exit_address, csv_d, fee_rate, network) -> Result<ChildLadder>`: one `build_state_from(sp_txid, sp_vout, sp_out_value, owner_exit_address, network, csv_d, fee_rate)` + one `cosign_tier(.., sp_out_value, ..)`. **The co-sign prevout value is `sp_out_value`, not `x.out_value`** — `establish_child` passes `x.out_value` at `:1443` because its state spends the extension. Returns `ChildLadder { extension: None, state }`. |
| `tesr.rs:2620` | `in_ladder_split(.., pieces: &mut [(Coin, String, u64)], change: Option<&mut (Coin, String, u64)>)`. Returns `pieces.len() + change.is_some() as usize` bundles, **tip LAST**. Makes cardinality (≤1 tip) and position (tip = last payload output) unrepresentable-if-wrong; an `Option<usize> change_index` or a per-child role enum does not. Keep the `pieces.is_empty()` refusal from `:2645`. |
| `tesr.rs:2662` | build `payees` from `pieces`, then push `change`. Tip's `sp_vout = sp.payload_vout + pieces.len()`. |
| `tesr.rs:2729-2750` | journal `pieces` with `role: Piece`, then the change with `role: SpineTip`. Journal order == returned bundle order. |
| `tesr.rs:2784` | iterate pieces then change; dispatch inside `establish_child_journalled` on `rec.children[j].role`. |
| `tesr.rs:2465-2487` | branch on `role` FIRST. `SpineTip` ⇒ skip the extension arm entirely; build the cap over `(rec.sp_txid, sp_vout, value)` at `csv_d = rec.child_state_csv`; keep the per-tier journal write and `crash_point`. `Piece` ⇒ unchanged. |
| `tesr.rs:2273` | `bundles()` gates on `role`, **bidirectionally**: `Piece` ⇒ `(Some, Some)` required; `SpineTip` ⇒ `extension.is_none() && state.is_some()` required. An extension PRESENT on a tip is an error, not something to pass through — it is a real co-signed rival over `SP.out[K]` at `e0`(720) that would beat the disclosed cap at `d0`(1440). |
| `tesr.rs:2554` | extract `fn ladder_is_complete(c: &SplitJournalChild) -> bool` and call it from `:2273` and `:2554`. One predicate, one place. |
| `tesr.rs:3408-3470` | `child_in_ladder_split`: same `pieces`/`change` split; `child_split_source(cb)` (C0) gains its `None` arm → `(funding_txid, cb.sp_vout, funding_tx.output[sp_vout].value)`, parsed from `cb.ancestors.last().map(|a| &a.state).unwrap_or(&cb.parent.current().state).signed_tx`; route the `None` case through `build_split_state_from`. `cosign_tier`'s prevout value must be that same parsed value. |
| `tesr.rs:3537` | `extension: cb.child_extension.clone()` — pass the Option straight through. `funding_vout: cb.sp_vout` (`:3536`) already carries the tip's real vout, so the verifier's spine branch lines up. |
| `tesr.rs:3635` | **C2 interim:** refuse `cb.child_extension.is_none()` by name ("a spine tip's whole-coin handover is not built yet — pay it in a batch, or see CATS-B C3"). C3 replaces this. |
| `clients/libs/rust-sdk/src/transfer.rs:1064-1075`, `:1183-1208`, `:1336-1347`, `:1535-1557` | all four already build "pieces then change, change last" — pass them separately. |
| `clients/libs/rust-sdk/src/transfer.rs:1682-1692` | keep the address-derived `ours` test AND cross-check it against `jc.role`; a disagreement is a corrupt record and must refuse, not pick one. |
| `clients/tests/rust/src/sdk58_inladder_split.rs:61`, `sdk70_verifier_binding_adversarial.rs:557` | new signature. |
| `tesr.rs:6140` `uncoloured_builder_census` | `establish_spine_tip` calls `build_state_from`, so it needs `refuse_uncolored_over_colored_child` or an explicit `GUARD_EXEMPT` entry, with the exemption written down. A stale `GUARD_EXEMPT` entry is itself a hard failure (`:6250`). |

### C3 — `child_retransfer` promotes a tip (D3)

`tesr.rs:3610-3700`. When `cb.child_extension.is_none()`:
1. `(fund_txid, fund_vout, fund_value) = child_split_source(cb)?`.
2. `cap_csv = cb.child_state.csv.ok_or(..)?`; refuse unless `p.ext_csv(0) < cap_csv`
   (replace-by-lower-timelock over the funding outpoint; mainnet 720 < 1440, regtest 12 < 24).
3. `ext = build_extension_from(fund_txid, fund_vout, fund_value, &A_child, net, p.ext_csv(0), rate)`,
   co-signed at prevout value `fund_value`.
4. `st = build_state_from(&ext.txid, ext.payload_vout, ext.out_value, &payee, net, p.state_csv(0), rate)`,
   co-signed at prevout value `ext.out_value`.
5. `next.child_superseded_states.push(cb.child_state.clone())` (the cap, CSV 1440, prevout = the
   funding outpoint — out-raced by the new ext at 720); `next.child_extension = Some(ext)`;
   `next.child_state = st`.
6. Keep the existing IN_TRANSFER-before-co-sign ordering and the store-before-convey ordering verbatim.

The `old_csv − delta` decrement (`:3625-3631`) does **not** apply on this branch: the promoted child
starts at a virgin `(E0, D0)`. It applies unchanged on the `Some` branch.

---

## 5. Tests

### Re-derive (existing tests whose expected values change)

| test | new expectation |
|---|---|
| `clients/libs/rust/src/tesr.rs:7455` `a_half_built_ladder_is_never_rebuilt_into_a_bundle` | keep the three `Piece` refusals. ADD: (a) `SpineTip` with `state: None` still refuses; (b) `SpineTip` with an extension PRESENT refuses; (c) `Piece` with `extension: None, state: Some` refuses; (d) `SpineTip` with `extension: None, state: Some` **rebuilds**, and the rebuilt bundle has `child_extension.is_none()`. |
| new, same module | a `SplitJournalChild` JSON with no `role` key deserialises as `Piece`. |
| `tesr.rs:7503` (deep-ancestor test) | `extension: Some(tier("xc",12))`; add a second case with `extension: None` and assert `split_op_id` (`:2580`) is unchanged — it keys off `seg.state.txid` and is shape-independent. |
| `clients/tests/rust/src/sdk81_inladder_split_recovery.rs:200` | the replayed **change** must satisfy `change_bundle.child_extension.is_none() && !change_bundle.child_state.signed_tx.is_empty()`; the replayed **piece** must satisfy both tiers non-empty. The test now proves the two legs have DIFFERENT shapes, which is stronger than what it proved before. |
| `clients/tests/rust/src/sdk60_child_firstclass.rs:160` | keep `sigs_after_hop1 == 2` for the piece. ADD `num_sigs(alice_change) == 1` — the only end-to-end evidence the tip's census baseline is right. ADD, after the next batch, `num_sigs(previous_tip) == 2`. |
| `sdk29:833`, `sdk32:356`, `sdk34:247`, `sdk77:455`, `sdk31:252`, `sdk82:450`, `sdk58:294-296` | `chain.len() == 5` for a **piece** — **NO EDIT-BUT-VERIFY**. If any of these break, change 2 has leaked onto the piece leg. |
| `sdk80_plain_child_split_watchtower.rs:378` | `assert_eq!(prevout_of(&cb1.child_state), prevout_of(&csp_seg.state))` — NO EDIT-BUT-VERIFY. If it fails, the CSP was built over the wrong prevout. |
| `lib/src/transfer/receiver.rs:1068` `mainnet_chain` fixture + `depth_cap_is_derived_from_the_schedule_and_the_epoch` | only if the depth-cap signature change reaches it. Re-derive from the real chain form in §4; do NOT leave it green-but-wrong. |
| `clients/libs/rust-sdk/src/invalidation_model.rs:326` `exit_cost_scaling_model`, `config.rs:166/186/204` | **NOT in this unit.** The current model over-counts a spine level; over-counting `tesr_exit_txs` inflates the auto-exit margin (early exits, safe) and over-counting the wait refuses depth (safe). Both are wrong-but-conservative. Re-derive them as a named follow-up: `tesr_exit_txs(d) = 4 + d`, per-level vbytes `TIER_VBYTES`, per-level wait `SPINE_CSV`. |

### Add — in-crate unit tests (C1, no E2E lane needed)

Build the tiers locally holding the "aggregate" key — exactly what a blind SE co-sign hands anyone.
This is how C1's `None` branch gets tested before any producer emits one.

1. `a_spine_segment_verifies_with_one_tier` — one co-signed state at `nSequence 0` spending the
   funding outpoint, `extension: None`, `superseded_extensions: []`, facts `num_sigs = 1`, → `Ok`.
2. `a_demoted_two_tier_segment_is_refused_by_the_prevout_reanchor` — a real `[ext@e0, state@0]`
   segment, `extension` set to `None`, the real extension moved into `superseded_extensions`, facts
   `num_sigs = 2`. Assert **`Err`**, and assert the message names the shape/outpoint, **not** the
   census. Then assert, in the same test, that `CHILD_V2_BASELINE + 1 + 1 == 2 == num_sigs` — i.e.
   the census *balances* and is provably not what refused it. This test IS the §4.5 correction.
3. `a_spine_segment_may_not_disclose_superseded_extensions` — case 1 plus one real superseded
   extension. `Err`, named.
4. `an_extension_may_not_be_presented_as_the_lone_tier` — `extension: None`, `state` = the real
   extension (CSV `e0`) spending the funding outpoint. `Err` from the `[0,0]` pin.
5. `a_spine_segment_may_not_declare_two_tiers` — `extension: Some(seg.state.clone())`. `Err` from the
   repeat-txid guard or the state-spends-its-extension link.
6. `a_conveyed_piece_may_not_be_one_tier` — `child_extension: None` on an otherwise valid leaf.
   `Err`, named, and reached **before** the census.
7. `the_spine_csv_is_not_a_legal_state_csv_on_any_profile` (`:6572-6595`) — NO EDIT-BUT-VERIFY. It is
   what keeps the `[0,0]` pin disjoint.

### Add — adversarial E2E, `sdk83_cats_spine_shape_adversarial` (§4.5's demand)

**Construct.** Alice deposits and claims (root ladder). Alice pays Bob a partial amount →
`in_ladder_pay` builds `SP_1` = [piece(Bob), tip(Alice)]. Alice pays Carol out of the tip →
`child_in_ladder_split` builds `SP_2`; Carol's bundle carries **one one-tier ancestor**. Separately,
Bob re-splits his piece to Dave → Dave's bundle carries **one two-tier ancestor**.

**Assert.**
- **A (honest, both shapes).** `verify_conveyed_child(Carol)` → `Ok`;
  `Carol.ancestors[0].extension.is_none()`; that segment's state CSV == `SPINE_CSV`;
  `num_sigs(tip_1) == 2`. `verify_conveyed_child(Dave)` → `Ok`;
  `Dave.ancestors[0].extension.is_some()`; `num_sigs(bob_piece) == 3`.
- **B (DEMOTE — the attack).** From Dave's bundle: set `ancestors[0].extension = None` and push the
  real extension into `ancestors[0].superseded_extensions`. Nothing signed changes. Assert
  `verify_child_bundle` → `Err`; assert the message names the shape/prevout link. Assert
  `0 + 1 + 2 == 3 == num_sigs(bob_piece)` — the census balanced and did not refuse it.
- **C (dead knob).** From Carol's bundle: push any real co-signed extension into
  `ancestors[0].superseded_extensions`, leaving `extension: None`. `Err`, named.
- **D (PROMOTE).** From Dave's bundle: `ancestors[0].extension = None`,
  `ancestors[0].state = <the real extension>`. `Err` from `[0,0]`.
- **E (INFLATE).** From Carol's bundle: `ancestors[0].extension = Some(ancestors[0].state.clone())`.
  `Err`.
- **F (leaf).** From Carol's bundle: `child_extension = None`. `Err`, named, and the message must be
  the two-tier one, not a census one.
- **G (headroom, sdk82 shape).** The forged one-tier declaration in B must not shorten
  `child_exit_chain_bound`. Assert B's refusal is a shape refusal, i.e. it happens whether or not the
  epoch has headroom — run B once at ample headroom and once near the boundary and get the same error.
- **H (exit).** Materialise Carol's chain end to end on regtest: `T → X_m → SP_1 → SP_2 → ext_child →
  state_child`, waiting each signed `nSequence`. Assert Carol is paid. This is what proves the
  one-tier ancestor is a real, spendable link and not just a verifier concession.

**Also add to an existing suite:** after Alice's second batch, assert
`enforce_split_depth_cap` admits a depth the pre-CATS `per_level` would have refused (the §4.4 win),
and that a chain mixing a spine ancestor with a re-split piece ancestor is charged the *sum* of their
real waits, not `d × 1`.

> Every test above must be RUN by the orchestrator. Do not report any of them green from this plan.

---

## 6. What NOT to do

1. **Do not wrap both `live.insert` calls at `:5736-5740` in `if let Some`.** Only `:5737` is
   conditional. Dropping the funding-outpoint key leaves the superseded cap `C_i` with no live entry
   to race; `:5167` misses, `:5189` cannot mark it dead, and `:5204` refuses it as an
   "orphan/threat branch". Every honest CATS bundle is rejected with a message accusing the sender of
   a hidden threat branch — and the tempting fix is to relax `:5204`, which IS the fail-open.
2. **Do not widen `:5702` for the `None` case.** `else if seg.extension.is_none() { (p.d_floor, p.d0) }`
   destroys the only disjoint interval in the schedule and lets a real co-signed extension be
   presented as the lone tier.
3. **Do not derive the tier count from `live_ids.len()` or from a parsed-tx vector.** A `HashSet`
   collapses duplicates and the ancestor loop has no repeat guard. Derive it from the same `match`
   that ran `verify_tier_cosigned`.
4. **Do not make `TesrLevel.extension` (`:73`) optional.** It inverts `i % 2 == 1` (`:5283`) and
   leaves `final_is_split` as the sole discriminator on the whole-coin lane.
5. **Do not overload `SplitJournalChild.extension`.** `None` there already means "not co-signed yet".
   A stale `resume_in_ladder_split` (`:2554`) calls `establish_child_journalled`, which at `:2465`
   sees `None` and **co-signs a fresh extension over `SP.out[K]`** — a live rival at CSV 720 beating
   the sender's own cap at 1440, and `num_sigs` is monotonic at the SE so the tip is permanently one
   over. It only fires after a crash. Use the `role` field.
6. **Do not pin the tip's cap to `[SPINE_CSV, SPINE_CSV]`.** The cap is the *slow* tier — it is what
   the next batch's `SP` must out-race. At 0 it ties, `replace-by-lower-timelock` fails, and the
   builders' own `s0_csv <= SPINE_CSV` guards (`:2636`, `:3434`) refuse the next batch, stranding the
   tip. The cap is a state tier at `p.state_csv(0)`.
7. **Do not use `unwrap_or(&cb.child_state)` anywhere.** In `child_retransfer` it builds a
   *descendant* of the cap instead of a rival: the old cap stays live, the sender keeps the coin, the
   handover transfers nothing, and the receiver sees 1 live + 0 superseded against `num_sigs` 2 —
   after the sender has booked the coin away.
8. **Do not reuse `build_split_state` for a spine batch.** It hard-codes its input vout to
   `UNCOLORED_PAYLOAD_VOUT` (`lib/src/tesr.rs:466`) *despite the comment on that line*. Every batch
   after the first spends `SP_i.out[K]`, `K >= 1`. The result is a tx naming piece zero's output while
   `cosign_tier` is handed the tip's prevout value — a signature that verifies against nothing, found
   only after `set_spend_budget` has terminalized the tip.
9. **Do not write `if let Some(ext) = &cb.child_extension` in `verify_child_bundle`.** It silently
   skips the extension's co-sign check, CSV bounds, `bind_declared_csv` and its `live` seed, and then
   forces `:5936` to be relaxed to match — which is precisely where the leaf shape becomes
   sender-declared. Use the unconditional `ok_or_else`.
10. **Do not add `#[serde(default)]` and expect it to matter, in either direction.** serde
    deserialises a missing key as `None` for any `Option<T>` regardless. Presence is enforced by the
    prevout derivation, not by serde. Say so in the doc comment.
11. **Do not lower any floor in this unit.** `split_output_floor` returns ONE number applied to both
    legs (`transfer.rs:2440/2470`); lowering it to 820 admits *piece* children below
    `min_child_value`, and `establish_child` runs after `set_spend_budget` — the parent terminalizes
    and then dies with `FeeTooHigh`, stranding a fully-funded coin for a payment the user thought
    succeeded.
12. **Do not read the `sup.csv <= live_csv` race check (`:5171`) as protecting a spine slot.** With
    the live tier at CSV 0 it is vacuous — `d_floor` and `e_floor` both exceed 0. The whole defence
    over that outpoint is the prevout re-anchor, the `[0,0]` pin, the orphan rule and exact equality.
    Anyone who reads the race check as load-bearing there will mis-scope the E2Es.
13. **Do not add a `kind`/`is_spine` boolean to `ChildTesrBundle`.** It crosses the wire
    (`transfer_receiver.rs:1271`). Dropping a tier is at least census-visible; flipping a boolean
    changes no count at all.

---

## 7. Open questions for the product owner

**Q1 — Coloured lane.** D4 keeps the coloured change leg two-tier, so coloured payments keep the old
cost and the old 720-block level while plain payments get the CATS win. Is a plain-first ship
acceptable, or does coloured CATS (§4.5 RGB items 1–3, including the **per-output blinding** leak at
K>1) gate the release?

**Q2 — Tip handover.** §4.5 says a whole-coin handover of the tip is "+1/+1"; the §4.5 CORRECTION
says the tip is never conveyed. Both cannot hold. D3 resolves it as promote-to-two-tier (+2/+1), which
preserves exact-amount payments out of the change coin at the cost of one extra SE co-sign. Confirm,
and amend the §4.5 census bullet. The alternative — refuse the handover — is a live capability
regression the moment change 2 lands.

**Q3 — V4.** D2 recommends *not* creating a third persisted prefix, because `parent_shape`'s
fall-through is `Unladdered` → `split_coin` (B1-unsafe) and three fail-open sites key on prefix
absence. §4.5's V4 row should be re-scoped from "new persisted key" to "shape accessor on the loaded
record". Confirm, or state the requirement the key satisfies that the type does not.

**Q4 — Wire compatibility.** A CATS bundle serialises `"extension": null` inside a spine ancestor;
a pre-CATS reader errors with `invalid type: null, expected struct TesrTier` and cannot claim it. A
conveyed *piece* bundle is byte-identical (`Some(t)` serialises as `t`), so the break is confined to
CATS-shaped conveyances that a pre-CATS verifier could not admit anyway. Recommend bumping
`protocol_version` and adding a CATS floor constant so the refusal is a named version refusal rather
than a parse error. Confirm a coordinated client upgrade is acceptable.

**Q5 — Watchtower.** §4.7 names the tower as a ship blocker. Two facts changed since §4.7 was
written: `WatchTrigger`, the dual predicate and `WatchState::Blind` all now exist
(`watchtower.rs:55-65`, `:283-303`), and the real hole is the unconditional `continue` at
`watchtower.rs:137` — `export_watch_bundle` has **no child arm at all**, so under CATS the sender's
entire remaining balance is silently absent from any exported bundle while the export returns
success. Is closing that `continue` a blocker for change 2, or for K>1?

**Q6 — Two pre-existing defects this work sits on top of** (both confirmed by code-read; neither is
created by CATS):
- `refresh()` / `refresh_sponsored()` on any `ctesr-` coin silently unilaterally-exits it and returns
  `Ok` (`refresh.rs:210` → `wallet.rs:1976`). CATS makes the tip the wallet's principal coin.
  Recommend a typed refusal in `reanchor` **before** `take_derived_tokens`.
- The claim lane books the **funding** value, not the exit value: `process_encrypted_message` discards
  `verify_conveyed_child`'s return (`transfer_receiver.rs:979`) and books `sp_out.value`
  (`:1321`/`:1389`). Even honestly that overstates a child by one or two rungs; a malicious sender can
  make the gap arbitrary by adding a second output to `ext_child`. The one-line fix is to book
  `cb.child_state.out_value` — which the verifier already binds (`:5882`) and already returns. It
  changes displayed balances, so it needs a product call rather than an engineering one.

---

## 8. Corrections this plan makes to the research it was given

- **`scout:verifier` / `scout:blast-radius` are wrong about the leaf.** Their `Option`-branching leaf
  verifier is exactly what the §4.5 CORRECTION forbids. `scout:builders` is right: unconditional
  `Some` on the conveyance path. `tesr.rs:5936` keeps its literal `2`.
- **`scout:verifier` is wrong that the `[0,0]` pin makes the kind unforgeable once shape is
  declared.** The pin is one-directional: it refuses an extension presented as the lone tier, and does
  nothing against a real state presented as the lone tier. The prevout re-anchor is the load-bearing
  check, and it does not exist in the file today.
- **`scout:builders` is wrong that "the type change needs NO default (old rows carry the key and
  parse to `Some`), and a truncated new row must be a parse error".** serde returns `None` for a
  missing `Option` field with or without the attribute. Presence cannot be enforced by serde here.
- **`scout:sdk` is right about every fail-open site a third prefix arms, and that is the reason not to
  create one** (D2), not the reason to co-edit them.
- **`scout:builders`' `change: &mut (Coin, String, u64)` is one notch too strict.** A payment that
  consumes the coin exactly has no change leg. `Option<&mut _>` keeps "at most one tip, last" while
  admitting the zero-tip case.
- **§4.5's V2 line reference `tesr.rs:4706` is stale** — the expression is at **`:5754`** (`:4706` is
  inside `exit_pass`). §4.5's V3 references `:4287`/`:4162` are likewise ~1 000 lines low.
- **Both `scout:builders` and `scout:blast-radius` propose `per_level = vec![Some(SPINE_CSV)]` in
  `enforce_split_depth_cap`. That under-charges a mixed chain** (a re-split piece segment still costs
  two tiers) and this function is a P0-1-adjacent refusal. Use the exact-chain form in §4.
- **`TesrParams::regtest` is `[e_floor,e0] = [3,12]` and `[d_floor,d0] = [6,24]`** — overlapping but
  *not* a subset, unlike mainnet. Both are non-disjoint, which is all the argument needs; the
  "strict subset" phrasing in the research is mainnet-only.
