# First-class children — verified implementation plan (2026-07-28)

Produced by a 5-agent preflight that verified `V2-CHILD-FIRSTCLASS.md` against the CURRENT code,
plus a synthesis pass. Corrections to the design doc found by the preflight:

- The **ordering pitfall is already fixed** — `transfer_sender.rs::execute` already does every sender
  pre-sign (`create_transfer_signature`, `create_backup_transactions`, `presign_receiver_state`)
  BEFORE `get_new_x1` opens the transfer. No re-order needed.
- The **ancestor branch is already conveyed** inside `ChildTesrBundle.parent` (a full `TesrBundle`
  with `f_txid`/`trigger`/levels). No new wire field. Do NOT populate `TransferMsg.branch_txs` for a
  child — `required_terminal_ancestors` counts Σ inputs (3) against 1 real ancestor node and rejects.
- **Key-update does not consume a co-sign** (`lockbox` bumps `sig_count` only in the sign path), so
  the child census `child_num_sigs == CHILD_V2_BASELINE + 2` survives the handover.
- Child terminalization is exactly ONE line: `tesr.rs::in_ladder_split` `set_spend_budget(child,0)`.

---

# Implementation plan — first-class in-ladder split children

## VERDICT UP FRONT

**This is too large and too interdependent to land in one pass.** The five preflight agents collectively specified ~30 edits across 12 files spanning four independent risk surfaces (key handover, census generalization, terminality removal, onward-transfer sender route), plus a server SQL change to the predicate that gates *every* `sign_first`/`sign_second` call.

But there is a **clean seam** none of the agents found, and it inverts the doc's framing:

> Landing the handover **without** dropping terminality is a pure security gain (permanent sender lockout at claim, on top of terminality) with **zero** new theft window. Dropping terminality **without** re-spendability is a pure security *loss* with **zero** product gain.

So the doc's "must land as ONE change" is true only for `{drop terminality, census, onward re-transfer}`. The handover is separable, and separating it is strictly better than bundling it.

**Three commits, in this order.** Commit A is independently shippable and independently valuable. Commit B is the doc's interdependent set. Commit C un-pins sdk17.

Also flagged below: **a landmine none of the five agents caught** — the receiver agent's `locktime = Some(0)` would make every adopted child permanently "due for auto-refresh" (`refresh.rs:364`) and inflate every transfer quote (`transfer.rs:236`). Do not do it.

---

## 1. ORDERED EDIT LIST

### COMMIT A — "convey the key handover, complete it at claim" (terminality UNCHANGED)

Net effect: an adopted child becomes a structurally first-class coin (`server_pubkey`, `aggregated_pubkey`, `signed_statechain_id` populated; `A_child` proven invariant) and the sender is **permanently** locked out by auth rotation at claim time. The child stays terminal, so it is not yet re-spendable — but nothing regresses and no window opens.

**A1. `lib/src/transfer/sender.rs` — `create_child_conveyance_update_msg` (:125-165)**

New signature:
```rust
pub fn create_child_conveyance_update_msg(
    x1: &str,
    recipient_address: &str,
    child_coin: &Coin,
    transfer_signature: &str,
    child_tesr_bundle_json: &str,
) -> Result<TransferUpdateMsgRequestPayload, MercuryError>
```
Body: keep the decode + sid/auth extraction (:130-139). Add the `t1` construction, copied from `create_transfer_update_msg_with_branch` :174-181 but with `map_err` instead of the reference's `.unwrap()` at :178:
```rust
let client_seckey = PrivateKey::from_wif(&child_coin.user_privkey)?.inner;
let x1: [u8; 32] = hex::decode(x1)?.try_into().map_err(|_| MercuryError::ParseError)?;
let t1 = client_seckey.add_tweak(&Scalar::from_be_bytes(x1)?)?;
```
In the `TransferMsg` literal: `transfer_signature: transfer_signature.to_string()` (was `String::new()`, :143), `t1: t1.secret_bytes()` (was `[0u8; 32]`, :145), `protocol_version: 4` (was 3, :149). Leave `backup_transactions`/`branch_txs`/`terminal_parents` empty and `user_public_key: child_coin.user_pubkey` (already correct). Rewrite the doc comment :113-124 — "carries NO key handover … no `x1`/`t1` blinding" becomes the opposite of the truth.

`PrivateKey`, `Scalar`, `hex` are already imported; `MercuryError::ParseError` already exists.

**A2. `clients/libs/rust/src/tesr.rs` — `convey_child_bundle` (:573-629)**

Signature unchanged. Between the auth extraction (:589) and the message build (:612):

1. Defensive bind: `if cb.child_statechain_id != *statechain_id { return Err(anyhow!("child bundle sid != conveyed coin sid")) }`.
2. Derive the un-broadcast funding outpoint from the bundle — **not** from `child_coin.utxo_txid`, which is `None` for a slot created by `get_deposit_bitcoin_address` (`clients/libs/rust/src/deposit.rs:23-43`):
   ```rust
   let sp: Transaction = deserialize(&hex::decode(&cb.parent.current().state.signed_tx)?)
       .map_err(|_| anyhow!("child bundle SP is not a transaction"))?;
   let sp_txid = sp.txid().to_string();   // vout = cb.sp_vout
   ```
3. Build the sender binding **before** `get_new_x1` (preserves the `transfer_sender.rs:269-275` discipline — all sender work before the transfer is opened):
   ```rust
   let transfer_signature = mercurylib::transfer::sender::create_transfer_signature(
       recipient_address, &sp_txid, cb.sp_vout, &child_coin.user_privkey)?;
   ```
4. Capture the x1: `let x1 = crate::transfer_sender::get_new_x1(...).await?;` (currently discarded at :602-609).
5. Pass both into `create_child_conveyance_update_msg(&x1, recipient_address, child_coin, &transfer_signature, &json)`.
6. Rewrite the comment at :591-596 (the "no key handover / the returned x1 is unused" claims are now false). **Keep the batch-lock paragraph :597-601 verbatim.**

Do **not** populate `branch_txs`/`terminal_parents`: the ancestor chain `F→T→X_m→SP` already lives inside `cb.parent` and is validated by `verify_child_bundle` :1240-1265; routing it through `TransferMsg.branch_txs` would trip `required_terminal_ancestors` (`transfer_receiver.rs:528-536` counts Σ inputs = 3 against exactly 1 real ancestor statechain node).

**A3. `clients/libs/rust/src/transfer_receiver.rs` — `validate_encrypted_message` child branch (:587-604)**

Keep the `load_child` → `Err` idempotency guard (:597-599) and `verify_conveyed_child` (:602). After the census passes, add the transfer-signature binding (the flat-lane analogue of :640), gated on the version so in-flight legacy messages still validate:
```rust
if transfer_msg.protocol_version >= 4 {
    let sp_outpoint = TxOutpoint { txid: sp_txid, vout: cb.sp_vout };
    if !mercurylib::transfer::receiver::verify_transfer_signature(&coin.user_pubkey, &sp_outpoint, &transfer_msg)? {
        return Err(anyhow!("Invalid transfer signature"));
    }
}
```
Update the comment at :587-590.

**A4. `clients/libs/rust/src/transfer_receiver.rs` — `process_encrypted_message` child branch (:767-836)** — *the load-bearing edit*

Replace the body between the `cb` parse (:772-773) and the `return` (:835). **Exact order, mirroring the flat path at :872-913:**

1. Keep the `load_child` early-return (:776-778).
2. **Legacy fallback:** `if transfer_msg.protocol_version < 4 { <today's body verbatim>; return Ok(...); }` — a v3 message conveyed by an older client is still adoptable exit-only. Remove this branch in Commit C.
3. Parse SP once: `sp_hex`, `sp_tx`, `sp_txid`, `sp_out = sp_tx.output[cb.sp_vout]`, `sp_outpoint`.
4. `let statechain_info = utils::get_statechain_info(&cb.child_statechain_id, client_config).await?.ok_or_else(|| anyhow!("no statechain info for child sid"))?;` — the **child** sid, not the parent; this supplies `x1_pub`.
5. Clear the receiver lock bit **before** `/transfer/receiver`, and make it **non**-best-effort (replaces the `let _ =` at :788-792):
   ```rust
   let signed = mercurylib::transfer::receiver::sign_message(&cb.child_statechain_id, coin)?;
   unlock_statecoin(client_config, &cb.child_statechain_id, &signed, &coin.auth_pubkey).await?;
   ```
6. `create_transfer_receiver_request_payload(&statechain_info, &transfer_msg, coin)` → `send_transfer_receiver_request_payload`.
7. **`is_batch_locked` → return early BEFORE any coin mutation and BEFORE `persist_child`**, exactly as :887-893. (`transfer_receiver.rs:187` pushes `new_coin` unconditionally, so the coin must be left `INITIALISED` for the next claim to re-serve the message.)
8. `get_new_key_info(&server_pubkey, coin, &cb.child_statechain_id, &sp_outpoint, &sp_hex, network)` — passing the **un-broadcast SP hex** as `tx0_hex` makes it *require* `receiver_pub + new_server == SP.out[j]`'s output key (`lib/src/transfer/receiver.rs:764-766`), i.e. it proves the SE rotated correctly and `A_child` is invariant. It also yields `amount` and `signed_statechain_id`.
9. Book as first-class:
   ```rust
   coin.server_pubkey = Some(server_public_key_hex);
   coin.aggregated_pubkey = Some(nk.aggregate_pubkey);
   coin.aggregated_address = Some(nk.aggregate_address);   // supersedes the ad-hoc Address::from_script at :809-817
   coin.statechain_id = Some(cb.child_statechain_id.clone());
   coin.signed_statechain_id = Some(nk.signed_statechain_id);
   coin.amount = Some(nk.amount);                          // == sp_out.value; drop child_value at :806
   coin.utxo_txid = Some(sp_txid.clone());
   coin.utxo_vout = Some(cb.sp_vout);
   coin.status = CoinStatus::CONFIRMED;
   ```
   **`coin.locktime` STAYS `None`.** See §2 conflict C5 — `Some(0)` is a latent bug.
10. `crate::tesr::persist_child(...)` **LAST** — it is the adoption marker, so a mid-flight failure leaves the message re-claimable.
11. Keep the activity push and `transfer_receive_result.statechain_id = Some(child_sid)`.

**A5. Doc/comment truth-up (same commit, no behaviour):**
- `docs/utexo/V2-CHILD-FIRSTCLASS.md:91-99` — the ORDERING PITFALL block is **stale**. Rewrite as "DONE (7e23891)"; the real lines are `transfer_sender.rs:282/284/310/323` with the rationale at :269-275.
- `clients/libs/rust/src/transfer_receiver.rs:381-391` (peek doc), `:753-756` — "no key handover" is now false.
- `clients/tests/rust/src/sdk59_inladder_pay.rs:9, :87, :157` — same.

**A6. `clients/tests/rust/src/sdk59_inladder_pay.rs` — assertions (not just comments).** After Bob's claim, assert `bob_coin.server_pubkey.is_some() && bob_coin.signed_statechain_id.is_some() && bob_coin.aggregated_pubkey.is_some()`, and that `bob_coin.aggregated_address` equals the address of `SP.output[piece_vout].script_pubkey` (the `A_child`-invariance payoff). Assert `num_sigs(piece_sid)` is **unchanged** across the claim (keyupdate is sig-count-neutral, `lockbox/src/db_manager.cpp:525-573`). Assert `bob_coin.status == CONFIRMED` (sdk59:131's `unilateral_exit` refuses non-CONFIRMED, `wallet.rs:1060-1065`) and `bob_coin.locktime.is_none()`.

> **Commit A is atomic:** A1+A2+A4 must land together (the message format and its consumer). A3/A5/A6 are safe alone but belong in the same commit.

---

### COMMIT B — the doc's interdependent set: drop terminality (plain lane), child-superseded census, onward whole-child re-transfer

**Everything in Commit B lands together or not at all.** Dropping terminality without the onward route is a security loss for no gain; the onward route without the census is unreceivable at hop 2 (Carol computes `0 + 2` against `num_sigs == 3` and rejects every re-transferred child).

**B1. `clients/libs/rust/src/tesr.rs` — `in_ladder_split` (:328-334).** Delete `set_spend_budget(cc, wallet_name, &child_sid, 0)`. **Keep** `set_spend_budget(&parent_sid, 1)` at :302. Replace the `[F1]` comment with the new argument: pre-conveyance rivals are closed by the exact-equality census (:1372-1380); post-conveyance rivals by the pending-transfer lock (`server/src/endpoints/sign.rs:148/:323`), which is **released into a permanent lockout** by the receiver's auth rotation (`server/src/database/transfer_receiver.rs:242-250`). Update the fn doc :240-248.

**B2. `clients/libs/rust-sdk/src/transfer.rs` — `in_ladder_pay`, immediately after the `latch` match block (:748) and BEFORE `convey_child_bundle` (:752).** Re-apply terminality for the **latched lane only**:
```rust
if latch_batch.is_some() {
    mercuryrustlib::lightning_latch::set_spend_budget(
        &self.inner.cc, &self.inner.config.wallet_name, &piece_sid, 0).await?;
}
```
This is the LN carve-out (see conflict C1). Placement is load-bearing: `set_spend_budget` uses `fresh_auth` (`lightning_latch.rs:284` → `utils.rs:79-99`), so it must run while the sender still holds the child auth key, i.e. before the receiver's key update. Update the `in_ladder_pay` doc (:605-624) and the `transfer()` routing comment (:130-133), both of which still say "no key handover".

**B3. `clients/libs/rust/src/tesr.rs` — `verify_child_bundle` (:1190-1218) + `verify_conveyed_child` (:508-554).** Delete the `child_terminal: bool` parameter (:1200) and the `if !child_terminal` block (:1214-1218). **Keep `parent_terminal` (:1209-1213) and both censuses.** Rewrite the `[F2]` comment to the real duty split: an **ancestor** segment's census is a snapshot nobody in this transfer can refresh, so terminality pins it; the **leaf** segment's census is made durable by the receiver completing the handover, with the pending lock covering census→`key_updated`. In `verify_conveyed_child`, delete the child `get_spend_budget` fetch (:537-538) and its argument (:550) — one fewer round-trip per claim. Note the verifier must **tolerate** `terminal == true` on a child (latched lane) — it stops *checking*, it does not assert non-terminality.

**B4. `clients/libs/rust/src/tesr.rs` — extract `verify_superseded_segment`.** Move `verify_bundle_ex`'s superseded battery (:995-1152) into a private fn taking `(sup_states, sup_exts, agg_spk, params, &mut prevout_value_of, &live_csv_by_outpoint) -> Result<u32>`, returning `sups.len()`. Preserve both ordering properties: the pre-pass at :1000-1017 must insert superseded **outputs** into `prevout_value_of` before any validation (this is what makes transitive-death/renewal chains verifiable), and the map stays keyed per-`(txid, vout)`. `verify_bundle_ex`'s `expected = v1_backups + tiers.len() + superseded_ok` (:1158) is unchanged — **byte-identical behaviour for sdk54/55**. Do not write a second copy for the child; a second copy is how the `[S1]` count-padding class comes back.

**B5. `clients/libs/rust/src/tesr.rs` — child-superseded census.** Delete the blanket reject at :1291-1295. After both child tiers are parsed and co-sign-verified (after :1343), seed the child's own maps — note the difference from `verify_bundle_ex`, which seeds `live` from `txs[1..]` only (:1043) because the trigger has no CSV; a **headless child has no trigger, so both tiers seed it**:
```rust
child_prevouts: ((sp_txid, cb.sp_vout) -> sp_out.value) + every output of ext_tx and st_tx
child_live:     ((sp_txid, cb.sp_vout) -> ext_csv), ((ext_txid, 0) -> st_csv)
```
then `let child_superseded_ok = verify_superseded_segment(&cb.child_superseded_states, &cb.child_superseded_extensions, &child_agg_spk, &cb.parent.params, &mut child_prevouts, &child_live)?;` and replace :1375 with `let child_expected = child_v1_backups + 2 + child_superseded_ok;`.

The formula, stated once: `num_sigs(sid) == baseline + tiers + superseded`, where `baseline` is a property of the segment's **funding** — `PARENT_V2_BASELINE = 1` (:395) for the on-chain-rooted root (one `create_tx1`), `CHILD_V2_BASELINE = 0` (:400) for every derived slot. Handovers are census-neutral (`keyupdate` never touches `sig_count`).

**B6. `clients/libs/rust/src/tesr.rs` — NEW `child_retransfer` (whole-child onward hop).** A child cannot go through `transfer_sender::execute`: `tesr::load` returns `None` (a child has `ctesr-`, not `tesr-`), so it would fall into the B1-unsafe `split_coin`, and `create_backup_transactions` bails at `transfer_sender.rs:87-89`. Write a dedicated route that, given the receiver's address and the local `ChildTesrBundle`:
- builds a new child state over `ext_child.out[0]` one δ lower (`d0 − δ`, floored at `d_floor`), paying the **new** receiver (Model A), via `mercurylib::tesr::build_state_from`;
- co-signs it with `cosign_tier` under `A_child` (the receiver now holds the co-signing relationship — that is what Commit A bought);
- pushes the old `child_state` into `cb.child_superseded_states`;
- conveys via `convey_child_bundle` (unchanged — it already carries the handover after A2).

Exactly **+1** `num_sigs` and **+1** superseded per hop, which is what B5 counts.

**B7. `clients/libs/rust-sdk/src/transfer.rs` — routing (:60-81, :133-152, :159-181).** Do **not** merely delete the `child_claim_sids` exclusion — a selected child would silently take the B1-unsafe `split_coin` path. Replace it with dispatch: in the `Plan::Exact` handover loop (:159-181), route any id with a `ctesr-` bundle (`tesr::load_child`) to B6's `child_retransfer` instead of `transfer_sender::execute`. In the `WithSplit` arm, a child selected for splitting must be **refused with a typed error** until Commit C ("a received split child cannot yet be split again; send the whole child or exit it"). Adjust the selection filter accordingly: children become spendable **only** by exact/whole-coin selection.

**B8. `clients/libs/rust/src/tesr.rs::child_claim_sids` (:425-437)** — rename to `child_bundle_sids`, rewrite the doc: these are first-class coins whose **funding is un-broadcast**, not exit-only claims. `clients/libs/rust-sdk/src/wallet.rs:838-863` (`withdraw` → `unilateral_exit`): keep the routing, fix the rationale (the reason is the un-broadcast `SP.out[j]`, not "no SE key"). `wallet.rs:1099-1113` (`exit_child_pass`): **no change**. `clients/libs/rust-sdk/src/config.rs:76-81`: rewrite the "KNOWN V2 SEMANTIC / EXIT-ONLY" block.

**B9. `clients/tests/rust/src/sdk58_inladder_split.rs` — mechanical + a back-fill.** Drop the `child_terminal` fetch (:77) and the `assert!(child_terminal, ...)` (:79). Drop the arg from the `vcb` closure (:105-107) and the three call sites (:84, :110-118, :126-131). **Attack `I` (:118) may only be deleted AFTER its replacement is green** — `V1-DELETION-PLAN.md:17` forbids dropping a security assertion, and :52 records the retired sdk13/sdk14 as covered by "sdk58 H/I". The replacements, both stronger than the synthetic flag: **sdk60 PART F** (a *live* pre-conveyance hidden co-sign, caught by the census) and two new sdk58 cases — (I′) a disclosed child superseded state at a CSV ≤ the live `child_state` CSV over the same `ext_child.out[0]` must REJECT; (I″) a padded child superseded entry that is structurally valid but not co-signed by `A_child` must REJECT (the `[S1]` class, reachable on the child segment for the first time). Keep A–H and J. Promote `num_sigs` (:22-24) to `pub(crate)`.

**B10. `clients/tests/rust/src/sdk60_child_firstclass.rs` (NEW) + `main.rs`.** Full spec in §4. Register `pub mod sdk60_child_firstclass;` after `main.rs:75` and the `"60"` dispatch block after :326-329 (slots 60/61/62 are free).

---

### COMMIT C — un-pin sdk17 (child-level in-ladder split; the `ancestors` chain)

Commit B does **not** un-pin sdk17. sdk17 hop 2 (`sdk17_oor_chain.rs:98`, 10 000 of Bob's 20 000, asserting `used_split`) is a **non-exact split of a received child**, which needs a depth-2 ancestor chain, not a whole-child re-transfer.

**C1.** `ChildTesrBundle` gains `#[serde(default)] pub ancestors: Vec<ChildSegment>` (root→leaf order, empty for today's shape). `#[serde(default)]` is load-bearing: every persisted `ctesr-*` row and in-flight mailbox JSON keeps deserializing. `sp_vout` keeps its meaning relative to the immediately preceding segment (`cb.parent.current().state` when `ancestors` is empty, else `ancestors.last().state`).
**C2.** Extract `verify_child_segment(...)` from :1282-1380 and add `verify_child_chain(...)`, which fails closed if `ancestor_facts.len() != cb.ancestors.len()`, requires `terminal == true` for every ancestor, requires nothing for the leaf, and applies Model-A + value-binding to the **leaf only**. `verify_child_bundle` becomes a thin wrapper that hard-errors when `!cb.ancestors.is_empty()`.
**C3.** `child_exit_chain` (:442-448) must splice each ancestor's `extension` then `state` — **skipping this is silent fund loss**, not a verification gap.
**C4.** `transfer_receiver.rs:794-806` must read `cb.ancestors.last().state.signed_tx` when non-empty.
**C5.** `in_ladder_split` over a `ChildTesrBundle` parent (it currently takes `&TesrBundle` and reads `bundle.levels`).
**C6.** sdk17: delete the V1 pin (:47-48), replace the one-block exit with the sdk59:129-145 multi-pass loop, add the missing payoff assertion (funds land at Carol's backup key), and delete the private `is_outpoint_spent` (:23-34) in favour of `sdk40_tesr_consensus::is_outpoint_spent` — **the two copies disagree on the electrum-error case (sdk17 returns `true`, sdk40 returns `false`), so one of the two assertion directions currently fails open.**
**C7 (optional, separate review):** align `has_open_transfer` to the latch expiry and remove the B2 LN carve-out. See conflict C1.

---

## 2. CONFLICTS / DRIFT — resolved

**C1 — LN-latched child terminality. Agents split 2-2. RESOLVED: keep the budget on the latched lane (B2); do NOT change the server SQL now.**
The clock mismatch is real: `validate_batch` keeps the claim window open until `lightning_latch.expires_at − grace` (`server/src/endpoints/transfer_receiver.rs:189-205`), which for the external-hash latch used by non-exact LN pay is `now + 90000 s` (25 h, `server/src/endpoints/lightning_latch.rs:202`), while `has_open_transfer` releases a batch row at `batch_time + batch_timeout` = **120 s** (`server/src/database/transfer_sender.rs:71`, `server/Settings.toml:4`). Dropping terminality uniformly opens a ~25 h rival window on every non-exact LN payment.

The server/receiver agents wanted to lengthen the lock. **Decisive counter neither raised:** the SDK's *documented* V1 reclaim contract depends on the 120 s release — `reclaim_lightning_payment` (`clients/libs/rust-sdk/src/ssp.rs:1191-1202`) does a co-signing self-transfer through `transfer_sender::execute`, and its own doc says "Once the SE `batch_timeout` window has elapsed the latch is no longer held; re-transfer the coin to yourself". Lengthening the lock to the latch expiry breaks that for up to 25 h. Combined with the blast radius (the predicate gates *every* `sign_first`/`sign_second`), the carve-out is strictly cheaper and strictly more conservative: it preserves today's LN guarantee **exactly**. Record the residual (LN in-ladder pieces stay exit-only for the SSP) in `V2-LN-HODL.md`, and lift it in C7 with its own review.

**C2 — `verify_child_bundle` signature. RESOLVED: delete the `child_terminal` parameter (B3).** A dead fail-closed flag is a footgun. Three in-tree call sites (`tesr.rs:540`, sdk58 ×3); grep confirms no nodejs/web-wallet/uniffi consumer — there is no `#[uniffi::export]` on it.

**C3 — `protocol_version` 3 → 4. Agents split. RESOLVED: bump to 4 (A1), and keep a `< 4` legacy branch in the receiver (A4 step 2).** The gate is cheap insurance: without it a legacy `t1 = [0u8; 32]` message dies obscurely inside `SecretKey::from_slice` in `validate_t1pub` (`lib/src/transfer/receiver.rs:661-669`). The legacy branch is what makes a rolling upgrade non-destructive for messages already sitting in a mailbox. Every existing comparison (`transfer_receiver.rs:409/:667/:721/:1019`) is `>= 2` and unreachable from the child branch, which keys off `child_tesr_bundle.is_some()`.

**C4 — idempotency key inversion. All agents converge; the census agent stated it sharpest. RESOLVED: `persist_child` LAST, `is_batch_locked` returns before any mutation (A4 steps 7/10).** Today the bundle is persisted first (:779) and `load_child(...).is_some()` is the short-circuit (:776-778, mirrored at :597-599). Append the handover after the persist and any transient failure strands a child that is booked but never handed over — precisely the reverted state.

**C5 — `locktime` on the adopted child. NEW; contradicts the receiver agent's edit 8. RESOLVED: leave `locktime = None`.**
The receiver agent proposed `Some(0)` to satisfy `lightning_latch.rs:41-42`. That is a latent bug: `coin_near_final` (`clients/libs/rust-sdk/src/refresh.rs:364`) is `l.saturating_sub(tip) <= margin`, so `Some(0)` makes **every adopted child permanently "due for auto-refresh"** — `auto_refresh_due` (:277-283) would attempt a `reanchor` on an un-broadcast child on every pass (tolerated per-coin, but churn on every `auto_refresh_before_spend`), and `quote_transfer` (`transfer.rs:236`) would report a bogus renewal fee on **every** quote for that wallet. `None` is also the zero-drift choice: today's child branch never sets it, and sdk59/65/67 are green. `auto_exit_due` is unaffected either way — it keys on `estimate_exit_cost().exit_deadline_block`, which errors out for a coin with no backup txs (`wallet.rs:947-949`) and is skipped. If a child ever needs a RECEIVE latch, use the existing point-of-use placeholder trick (`transfer.rs:726-739`), not a wallet-wide field.

**C6 — behaviour change: child adoption becomes batch-gated.** Adding `/transfer/receiver` puts the child branch under `validate_batch` for the first time (today `get_statechain_transfer_messages`, `server/src/database/transfer_receiver.rs:119-142`, serves the message with no lock filter and the child branch returns before the standard flow). This is a **strengthening**, and it works in both LN lanes as ordered today — sdk65 calls `unlock_by_preimage` before `claim()` (`ssp.rs:504-510`); sdk67's `settle_receive` calls `confirm_pending_invoice` before the preimage-retrieve loop (`ssp.rs:718`), and Alice's watcher retries. But it is now timing-sensitive, which is exactly why A4 step 7 must propagate `is_batch_locked` rather than hard-coding `false` (:757-761).

**C7 — sdk60's hop 2 (exact) ≠ sdk17's hop 2 (non-exact).** Test agent flagged; census agent independently derived it. RESOLVED: sdk60 tests the whole-child re-transfer the doc specifies; sdk17 needs Commit C. **State this in `V1-DELETION-PLAN.md` rather than letting Commit B look like it clears the last feature gap.**

**C8 — doc drift.** `V2-CHILD-FIRSTCLASS.md:91-99` (stale ordering pitfall — all five agents), `tesr.rs:1186-1189` ("DORMANT + UNREVIEWED"), `tesr.rs:425-437`, `tesr.rs:328-334`, `transfer.rs:60-64`, `config.rs:76-81`, sdk59:9/:87. Fixed across A5/B1/B2/B8.

---

## 3. BLOCKING UNKNOWNS

**None.** Every technical question the five agents raised is settled in code and resolved above. The two they called "blocking" were scope decisions, now decided: the LN lane keeps terminality (C1); the change lands as three commits, and sdk17 is Commit C (C7).

One thing to **confirm with the operator before Commit B ships**, not a code unknown: Commit B moves the plain (non-LN) child from a **permanent** lockout (terminal) to the **same** guarantee every flat V2 transfer already has (temporary 1-hour pending lock + census + handover-at-claim). A receiver who stays offline past the 1-hour non-batch expiry (`server/src/database/transfer_sender.rs:70`) before claiming is exposed to a sender rival — but that is *identical* to the flat V2 transfer lane's residual today, not a new class. Framing it as "the child reaches parity with a normal transfer" is honest; framing it as "no reduction" is not.

---

## 4. VERIFICATION

Run recipe (`V1-DELETION-PLAN.md:96-102`), from an **isolated CWD** (E2Es collide on the shared `wallet.db`):
```
cd clients/tests/rust
SDK_E2E=<n> ML_NETWORK=regtest \
  RLN_REGTEST=$HOME/Claude/rgb-lightning-node/regtest.sh \
  COMPOSE_FILE=$HOME/Claude/rgb-lightning-node/compose.yaml \
  cargo +stable run
```
`cargo +stable` is mandatory. Also confirm `~/Claude/utexo-rgb-lib` is on `feat/spark`, not `pop`, or the test binary will not build.

**Before Commit A (baseline):** 59, 65, 66, 67 — record them green *before* touching anything.

**After Commit A, in this order:**
1. **59** — the primary canary. New handover runs here first. Watch `CoinStatus` (a non-CONFIRMED child breaks sdk59:131's `unilateral_exit`, `wallet.rs:1060-1065`).
2. **65, 67, 66** — the LN lanes, now batch-gated for the first time (C6). sdk67's 90 s timeout (:73) is the budget; if it flakes, the `is_batch_locked` retry path is wrong.
3. **12** (Part B, :117-119), **37** (:182-202, `ladder_census_ok`), **36**, **04** — child-census consumers.
4. **58** — must still be green *untouched* (Commit A does not change `verify_child_bundle`). If sdk58 moves in Commit A, something leaked.
5. **41, 49, 50, 47, 48** — the flat V2 transfer baseline (the 76bbcbb-era wedge was found this way).
6. **24, 25** — pending-row lifecycle.
7. **63, 64, 68** — whole-coin V2 LN + reclaim.

**After Commit B:** everything above, **plus** 54/55 (must be byte-identical after the B4 extraction), plus the edited 58, plus the new **60**.

### sdk60 — `sdk60_child_firstclass` (Commit B)

Constants `DEPOSIT=100_000`, `PAY=30_000`, `FEE_RATE=2.0`, regtest. Sizing (params `lib/src/tesr.rs:125-127`: d0=24, δ=6, d_floor=6, e0=12, e_floor=3): `T.out=99_512`, `X_0.out=99_024`, `SP` total `98_450` → piece 30_000 / change 68_450; Bob's child ext 29_512, state 29_024 — all above `min_child_value = 1306`. Drive claims **explicitly** (no background watcher), or PART B passes for the wrong reason.

| Part | Assertion | Theft window it closes |
|---|---|---|
| **A** | `!child_terminal && budget.is_none()`; `num_sigs(piece)==2`; `num_sigs(parent)==5`; `parent_terminal` | Pins B1+B3 landed and B1's parent terminalization survived |
| **B1** | Sender co-signs a rival state over `SP.out[j]` at `e_floor` while the transfer is open → Err. **Discriminating:** message contains `409`/`"open transfer"` and **NOT** `"spend budget"`. `num_sigs` unchanged after | **Post-conveyance rival during the pending window.** Without the discriminator the case passes for the OLD (terminality) reason and proves nothing |
| **B2** | `get_new_x1(piece_sid, …, charlie_auth)` → Err `"different recipient"` | **Post-acceptance re-address** — `insert_new_transfer`'s DELETE-by-statechain_id (`server/src/database/transfer_sender.rs:117`), the vector `V2-CHILD-FIRSTCLASS.md:20-24` names |
| **B3** | Same with **Bob's** auth → Err `"already exists"`, NOT `"different recipient"` | Proves B2 is recipient-scoped, not a blanket wedge |
| **C1** | Re-run B1 **after** Bob's claim → Err containing `401`/`"Signature does not match"`, **NOT** `"open transfer"` | **THE CORE CLAIM.** The temporary lock has been *released* by `key_updated` and the sender is now held out by the *rotated auth*. A C1 that still says "open transfer" means the handover never completed and the coin is protected only until the 1-hour expiry — the exact window the reverted attempt shipped |
| **C2-C5** | `server_pubkey`/`signed_statechain_id`/`aggregated_pubkey` set; `aggregated_address == addr(SP.out[j].spk)`; `num_sigs(piece)==2` still; `status==CONFIRMED`; `locktime.is_none()` | `A_child` invariance (pre-signed exit ladder still valid); census-neutrality of keyupdate; C5 landmine |
| **D** | `bob.transfer(charlie, PAY)`; `!r2.used_split`; sid unchanged; `num_sigs==3` (**exactly one** new co-sign — a 4 means a V1 backup ran and the census can never balance against `CHILD_V2_BASELINE=0`); rival co-sign → `"open transfer"`; `F` still unspent | The lock protects **every** hop, not just the first. Zero on-chain footprint across two off-chain hops |
| **E** | Charlie claims; `cb.child_superseded_states.len()==1`; `child_state.csv==18`; multi-pass exit (sdk59:129-145 loop); funds land at Charlie's backup key for `29_024`; `F` finally spent; Bob's stale coin → `401` | The multi-hop census counts honestly; Model A survives two hops with **no** extra on-chain tier |
| **F** | Low-level: between `establish_child` and `convey_child_bundle`, Alice co-signs a hidden rival state at `d_floor`. Assert the co-sign **SUCCEEDS** (`num_sigs==3`) — that is *why* the census is load-bearing — then convey; Dave's claim leaves balance 0 and no `ctesr-` bundle | **Pre-conveyance hidden state.** This is the live back-fill that must be green *before* sdk58 attack I is deleted (`V1-DELETION-PLAN.md:17`). Under pre-change code the co-sign fails with 410 `"spend budget exhausted"` — that difference is itself proof the change landed |

**Cheap extra hardening, worth adding in A4:** after `key_updated == true`, re-read `/info/statechain` `num_sigs` and refuse to book the child if it moved. Post-rotation the sender can never co-sign again (`server/src/endpoints/utils.rs:59-75`), so that read is final and authoritative — it makes the whole scheme robust to any future lock-expiry regression, including C7.

---

## 5. ROLLBACK

**Commit A:** plain `git revert`. It is additive on the wire (`protocol_version 4`) and the receiver keeps the `< 4` legacy branch, so a reverted client still adopts every child — including one conveyed by a v4 client, because A4 step 2's legacy path is a superset of today's behaviour. Nothing else depends on it. **No server state is left inconsistent:** a child whose handover completed simply has a rotated auth key that the reverted client cannot use — but the child is *terminal* through Commit A, so it was exit-only anyway and `exit_child_pass` still works from the persisted `ctesr-` bundle. Zero fund risk.

**Commit B:** revert restores `set_spend_budget(child_sid, 0)` and the `child_terminal` requirement — but **not retroactively**. Children created while B was live are permanently non-terminal: `sig_budget` is monotone-tightening (`server/src/database/deposit.rs:236-238`) and there is no writer that clears it (grep over `server/src` + `server/migrations` finds only `0005_spend_budget.sql:4`). So the reverted `verify_child_bundle` would **reject every in-flight B-era child** that has not yet been claimed. Mitigation, decided in advance:
- **Roll forward, do not roll back**, if any B-era child is in flight. The safe hot-fix is a one-line re-add of `set_spend_budget(child_sid, 0)` in `in_ladder_split` (stopping the bleed for *new* children) while leaving `verify_child_bundle` permissive, so in-flight children stay claimable.
- Only do a full revert from a state where no unclaimed B-era conveyance exists.
- Practical containment: gate B behind `UTEXO_CHILD_FIRSTCLASS=1` (default off) for one release, so the rollback is a config flip and the SQL clamp is never reached in production.

**Commit C:** `#[serde(default)] ancestors` makes the wire format backward-compatible in one direction only — a reverted client deserializing a **deep** bundle silently drops `ancestors`, which would make `child_exit_chain` produce a **broken exit chain (fund loss, C3)**. So Commit C's rollback requires the same "no deep bundle in flight" precondition, and the reverted `verify_child_bundle` must hard-error on a bundle carrying an unknown non-empty field rather than ignoring it. Ship C with `deny_unknown_fields` on the pre-C struct, or accept that C is one-way.

**Known-good state to return to:** `5fb15b7` on `feat/spark` — child conveyed exit-only, both sids terminal, sdk58/59/65/66/67 green.