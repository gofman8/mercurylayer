# Trust model

> This is the short version. The complete party-by-party matrix — sender, receiver, SE,
> watchtower, Bitcoin indexer, RGB proxy, operators; what each verifies vs trusts, with code and
> test citations, and the numbered list of boundaries that cannot be removed — is
> [TRUST-MODEL.md](../TRUST-MODEL.md). The ladder mechanics referenced below are specified in
> [PROTOCOL.md](../PROTOCOL.md); split children in [CHILDREN.md](../CHILDREN.md).

## One protocol, two coin shapes

Read everything below with this split in mind, because two of the guarantees are enforced by
different machinery on each shape. There is exactly **one protocol** — `claim()` builds a TES-R
ladder for every fresh confirmed root coin, unconditionally — but not every coin is laddered:

- **Laddered** — every plain BTC deposit. The coin is a pre-signed, **un-broadcast** chain of three
  tiers: **trigger** `T` (no timelock, signed once at deposit) → **extension** `X_m` (relative CSV
  `E_m`) → **state** `S_k` (relative CSV `Δ_k`), all v3/TRUC with a P2A anchor.
- **Un-laddered** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
  sweep the sats and destroy the allocation — terminal-freeze, `sdk52`), and a split sub-coin whose
  funding tx is un-broadcast has no on-chain outpoint to root a trigger. (A `single_use` coin is
  likewise skipped.) These keep the signed-once backup tx and move by backup-chain handover over a
  verified exit **branch**. This lane is load-bearing for tokens (`sdk39`, `sdk52`) — current, not
  legacy.

## What you trust, exactly

| Property | Guarantee | Mechanism |
|---|---|---|
| SE cannot steal | cryptographic | 2-of-2 MuSig2: the SE holds one share, blindly, and never the full key |
| You can always exit | cryptographic (+ timeliness) | *laddered*: walk the pre-signed tier chain `T → X_m → S_k`, waiting out each relative timelock (`sdk50`, and `sdk45` shows a keyless third party can drive it). *Un-laddered*: broadcast the pre-signed branch + latest backup. Neither path calls the SE |
| Holding costs nothing | structural | BIP-68 relative timelocks only start counting once the **parent** confirms, and `T` carries no timelock — so nothing matures until someone broadcasts `T`. An idle laddered coin **never ages**: no calendar deadline, 0 vB of on-chain rent, and renewal is off-chain and unbounded (`sdk43`) |
| Current owner wins the exit race | timelock + receiver-verified | *laddered*: replace-by-lower-timelock — each transfer co-signs a fresh state one `δ` **below** the one it replaces, so the new owner's state always matures first, and every superseded state is disclosed and counted by the receiver's census. *Un-laddered*: decrementing absolute locktimes on the backup chain — your backup unlocks before any previous owner's |
| A received payment is final | cryptographic | the claim completes the SE key-share and auth-key rotation, so the receiver co-owns the coin (or child) and the **sender is permanently locked out** (`sdk60`, `sdk17`) |
| No double-spend of off-chain state | SE honesty + receiver-verified | the SE refuses to co-sign a node whose spend budget is exhausted (publicly auditable, `GET /statechain/spend_budget/<id>`; `sdk04`); optional per-coin `single_use` hard rule. On top of that the *receiver* independently proves it — the ladder census on a laddered coin, terminal-ancestor checks on an un-laddered branch — so this is receiver-verified, not merely trusted |
| Token correctness | cryptographic | RGB client-side validation — the SE never sees or vouches for token state |

Spark distributes the "refuses conflicting state" role across n operators with FROST (honest if
≥1 of n is honest). Here the role is one SE. In both systems a fully-colluding operator side can
fresh-sign an old owner's state; in both, the current owner's **lower timelock** plus a timely exit
is the backstop. Nothing about *custody* rests on the SE in either design (`sdk15` pins that trust
floor explicitly).

## Timeliness obligations

There is no "your coin expires" clock. What each shape actually asks of you:

- **Laddered coins: reactive, not calendar.** Nothing in your ladder matures while it sits
  un-broadcast, so you owe nothing while idle. The obligation only starts *if someone broadcasts
  the trigger*: from that moment a defender must walk the tiers within their CSV windows. Your own
  pass is `defend_ladders()` — call it per block from your loop (`sdk51`) — or hand the keyless
  `TesrBundle` to any machine; a third-party tower with no key material defends an offline owner
  against a hostile trigger end-to-end (`sdk45`).
- **Un-laddered coins (RGB carriers, un-broadcast-funding sub-coins): an absolute deadline is
  real.** Materialize or exit before the backup locktime floor. The wallet's background pass
  (`auto_exit`, default-on, margin 288 blocks ≈ 2 days) force-exits plain sub-coins and
  materializes token carriers near their deadlines (`sdk34`), and `export_watch_bundle()` delegates
  the same job — the bundle carries pre-signed exit material and **no key material at all**.
- **Renewal and rollover are off-chain and unbounded.** A laddered coin lives off-chain forever: a
  lower-CSV extension replaces `X_m` horizontally, and a rollover mints a fresh level, neither
  touching the chain (`sdk43`).
- **`refresh` is the re-anchor primitive, not a deadline reset.** It spends one on-chain tx to move
  a coin to a fresh funding outpoint and mint a new ladder (`sdk30`). Routine background refreshing
  is default-**off**: a laddered coin has no floor to approach, and on the un-laddered lane the
  re-anchor cost is folded into `transfer` as an on-demand payment fee.
- **Epoch deadline (optional, opt-in at deposit).** A coin created with `epoch_deadline` stops
  getting new SE co-signatures after that wall-clock time — transact or exit before it. Unilateral
  exit still works after, because it needs no SE.

Availability is the one obligation that cannot be removed — but it is fully delegable, redundant
(multiple towers hold the same pre-signed txs and can never conflict), and never custodial.

## What a receiver verifies

A receiver **verifies, and does not trust**, the sender. Both shapes check the transfer signature
binding the coin to *their* key, that their new share plus the new SE share recombine to the coin's
on-chain aggregate pubkey, and that the funding output is unspent. Beyond that the shapes diverge:

**On a laddered coin — the census** (`verify_bundle` for a whole coin, `verify_child_bundle` for a
split child):

1. The handed-over state carries the **strictly-lowest CSV** in the ladder, and no hidden
   lower-CSV state exists.
2. Every **superseded** state was disclosed and is provably out-raced — each off-chain hop costs
   exactly one co-signature and discloses exactly one superseded state, and the receiver counts
   them (across two hops in `sdk60`, a partial second hop in `sdk17`).
3. The ancestor chain is **key-derived, never name-supplied**: the parent's aggregate is derived
   from the fetched on-chain funding output and must match the SE's record for the claimed id, and
   each intermediate segment's aggregate is derived from the funding output it actually spends. A
   substituted ancestor fails on the key, not on a label.
4. For a split child, the claim completes the key handover — the child's aggregate is invariant
   across the rotation, which is what keeps every pre-signed child tier valid and locks the sender
   out for good. A child is then first-class: payable onward whole or split, off-chain.

`sdk58` runs 11 adversarial child bundles (hidden lower-CSV state, decoy parent, non-terminal
parent, count padding, value spoof, …) and rejects all of them; `sdk46` shows a malicious sender
who bypasses his own client's guard is still rejected by the receiver.

**On an un-laddered sub-coin — the branch:**

1. The **exit branch**: every branch tx is consensus-valid (scripts + signatures verified locally),
   links parent→child, and terminates at an on-chain, unspent, **confirmed** root (a mempool-only
   root is rejected). The branch is also a **tree** — no outpoint consumed twice
   (`reject_non_tree_branch`) — every branch tx is immediately broadcastable, and value is
   conserved at every hop (Σ outputs ≤ Σ inputs, so no hop creates sats).
2. The backup chain: the latest backup pays the receiver, and locktimes decrement by exactly the
   SE's `interval` per hop.
3. **Every structural ancestor the branch consumes is terminal at the SE.** The receiver
   independently queries `GET /statechain/spend_budget/<id>` per ancestor and requires one named
   terminal ancestor per structural *input* across the whole branch (`required_terminal_ancestors`
   / `verify_terminal_parents`, Σ inputs) — so a combine of N carriers forces all N to be named and
   terminal (`sdk31`, `terminal_parents_tests`). This makes double-spend prevention
   receiver-verified: a malicious *sender* cannot double-spend a parent to invalidate the branch.
4. For tokens: the RGB consignment validates off-chain against the same branch, and the balance is
   booked under the consignment's **verified** contract id (`sdk02`, `rgb13`).

The one honest gap on this lane: the terminal-ancestor *ids* are not cryptographically bound to the
branch's outpoints (the SE is blind and cannot attest to the mapping), so the count check defeats
*omission* but not *substitution* by a sender who controls other terminal coins. Settling on-chain
closes it, and the automatic pass does that near the deadline. Laddered coins are structurally
exempt — there is no id to substitute (point 3 above).

## See also

[Exits](exits.md) · [Transfers](transfers.md) · [Tokens](tokens.md) ·
[Lightning](lightning.md) · [TRUST-MODEL.md](../TRUST-MODEL.md) (the full matrix, including the
numbered boundaries B1–B10 and the privacy table).
