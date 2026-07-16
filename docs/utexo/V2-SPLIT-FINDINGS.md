# V2 split-transfer design — FLAWED verdict + two FATAL holes found in the LIVE V2 lane

Source: the V2-split-transfer design + adversarial review workflow (3 probes → design → 3 lenses → ruling).
**All three lenses returned FLAWED.** Critically, the two FATALs are **NOT split-specific — they are live on
the flat V2 transfer lane**, which the default flip (1b2c881) had made the default. **The default has been
reverted to V1 (`config.rs`) until they are fixed.**

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
