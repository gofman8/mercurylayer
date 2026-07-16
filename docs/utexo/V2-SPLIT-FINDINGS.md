# V2 split-transfer design — FLAWED verdict + FATAL holes found in the LIVE V2 lane

## 🔴 B1 — LIVE THEFT VECTOR (was shipped in the V2 default at 434d334; default REVERTED)

**Not a future design constraint — an exploitable theft in the shipped default. No collusion, one SDK call.**

| Fact | Evidence |
|---|---|
| `T` has NO timelock, spends `F`, fully co-signed at `establish` | `lib/src/tesr.rs:145` (`TRIGGER_SEQUENCE=0xFFFF_FFFD`), `:159` `lock_time:0`, asserted `:352-353`; co-signed `clients/libs/rust/src/tesr.rs:130` |
| The split tx spends the SAME `F` | `lib/src/transaction.rs:402-412` builds its input from `coin.utxo_txid/vout`; `clients/libs/rust/src/tesr.rs:125-126` sets `f_txid/f_vout` from the same fields |
| `split_coin` had NO ladder check | `clients/libs/rust-sdk/src/transfer.rs:387-406` — guards were CONFIRMED / !dup / !carrier only |
| Every V2 transfer leaves the SENDER a retained `T` | `presign_receiver_state` clones and never deletes (`tesr.rs:299,302`); there is NO deletion path for `tesr-*` |
| The SDK performs the attack | `unilateral_exit(Some(ids))` filters the explicit-id branch on carrier status only (`wallet.rs:1016-1028`), then `exit_pass` broadcasts `T` with no `outpoint_spent` gate (`tesr.rs:389-406`) |

**Attack:** Alice receives a V2 coin → pays Bob a non-exact amount (⟹ `split_coin`; Bob gets a **V1**
sub-coin, `transfer_sender.rs:312`, funded by the un-broadcast split tx) → Alice `unilateral_exit`s the
PARENT. `T` confirms, `F` is consumed, **Bob's split tx is permanently dead**, Alice's ladder pays her the
full parent value.

**The race is rigged:** `T` is v3/TRUC + P2A — fee-bumpable by anyone, forever. The split tx is v2 with a
frozen fee and no RBF headroom (the parent's SE budget is set to exactly 1 and the split consumes it,
`transfer.rs:458-464`). Alice wins deterministically.

**Bob's due diligence is meaningless:** his `terminal_parents` check (`transfer_sender.rs:290-294`) returns
true. He cannot see `T` — the ladder is never conveyed on the V1 lane and the SE has never seen it
(`tesr.rs:47`).

**The code's load-bearing claim is FALSE for V2** (`transfer.rs:455-457`: *"No later withdraw/transfer/
backup of the parent can be signed — the branch cannot be double-spent even by a malicious sender"*). It
rests on the V1 premise that every spend of `F` needs a FRESH SE co-sign. V2 breaks it at the root: `T` was
co-signed at `establish`, long before `set_spend_budget`. **A budget bounds future co-signs; it cannot
retract an issued signature.** Strictly WORSE than V1, where the parent's backup is locktimed above the
branch and the branch always matures first (INV-4).

### Status
- **Default REVERTED to V1** (V2 opt-in via env) — exposure closed.
- **HF-1 landed**: `split_coin` refuses a laddered coin (hard error beats silently voiding the receiver).
- **Follow-ups still open**: HF-2 (teach the planner `splittable`, so HF-1 doesn't turn into a hard
  transfer() failure when a safe coin exists); HF-3 (same gate in `mint_piece`, `transfer.rs:365-380`);
  HF-4 (`unilateral_exit` explicit-id branch must filter on `CoinStatus` — a WITHDRAWN parent must not be
  exitable; stops the SDK being the weapon and kills the accidental-loss variant); HF-5 (delete the false
  claim at `transfer.rs:455-457`).

### THE FIX — in-ladder split (V2-DESIGN §5.4), not a gate
The gate is a feature amputation: `presign_receiver_state` runs on EVERY V2 transfer, so essentially every
circulating V2 coin is conveyed-ladder ⟹ V2 coins could not pay non-exact amounts (violating REQ-2). Its
escape hatch (`reanchor`, 112 vB + a confirmation per non-exact payment) negates V2's "0 vB rent / ~320×
footprint win" — and may not even work (does `withdraw` on a conveyed V2 coin have SE sign-budget headroom?
external review F2 / migration 0008 — MUST be checked).

V2-DESIGN §5.4 already specifies the answer: a split `SP` is a **state tier** spending `X_m.out[0]` at
`nSequence Δ_{k+1}`, N child resting outputs, Σout = Σin − fee_committed, + P2A; **no trigger needed** (SP
is itself un-broadcast). `build_trigger` (`lib/src/tesr.rs:236-248`) is the ONLY builder that touches
`f_txid/f_vout`; every other tier spends its parent's output and `verify_bundle` enforces it
(`clients/libs/rust/src/tesr.rs:481-487`). So an SP is a **DESCENDANT of `T`, not a rival** — a retained
trigger has nothing to race. This dissolves B1 rather than mitigating it, and makes split pieces real V2
coins (retiring the V1 split lane + letting the 12 V1-pinned tests migrate).

---

## S1 — FATAL (fund-loss): `verify_bundle`'s exact-count linchpin is paddable
`clients/libs/rust/src/tesr.rs:508-529`.
```
expected = v1_backups + tiers.len() + superseded_states.len() + superseded_extensions.len()
```
- Exit tiers are parsed + structurally checked (`:458-503`). **Superseded entries are only `.len()`-counted**
  — never deserialized, never linked to the ladder, never signature-checked.
- Check 4 is `if let Some(csv) = s.csv { ... }` — an entry with **`csv: None` is silently skipped**.

**Attack.** A sender who has reached `num_sigs = N+1` (one hidden low-CSV state paying themselves) pads one
junk `TesrTier { txid:"", signed_tx:"", out_value:0, csv:None }`. `expected` becomes `N+1` ⟹ check 3 passes;
check 4 skips the `None`. `verify_bundle` **ACCEPTS**. The receiver takes the coin; the sender later
broadcasts the hidden state (which matures first) and takes it back. The doc-comment at `:433-436` claiming
a hidden state is "impossible" is **false**.

**Root cause (mine).** `superseded_states` was added to fix sdk49's count mismatch (full-disclosure
counting) — introducing attacker-controlled terms into the very equation meant to make hidden states
impossible.

**Required fix.** Every term of `expected` must correspond to a *real, co-signed* tier of *this* ladder:
1. **Parse** every superseded entry (reject bad/empty hex) — no unparseable tier may contribute.
2. **CSV required, unconditional**: `let csv = s.csv.ok_or(...)?` then require `csv > final_csv` (states).
3. **Ladder linkage**: each superseded state must spend a disclosed-or-superseded extension's `out[0]`;
   each superseded extension must spend the trigger's or a level state's `out[0]`.
4. **Signature verification** (the real guarantee): parsing does not prove a co-sign — a structurally valid
   but never-co-signed tx would still pad the count. Each counted tier's witness must verify as a valid
   schnorr/MuSig2 signature by the aggregate key `A` over its own sighash. Without this, `expected` remains
   forgeable. (Note the *exit* tiers are not signature-verified today either — same class.)
5. Unit tests: `csv:None`, empty `signed_tx`, off-ladder outpoint, unsigned tier → each must REJECT, and be
   shown ACCEPTED before the patch.

## S2 — FATAL (fund-loss): `v1_backups` is attacker-supplied and unvalidated on the V2 path
`clients/libs/rust/src/transfer_receiver.rs:600-604` passes `transfer_msg.backup_transactions.len()`
straight into `expected`; `:635` gates `validate_signature_scheme` on `protocol_version < 2`, so **nothing
inspects the vector's interior on V2** (only `get_tx0_outpoint` first-by-`tx_n` and
`verify_latest_backup_tx_pays_to_user_pubkey` `.last()`).
- **Count padding**: duplicate `tx1` → `[tx1, tx1, tx2]`. Same prevout ⟹ one group; first/last unchanged.
  `expected` inflates to match a hidden-state `num_sigs`. Middle entries need not be valid txs. Unbounded.
- **Locktime inversion**: build the receiver-paying backup at `L+interval` (not `L−interval`) and retain
  one's own at `L`. INV-5 (`ladder_decrements_by_interval`) is the only enforcement — and it is skipped.

**Strict V1→V2 regression**: V1 rejects both (`:615` + `validate_signature_scheme`). Root cause (mine):
V2DEF-1 gated the V1 structural check off for V2 without replacing it.

**Required fix.** Validate the backup vector structurally on the V2 path too (count, per-group uniqueness,
decrementing-locktime), or derive `v1_backups` from an authoritative source rather than the sender's vector.

## B0 — FIXED (commit 5c42667): root-only laddering
`claim()`'s establish loop filtered only `CONFIRMED/!dup/!single_use/!carrier` — no root check — so a split
**sub-coin** got laddered under the V2 default; its `F` is an un-broadcast split output ⟹ `exit_pass`
broadcasts a trigger with no prevout ⟹ silent stall, `wait_blocks:0` forever ⟹ unexitable via the SDK.
Fixed by requiring F on-chain (electrum `transaction_get`), fail-closed. (An earlier `branch-<id>` proxy
broke root laddering because `get_backup_txs` does `fetch_one(..)?` — a missing row returns `Err`, conflating
"root" with "db error"; sdk48 caught it.)

## B1 — structural constraint on V2 split-transfer (not yet addressed)
The conveyed trigger has **no timelock** (`TRIGGER_SEQUENCE = 0xFFFF_FFFD`, `lib/src/tesr.rs:145`), so every
prior owner of a V2-adopted coin holds a free, immediate spend of `F`. A branch-conveyed sub-coin's root hop
is the split tx, which **also** spends `F` ⟹ unwinnable fee race whose loser is the sub-coin's receiver, who
cannot detect the exposure at accept time. ⟹ V2 split-transfer must refuse to ladder a sub-coin whose parent
ladder was ever conveyed, or give the trigger a CSV. Also: tiers are v3/TRUC and the split tx is v2, so a v3
trigger cannot relay while the split tx is unconfirmed — **F must confirm, not merely be broadcast**.

## Verdict / order of work
1. **DONE** — revert the default to V1 (exposure closed); B0 fixed.
2. **NEXT (blocking)** — fix S1 + S2 with adversarial review + E2Es proving padding/inversion REJECT.
3. Only then re-flip the default to V2.
4. V2 split-transfer (B1 + the exit/branch-awareness defects) remains follow-on.
