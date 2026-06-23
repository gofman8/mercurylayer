# SE / lockbox protocol changes for off-chain split sub-coins (Stage 2+3)

**Status: PARTLY IMPLEMENTED + VERIFIED GREEN.** The single-use ledger (B below) is implemented and
verified on regtest by `rgb04` (deposit a single-use coin, co-sign spend #1, conflicting spend #2 is
REFUSED). The derived-deposit part (A) is not yet needed: `rgb03` shows the SE already co-signs spends
of **un-broadcast** sub-coins (their funding tx need not be on chain), so off-chain tree depth works
without a dedicated derive endpoint. Companion to `rgb_offchain_split_spilman.md` and the shipped Stage
2 resolver enabler (`validate_consignment_offchain_chain`).

> Implementation note (single-use threshold): a coin deposited via `fund_statechain` already carries
> **one** finalized SE signature (the unilateral-exit backup), so the terminal spend is the 2nd and a
> double-spend is the 3rd — `sign_first` refuses at `>= 2` finalized signatures. Coins funded another
> way (e.g. split sub-coins opened with `get_deposit_bitcoin_address` and no backup tx) have a
> different baseline; a fully uniform rule needs per-coin baseline tracking, or single-use coins
> skipping the deposit backup. Deploy gotcha (dev env): `docker compose build` caches; deploy server
> changes by `docker cp` + in-container `touch` + restart (see `rgb-statechain-integration` memory).

## Why this is needed

Stage 2 (chain a second split) cannot be tested without Stage 3, because **chaining a split means
spending the first split's *reserve* output while it is still un-broadcast**, and the SE can only
co-sign a spend of a coin it holds a key share for. So every split output (recipient sub-coin **and**
the new reserve) must be a first-class `{owner, SE}` statechain coin — even though its funding UTXO is
an output of a transaction that may never hit the chain. This doc specifies the SE/lockbox changes to
make that possible, and the single-use rule that keeps it safe.

## Baseline: how the Mercury SE works today (grounded in the code)

- **Deposit** (`server/src/endpoints/deposit.rs`): client presents a one-time **token** + an auth key;
  the server mints a fresh `statechain_id` (UUID), asks the lockbox `get_public_key(statechain_id)` for
  the server's key share, and `insert_new_deposit(token, auth_key, server_pubkey, statechain_id,
  enclave_index)`. The client aggregates `{user, server}` → the deposit address and funds it on-chain.
- **Key fact #1 — keygen is UTXO-independent.** The lockbox creates a key share for a *statechain_id*,
  not for a UTXO. On-chain existence/confirmation of the aggregated address is tracked **client-side**
  (`coin_status::update_coins` scans the chain); the SE never verifies it. → A coin whose funding output
  is un-broadcast is *already* representable: the SE side looks identical.
- **Key fact #2 — the SE is blind.** `sign_first`/`sign_second` (`server/src/endpoints/sign.rs`) run
  blind-MuSig2: the lockbox returns a nonce then a partial signature over a *challenge*, never seeing
  the transaction. Auth is a Schnorr signature by the current owner over the `statechain_id`. → The SE
  cannot see the split tree or validate RGB conservation.
- **Transfer** = rotate the server key share to the new owner's auth key; old state is invalidated by
  **decrementing-locktime backup txs** held client-side (not by the SE refusing to sign).

## The two gaps

1. **Every coin needs a fresh deposit token.** Sub-coins must be created *without* a token (the whole
   point is to amortize one on-chain deposit across many sub-coins).
2. **No hard "one spend per node" rule.** Today double-spend protection leans on key rotation +
   decrementing locktimes. A split tree has no locktime invalidation yet (that's Stage 4), so the SE
   must explicitly co-sign **at most one** spend of each node.

## Design

### A) Sub-coins = ordinary statechain coins via a *derived deposit*

A new server endpoint registers child coins authorized by the **parent owner's signature** instead of
a token:

```
POST /deposit/derive
  { parent_statechain_id,
    signed_parent_statechain_id,          // Schnorr sig by the parent's current auth key
    children: [ { auth_key }, ... ] }     // one per split output (recipient + new reserve)
→ { children: [ { statechain_id, server_pubkey }, ... ] }
```

For each child it calls the **same** lockbox `get_public_key(child_statechain_id)` (unchanged) and
inserts a `statechain_data` row with a new `parent_statechain_id` link and **no token**. The client
aggregates each `{child_user, child_server}` → the child's address, which it then uses as the
corresponding output of the split tx. (Children are registered **before** the split tx is built, so the
tx pays the `{owner, SE}` aggregates.)

### B) Single-use: a split is the parent's one terminal spend

A split spends the parent into its children. The SE treats it as the parent's **final** action:

- It co-signs the split **once** (ordinary blind-MuSig2), then marks the parent `statechain_id` as
  `SPLIT` (a terminal state) and **refuses any further `sign_first`/`sign_second` or transfer** for it.
- Child registration (A) + parent retirement must be **atomic** with authorizing the split, so a parent
  can never be split twice or split-and-also-transferred. This is the explicit **single-use ledger**.

### C) The SE stays blind (RGB correctness stays client-side)

The SE does **not** see the split tx or check RGB value conservation. Receivers validate the full
consignment chain off-chain (`validate_offchain_chain`, already shipped). The SE guarantees only:
*≤ 1 spend per node* + *holds child shares for exit*. A malicious owner can register children that don't
match the tx or don't conserve value, but that only produces an invalid consignment / unspendable
children — **no honest party is harmed**. Trust = "SE honest about single-use," the same profile as
Spark/Ark (and strictly better than a revocation model — see `rgb_offchain_split_spilman.md`).

## Flow (depth-2)

```
1. Deposit R (token)                      → statechain_id R, on-chain root, RGB 1000
2. /deposit/derive(parent=R,              → child coins: reserve R1' (700), sub-coin B (300)
      children=[R1', B])
3. Build split S1 (R → R1' + B), blind-MuSig2 co-sign with SE; SE marks R = SPLIT. NOT broadcast.
4. /deposit/derive(parent=R1',            → child coins: reserve R2' (500), sub-coin D (200)
      children=[R2', D])
5. Build split S2 (R1' → R2' + D), co-sign; SE marks R1' = SPLIT. NOT broadcast.
6. D's owner validates off-chain: validate_offchain_chain(consignment_D, [S1_txid, S2_txid]) → valid.
7. Exit: cooperative = SE co-signs one fresh spend (collapse); unilateral = broadcast S1, S2, … branch.
```

## What changes where

| Component | Change |
|---|---|
| `server` (Rocket) | new `/deposit/derive` (token-free, parent-authorized); single-use ledger (`statechain_data` gains `parent_statechain_id` + a `SPLIT`/terminal state); reuse `sign_first`/`sign_second` |
| `lockbox` (enclave) | **none required** for keygen/signing (already UTXO-independent & blind). Optional: an explicit "retire share" call once a node is `SPLIT` |
| DB | `statechain_data`: add `parent_statechain_id` (nullable) + status column; derived rows skip the token check |
| `mercurylib`/`mercuryrustlib` | a `derive_deposit` client call; `create_colored_split_tx` already builds the multi-output tx — wire child registration before building S |

## Deferred to Stage 4

Decrementing-locktime / Decker-Wattenhofer invalidation for **branch exit ordering**, the
`(SE & CLTV)` reserve for **operator reclaim**, and old-state **poisoning**. Until then, tree
single-use rests entirely on the SE's hard "one spend per node" rule (B).

## Open questions for review

1. **Atomicity** — fold derive-children + co-sign-split + retire-parent into a single authorized
   request so a parent can't be split twice? (Proposed: yes.)
2. **Blind vs. validating SE** — keep the SE blind (no sat/RGB checks), relying on client-side
   validation? (Proposed: yes — preserves Mercury privacy; honest parties unharmed.) The alternative
   (non-blind validating SE) would let the SE reject over-claims at signing but breaks blind-MuSig2.
3. **Authorization scope** — is a parent-owner Schnorr sig over `(parent_statechain_id ‖ child auth
   keys)` enough to authorize derivation?
4. **Single-use now vs. via Stage 4 locktimes** — implement the explicit `SPLIT` terminal state now?
   (Proposed: yes — simpler to reason about; locktimes are an additive Stage-4 defence.)
5. **DoS** — token-free derived deposits are gated by a valid parent; acceptable, or add a quota?
6. **Exit/backup txs for sub-coins** — a sub-coin's unilateral exit must broadcast its parent branch
   first; confirm the backup-tx set + (Stage 4) locktimes model this branch correctly.
