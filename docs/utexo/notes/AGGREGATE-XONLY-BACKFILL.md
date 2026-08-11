# `aggregate_xonly` backfill — the proposed fix cannot work, and here is why

Investigated 2026-08-11 against the live coordinator DB. AUTHOR-ATTESTED.

## The claim being tested

`clients/libs/rust/src/tesr.rs:8063-8066` states the remedy for legacy coins that cannot carry a
bound ladder:

> The only complete fix is coordinator-side: backfill `aggregate_xonly` for the legacy rows from the
> coordinator's OWN columns (`x_only(user_public_key + server_public_key)`), which is the same value
> deposit-init records today and involves no client input.

`SPEC-ROADMAP.md` WP4 carries it as a gate ("Zero NULL `aggregate_xonly` rows"), and `DECISIONS.md`
D8 schedules it. **It cannot be done.**

## Measured

```
                                                    rows
statechain_data total                              11 948
  aggregate_xonly IS NULL                            8 155   (68%)
  …of those, backfillable (both operands present)        0
  …of those, missing user_public_key                 8 155   (100%)
  …of those, missing server_public_key                   0

aggregate_xonly IS NOT NULL                          3 793
user_public_key IS NOT NULL                          3 793   (identical set)
```

**`x_only(user_public_key + server_public_key)` has no first operand.** Migration 0009 added
`user_public_key` and `aggregate_xonly` *in the same statement*, so they are populated in exact
lockstep: every row that has one has the other, and the 8 155 legacy rows have neither. The
coordinator never stored the owner's share before 0009 — which is precisely what migration 0009's own
header says ("The server stored `server_public_key` per coin but NOT the owner's signing share, so it
could not compute or record the coin's aggregate key"). The proposed backfill would run, match zero
rows, and report success.

**These are not dead records.** 3 637 of the 8 155 have transfer history, i.e. they are live coins
with real owners.

## What this actually means

A pre-0009 coin **cannot be given a bound ladder by any coordinator-side action**, because the
missing value is the *owner's* public share and only the owner holds it. Consequences, stated
plainly:

* The gate at `tesr.rs:8049-8100` is doing the right thing and must stay: it keeps such a coin
  un-laddered rather than minting an unclaimable ladder. That is not a workaround, it is the correct
  terminal behaviour for a coin whose binding cannot be reconstructed.
* **Value is not lost.** On-chain withdrawal and the signed-once backup exit are untouched. What is
  lost is off-chain movement: these coins cannot transfer on the laddered lane.
* `SPEC-ROADMAP` WP4's "Zero NULL `aggregate_xonly` rows" gate is **unachievable as written** and
  must be restated, or WP4 will never close.

## The options that remain

| Option | Shape | Cost |
|---|---|---|
| **A. Owner-supplied re-binding** | Add an endpoint where the *current owner* proves control and supplies `user_public_key`; the coordinator computes and records the aggregate. Only the owner can do this, so it is opt-in and per-coin. | An endpoint + an authenticated proof + a client flow. Coins whose owners never act stay unbound forever. |
| **B. Derive from chain** | The coin's tx0 output *is* the aggregate. If the coordinator stores enough to locate tx0, it could read the x-only key off-chain **without** the owner. Needs checking whether the funding outpoint is recorded per sid. | Cheap if the outpoint is stored; this is the option worth investigating first. |
| **C. Accept and document** | State that pre-0009 coins are flat-lane-only for life, and restate the WP4 gate as "zero NULL among post-0009 rows". | A sentence, and 3 637 live coins that never gain the laddered lane. |

### B was checked, and it is also out

The narrow question was whether the coordinator records the funding outpoint or tx0 per statechain
id. It does not — **it stores no chain data of any kind.** Every column of all three relevant tables:

```
statechain_data           id token_id auth_xonly_public_key server_public_key statechain_id
                          enclave_index single_use epoch_deadline sig_budget user_public_key
                          aggregate_xonly
statechain_transfer       id statechain_id new_user_auth_public_key x1 encrypted_transfer_msg
                          created_at updated_at key_updated batch_id batch_time locked locked2
                          cancelled_at claim_started_at sender_auth_xonly_public_key
statechain_signature_data id server_pubnonce challenge tx_n statechain_id created_at
                          partial_sig_issued
```

No txid, no outpoint, no vout, no tx0 hex. The coordinator cannot locate a coin on-chain, so it
cannot read the aggregate off it. That is consistent with the design — the coordinator is
deliberately not a chain observer — but it closes option B.

**So the real choice is A or C**, and it is the owner's to make. A recovers the 3 637 live coins but
only for owners who act; C is one sentence and abandons the laddered lane for all of them. Neither
loses value: on-chain withdrawal and the backup exit are untouched in both.

D8's enumeration is unaffected either way — this is about which coins *can* be laddered, not about
what the coordinator is trusted for.
