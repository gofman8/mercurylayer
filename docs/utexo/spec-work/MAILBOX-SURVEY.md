# Mailbox availability and censorship — survey (D39.4)

**2026-08-13.** The one adversary surface with no round of analysis behind it, surveyed before §7 is
written because §7's credibility IS the adversary model.

**Scope.** Withholding, deletion, reordering, duplication and injection of the coordinator-held
encrypted transfer messages, and the read/write endpoints around them. **Adversary:** the coordinator
acting alone, plus the coordinator-in-collusion cases where they change the verdict.

---

## The surface, as it actually is

**Write.** `POST /transfer/sender` sets `statechain_transfer.encrypted_transfer_msg`
(`server/src/database/transfer_sender.rs:311`). One row per transfer, keyed by the recipient's
`new_user_auth_public_key`.

**Read.** `GET /transfer/get_msg_addr/<new_auth_key>` →

```sql
SELECT encrypted_transfer_msg FROM statechain_transfer
WHERE new_user_auth_public_key = $1
  AND encrypted_transfer_msg IS NOT NULL
  AND cancelled_at IS NULL
ORDER BY updated_at ASC
```

The read is **non-destructive** — a claimed message keeps being served — and both the ordering column
and the filter are coordinator-written state.

**Clear.** `apply_cancel` NULLs `encrypted_transfer_msg` and `x1` together
(`transfer_cancel.rs:142`); the destroyed `x1` is what makes a cancel irreversible rather than a flag.

**Consume.** The client fetches per unique `coin.auth_pubkey`, then for each ciphertext finds the
first `INITIALISED` coin with that auth key; if there is none it MINTS a slot by cloning an existing
coin's keys (`duplicate_coin_to_initialized_state`) and processes into that. Validation failure is a
`continue`, never a stop.

---

## Findings

| | class | adversary | verdict |
|---|---|---|---|
| **M-1** | withholding | coordinator alone | denial only |
| **M-2** | deletion | coordinator alone | denial only; the LOSS arm needs the sender |
| **M-3** | reordering | coordinator alone | **no ordering dependence** — binding is by key, not by position |
| **M-4** | duplication / replay | coordinator alone | refused — but **incidentally**, see below |
| **M-5** | cross-addressed injection | coordinator alone | fail-closed (ECIES) |
| **M-6** | serve-then-renege around an irreversible leg | coordinator alone | **real loss, LN lane only** |

### M-1 / M-2 — withholding and deletion are denial

Neither moves value. Withholding is recoverable (the read is non-destructive, so a later poll gets
the message); deletion is not, but the coin stays the sender's. The escalation worth stating is that
withholding **past the coin's epoch expiry** makes the conveyance permanently unacceptable, because
admission requires the exit walk to fit inside the epoch the payee inherits — so the coordinator can
convert a delay into a permanent failure without ever touching a key.

**The attribution correction stands, and the survey confirms its mechanism.** The loss arm — "the
sender re-conveys to a second payee" — is not coordinator-alone: the cancel that frees the coin
requires a single-use, endpoint-bound Schnorr signature under the SENDER's auth key, which the
coordinator cannot forge (`lib/src/transfer/cancel.rs:47-58`), and the re-conveyance is the sender's
act with the sender as beneficiary. **Coordinator + sender — the same adversary as CO-1.**

### M-3 — reordering: no dependence found

`ORDER BY updated_at ASC` is coordinator-controlled, and the consume loop's slot selection *is*
order-sensitive. It does not matter, because **the slot is not what binds a message to a coin — the
keys are**: the ciphertext is ECIES to the auth key, and `verify_transfer_signature`'s preimage is
`(tx0_txid, tx0_vout_le, new_user_pubkey)`. A message that reaches the wrong slot fails validation and
is skipped; the loop continues to the next. Reordering therefore costs at most a wasted pass.

Worth recording explicitly because the obvious-looking hazard is real and the mitigation is not
obvious: the `else` branch mints a slot by **cloning the keys** of an existing coin with the same auth
key, so slot identity carries no authority at all.

### M-4 — replay is refused, but by consequence rather than by intent

**This is the survey's finding.**

A duplicated ciphertext takes the same path as an honest re-serve: the first copy consumes the
`INITIALISED` slot, the second falls to the mint-a-slot branch with identical keys — so every check
that binds to the coin passes. What refuses it is `validate_tx0_output_pubkey`: the message carries
the **sender's** `user_public_key`, the completed handover **rotates the SE's share**, and
`S + E′ ≠ tx0.out.key`. Observed live twice in one sdk78 run (`Validation error: Invalid tx0 output
pubkey`).

Two things follow.

1. **The child lane has an explicit guard for this and the root lane does not.** The child lane
   refuses by name — *"split child … already adopted"* (`transfer_receiver.rs:1200`, seen live in
   sdk32) — with a comment claiming it "mirrors the flat-transfer pattern where a re-received coin
   fails validation". The flat lane has no such check; it is protected by key rotation. The comment
   describes an outcome as though it were a rule.

2. **If that incidental protection ever lapsed, the failure would be silent.**
   `compute_balance_excluding` sums coin ROWS, skipping only `duplicate_index != 0` — it never dedupes
   by `statechain_id`. Two rows for one coin would double-count into `available_sats`, and a merchant
   crediting on `get_balance` would over-credit.

**Recommendation (design, not merely code):** the root lane must refuse an already-adopted
`statechain_id` **by name**, exactly as the child lane does, and the balance must be a function of
distinct statechain ids rather than of rows. A protection that holds only because an unrelated
subsystem rotates a key is not a protection the specification can state.

### M-5 — cross-addressed injection is fail-closed

Serving a ciphertext addressed to a different auth key fails ECIES decryption under
`coin.auth_privkey`. No further check is needed and none is relied on.

### M-6 — the one arm that is not denial

The SSP's Lightning lane runs **census → pay → claim**. The census reads the conveyed message; the
payment is an irreversible Lightning leg; the claim comes after. A coordinator that serves a valid
message to the census and then withholds, alters or refuses at claim time leaves the payer out the
full invoice amount, **acting alone**.

The failure text for this already exists in the code, and it fired on the live stack during this
survey: *"paid the Lightning invoice for batch … but claimed 0 transfers — the latched coin was not
received; investigate before retrying"* (sdk21). Today's cause is unrelated (the derived-slot voucher
defect), but it is the exact shape, and it demonstrates that the window is real rather than
theoretical.

The shape is not specific to the mailbox — the coordinator can equally refuse `/transfer/receiver` —
so §7 should carry it as a general statement: **on the Lightning lane, coordinator liveness between
the pre-pay census and the completed claim is a payment-SAFETY dependency, not a liveness one.** That
is a stronger claim than the trust model currently makes anywhere, and it is the one sentence this
survey adds to §7 that was not going to be written otherwise.

---

## What the survey did NOT establish

* No empirical probe was built for M-4 against a **deliberately duplicating** coordinator. The verdict
  rests on the honest re-serve taking the identical code path (it does — the loop's in-memory
  `temp_coins` already reflects the first copy before the second is read) plus the two observed
  refusals. A driven test would be worth having alongside the explicit guard recommended above.
* Mailbox **availability under load** (as opposed to adversarial withholding) was not measured.
* The JS clients' consume loops were not surveyed; D39.3 already records that they are behind the
  Rust SDK on the census, and the same gap may extend here.
