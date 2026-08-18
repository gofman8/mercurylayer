# Trust model

> This is the short version. The complete party-by-party matrix — sender, receiver, SE, watchtower,
> Bitcoin indexer, RGB proxy, operators; what each verifies vs trusts, with code and test citations,
> and the numbered boundaries B1–B11 that no protocol change removes — is
> [TRUST-MODEL.md](../spec/TRUST-MODEL.md). The ladder mechanics referenced below are specified in
> [PROTOCOL.md](../spec/PROTOCOL.md); split children in [CHILDREN.md](../spec/CHILDREN.md).

The one-line version: **nothing here asks you to trust a counterparty.** What remains is one
well-known statechain assumption about the SE, your own view of the Bitcoin chain, and someone being
awake before deadlines — and the third is delegable without custody. One gap does not fit that
summary and is stated in full below: between the moment a payer conveys a coin and the moment the
payee claims it, the server side is held by a **wall-clock one-hour timer**, not by ownership.

## One protocol, one coin shape — and the material that survives it

Read everything below with two distinctions in mind, because several guarantees are enforced by
different machinery on each side of them.

**Every coin is laddered.** `claim()` builds a TES-R ladder for every fresh confirmed root coin: a
pre-signed, **un-broadcast** chain of three tiers — **trigger** `T` (no timelock, signed once at
deposit) → **extension** `X_m` (relative CSV `E_m`) → **state** `S_k` (relative CSV `Δ_k`), all
v3/TRUC with a P2A anchor. An RGB **carrier** is no longer the standing exception: it may never be
given a *plain* ladder (a plain tier spend is sats-only and would sweep the carrier out from under
its allocation — terminal-freeze), so it is given a **coloured** one, every tier carrying its own
valid RGB state transition (`build_colored_tier`, `renew_colored_ladder`, `colored_reanchor`).
`SdkConfig::colored_ladder` (`clients/libs/rust-sdk/src/config.rs`) selects it by **reading the
enclave pin** — `TesrParams::attestation_identity_const(network).is_some()` — because a ladder whose
terminality cannot be verified against a pinned identity is not worth building. Regtest is pinned and
ships on; **mainnet is off solely because no mainnet enclave is provisioned yet**, and pinning a real
identity flips it with no other change ([TRUST-MODEL.md](../spec/TRUST-MODEL.md) B11,
[SPEC.md](../spec/SPEC.md) §0.4 rows V-1 and V-6). Where a carrier does stay flat it is named
(`LadderSkipReason::RgbCarrier`), and a `single_use` terminalized carrier is likewise skipped.

**Un-broadcast funding is permanent, and is not a second shape.** A split sub-coin — a received
child, a spine tip — is funded by an output of the un-broadcast `SP`, so it has no on-chain outpoint
to root a **trigger** of its own. Colouring a tier cannot change that; nothing can. Its exit material
is the chain reaching back to its parent's confirmed `F`, and for a coloured sub-coin the retained
signed-once backup plus the verified exit **branch** are what settle the allocation on chain
(`sdk39`). That property is load-bearing for tokens and is where 0 vB of idle rent comes from.

**What is gone is the plain un-laddered lane.** `ParentShape::Unladdered`, `split_coin`, the plain
off-chain split and `ManyRoute::PlainSplit` are deleted, together with the two flat-lane licences
that let an ordinary coin travel without a ladder — `licence_rgb_carrier` and
`licence_funding_not_onchain`, and their `PermanentLicence` variants
(`clients/libs/rust/src/transfer_sender.rs`). What still licences a flat conveyance is narrower, and
each is established from *positive* evidence rather than from a recorded string: a `single_use`
terminalized carrier, a pre-migration-0009 coin the coordinator answers about with no aggregate on
record, and a wallet carrying no ladder artefact at all.

## What you trust, exactly

| Property | Guarantee | Mechanism |
|---|---|---|
| SE cannot steal | cryptographic | 2-of-2 MuSig2: the SE holds one share, blindly, and never the full key |
| You can always exit | cryptographic (+ timeliness) | *laddered*: walk the pre-signed tier chain `T → X_m → S_k`, waiting out each relative timelock (`sdk50`; `sdk45` shows a keyless third party can drive it). *flat material* (a legacy coin, or a carrier awaiting an enclave): broadcast the pre-signed branch + latest backup. Neither path calls the SE |
| The ladder costs nothing to hold | structural | BIP-68 relative timelocks only start counting once the **parent** confirms, and `T` carries no timelock — so no tier matures until someone broadcasts `T`. The tiers never age: 0 vB of on-chain rent, and renewal is off-chain and unbounded (`sdk43`). This is a statement about the tiers, **not** about the coin's root — see the two clocks below |
| Current owner wins the exit race | timelock + receiver-verified | *laddered*: replace-by-lower-timelock — each transfer co-signs a fresh state one `δ` **below** the one it replaces, so the new owner's state matures first, and every superseded state is disclosed and counted by the receiver's census. *on the flat material every coin also retains*: decrementing absolute locktimes on the backup chain — your backup unlocks before any previous owner's |
| A received payment is final | cryptographic | the claim completes the SE key-share and auth-key rotation, so the receiver co-owns the coin (or child) and the **sender is permanently locked out** (`sdk60`, `sdk17`) |
| No double-spend of off-chain state | receiver-verified over an attested count | the enclave checks a per-coin `sig_budget` against its lifetime `sig_count` before consuming a secnonce, and terminality is publicly readable (`GET /statechain/spend_budget/<id>`, `sdk04`). On top of that the *receiver* proves it independently — the ladder census on the tier chain, terminal-ancestor checks on a conveyed branch — against a count the **enclave signs**, not one the coordinator asserts |
| Token correctness | cryptographic | RGB client-side validation — the SE never sees or vouches for token state |

There is **no enclave single-active-state refusal**, and the specification does not claim one. The
enclave cannot know which state is current and *must* co-sign rivals, because that is what a renewal
is. What it enforces is narrower and real: **one signature per secnonce** (the sealed secnonce is
loaded and consumed in the same row-locked transaction; a second partial signature finds it NULL —
`lockbox/src/server.cpp`), and the budget check above. The second layer over the consensus race is
the receiver's census, not an SE promise.

Spark distributes the "refuses conflicting state" role across n operators with FROST (safe if ≥1 of
n is honest). Here the role is one SE. In both systems a fully-colluding operator side can fresh-sign
an old owner's state; in both, the current owner's **lower timelock** plus a timely exit is the
backstop. Nothing about *custody* rests on the SE in either design (`sdk15` pins that trust floor
explicitly).

## The SE is blind — and the operator runs both halves

The SE receives a session commitment, never the transaction: no amounts, no outpoints, no
destinations. Blindness covers *content*, not *traffic* — it still learns statechain ids, auth
pubkeys, signature counts, flags and transfer timing.

Two facts belong next to that, and both are in [TRUST-MODEL.md §3](../spec/TRUST-MODEL.md):

- **The coordinator (API + Postgres) and the lockbox (key shares) are run by the same operator.**
  The separation is a software boundary inside one administrative domain, not two parties. Any
  argument of the form "the coordinator cannot do X because the enclave would have to agree" is an
  argument about software, not about incentives.
- **The SE has no trustworthy chain access.** It runs in an operator-controlled container on an
  operator-controlled network, so "the SE checked the chain" reduces to "the operator says so." No
  design here rests on the SE verifying an on-chain fact.

What *is* attested is narrow and load-bearing: the enclave signs the numbers the census rests on —
a `utexo/sig_count/v2` signature over the statechain id, `num_sigs`, the spend budget and a
client-chosen nonce — and the client verifies it against a **pinned** enclave identity
(`TesrParams::attestation_identity`, `lib/src/tesr.rs`: compiled-in pin → configured value →
**refuse**, never a fallback to a served key). There is **no enclave-residency attestation**: "the
share lives in an enclave" is an operational claim you trust, not one you verify. Authenticity of
the count, not residency of the share.

## The conveyance window: one hour, measured

Between conveyance and claim the payer still holds a valid credential for the coin — the
`signed_statechain_id` written at deposit, not rotated until the claim completes. On the server side,
exactly one thing stops a payer from opening a fresh co-signing session over a coin they already paid
away: the open-transfer gate (`has_open_transfer` / `OPEN_TRANSFER_WINDOW_SQL`,
`server/src/database/transfer_sender.rs`), whose non-batch branch is a hard-coded
`updated_at > NOW() - INTERVAL '1 hour'`.

`sdk91` measures it on a live stack. A payer who skips their own client and POSTs `/sign/first`
directly with their own genuine credential gets **HTTP 409 while the window is open**, and **HTTP 200
with a `server_pubnonce` once the row is older than an hour**. Nothing is forged in that probe. So
the gate is real and fires correctly while the window is open, and once the hour lapses the
coordinator issues the session — on wall-clock time, whether or not the payee has claimed.

`sdk90` measures the other side: an honest client is stopped by **two independent local gates**
before it ever reaches the coordinator — the wallet's own coin lookup (`IN_TRANSFER`, a lookup in its
own SQLite) and the sender-side `refuse_outstanding_conveyance`
(`clients/libs/rust/src/tesr.rs`, called from the transfer sender, `in_ladder_split` and `renew`).
Both are the **payer's own software**, and a payer who wants to cheat does not run them.

**Do not read this as more than it is.** A `sign/first` session is the first link of a chain, not a
completed theft: `sign/second` and a broadcast race against the payee's strictly-lower-CSV state
still stand between it and money moving, and in the measured run the payee claimed his coin intact.
The remaining links are untested in either direction. The specified fix — an owner latch keyed off
the money itself, so co-signing binds to ownership rather than to elapsed time — is **design, not
built**; `EXPECT_LATCH=1` is already wired into both tests and converts the recording into an
assertion the day it ships.

## Timeliness obligations

A laddered coin has **two clocks, not zero**. Getting this backwards is the most common way to
misread the design.

- **Reactive (the tiers).** Nothing in your ladder matures while it sits un-broadcast, so an
  un-triggered coin costs nothing to watch and carries no calendar date. The obligation starts *if
  someone broadcasts the trigger*: from that moment a defender must walk the tiers within their CSV
  windows. Your own pass is `defend_ladders()`, wired unconditionally into `start_background` and
  gated to one pass per new block; or hand the keyless `TesrBundle` to any machine — a third-party
  tower with no key material defends an offline owner against a hostile trigger end to end (`sdk45`,
  `sdk51`).
- **Absolute (the root).** The ladder is not the coin's only pre-signed material. The **flat backup
  chain** is retained alongside it, carrying absolute locktimes `L_k = L_0 − k·interval`, and its
  lowest value `min(L_k)` is a real approaching height held by the coin's **prior owners**. When it
  passes, an ancestor's matured rung can spend `F`. `deadline_safety_due`
  (`clients/libs/rust-sdk/src/refresh.rs`) is the only scheduled defence of that clock: at
  `auto_refresh_margin_blocks` = 144 it re-anchors cooperatively first and, if that is refused,
  **severs from `F`** by broadcasting the already-co-signed trigger — `T` carries no timelock, so it
  beats every retained *timelocked* rung by being valid first. `sdk86` measures the height advancing
  and each hop spending `interval` of it.
- **Sub-coins over un-broadcast funding, and any carrier still travelling flat: the absolute
  deadline is the whole story.** They inherit the root's epoch and hold no calendar of their own, so
  materialize or exit before the backup locktime floor. `auto_exit_due` (default on) force-exits
  plain sub-coins and materializes token carriers near their deadlines (`sdk34`).
- **Renewal and rollover are off-chain and unbounded.** A lower-CSV extension replaces `X_m`
  horizontally, and a rollover mints a fresh level — neither touches the chain (`sdk43`).
- **`refresh` is the re-anchor primitive, not a deadline reset.** It spends one on-chain tx to move a
  coin to a fresh funding outpoint and mint a new ladder (`sdk30`). Routine *background* refreshing is
  default-**off** (`background_auto_refresh = false`): that flag governs maintenance, never safety —
  the safety passes above run either way.
- **Epoch deadline (optional, opt-in at deposit).** A coin created with `epoch_deadline` stops getting
  new SE co-signatures after that wall-clock time (HTTP 410 `Gone`, `server/src/endpoints/sign.rs`).
  Unilateral exit still works after, because it needs no SE.

Margins are **derived, not chosen**: `auto_exit_margin_blocks` is
`auto_exit_margin_blocks_for(k_max, interval, d) = k_max·interval + tesr_exit_txs(d)·144`
(`clients/libs/rust-sdk/src/config.rs`) — **2,120 blocks on mainnet** (14·100 + 5·144) and
**860 on regtest** (14·10 + 5·144). The second term is one confirmation window per *sequential*
transaction of the exit walk, because each must confirm before the next tier's relative lock starts
counting.

## What delegation does and does not cover

Availability is the one obligation that cannot be removed. It is delegable, redundant (multiple
towers hold the same pre-signed txs and can never conflict) and never custodial — but it has two
stated limits, and both are protocol facts rather than implementation gaps:

- **A keyless tower does not cover the root clock.** A laddered entry is exported with
  `deadline_block: u32::MAX`, which disables the height predicate by construction, so a delegate
  watches only the *event* of `F` being spent. Re-anchoring needs the owner's keys.
- **A keyless tower cannot fee-bump.** It can broadcast the pre-signed tiers at their committed fee,
  but a CPFP child spending the P2A anchor needs a funding input it does not hold and a signature it
  cannot make. Above the relay floor the defence falls back to the **owner being online**, or to an
  operator running the optional funded-tower variant. `SdkConfig::fee_bump` ships as `None` on both
  presets, so out of the box no wallet bumps anything.

Everything a tower broadcasts is already fully signed and pays only the owner, whichever material
it is holding.
`sdk45` serializes the bundle a user would hand a third party and asserts it contains no key material
at all, then has the keyless tower defend an offline owner end to end, and shows a second independent
tower is an idempotent re-broadcast. The worst a malicious or buggy tower can do is broadcast
*early* — which settles your coins on-chain **to you** — or not act, which is the same risk as running
no tower.

When a pass cannot tell, it says so rather than concluding "nothing is due":
`ExitCostEstimate::exit_deadline_blind` distinguishes "this coin genuinely has no deadline" from
"I could not compute one", `deadline_is_unknown()` is the predicate, and a blind pass emits
`WalletEvent::WatchtowerBlind`, retains a `WatchtowerFault`, and is refused from an exported bundle.

## What a receiver verifies

A receiver **verifies, and does not trust**, the sender. Both message shapes check the transfer signature
binding the coin to *their* key, that their new share plus the new SE share recombine to the coin's
on-chain aggregate pubkey, and that the funding output is unspent. Beyond that the shapes diverge.

**On a laddered coin — the census** (`verify_bundle` for a whole coin, `verify_child_bundle` for a
split child, both `clients/libs/rust/src/tesr.rs`):

1. The handed-over state carries the **strictly-lowest CSV**, and no hidden lower-CSV state exists.
   Every tier's *signed* nSequence must lie inside the band its kind allows, and the tier's
   *declared* CSV is bound to that same signed number — a schedule that contradicts the signatures is
   refused rather than believed.
2. Every **superseded** state was disclosed and is provably out-raced. `verify_bundle_bound` enforces
   the exact equality `se_num_sigs == flat_backups + tiers + superseded`, so a hidden extra co-signed
   state shows up as a count mismatch. The right-hand side is the **enclave's** attested lifetime
   count, refused outright if unattested, half-stated, replayed or signed by anything but the pinned
   identity.
3. The yardstick is the receiver's own. `cap_schedule` runs *before* the census on both receive paths
   and measures every conveyed `TesrParams` field by field against the receiver's own network preset,
   refusing by name on the first disagreement. A schedule that travels on an artifact is data, never
   a yardstick.
4. The ancestor chain is **key-derived, never name-supplied**: the parent's aggregate is derived from
   the fetched on-chain funding output and must match the SE's record for the claimed id, and each
   intermediate segment's aggregate is derived from the funding output it actually spends. A
   substituted ancestor fails on the key, not on a label. Terminality of those ancestors is taken from
   the enclave-signed payload (`attested_terminal`), keeping the coordinator's own answer only as a
   cross-check that refuses on disagreement.
5. For a split child, the claim completes the key handover — the child's aggregate is invariant across
   the rotation, which is what keeps every pre-signed child tier valid and locks the sender out for
   good. A child is then first-class: payable onward whole (`child_retransfer`) or split again
   (`child_in_ladder_pay`), off-chain.

`sdk58` accepts one real split child and then rejects a battery of tampered ones — hidden lower-CSV
state, decoy parent, non-terminal parent, count padding, value spoof, a genuinely co-signed rival
child state — each pinned to the **named** error it targets, so a rejection for an unrelated reason
(a parse, address or network failure) cannot report a safety it never observed. `sdk46` shows a
malicious sender who bypasses
his own client's guard is still rejected by the receiver; `sdk54` and `sdk55` are the whole-coin and
backup-chain adversarial suites.

**On a conveyed branch — the flat lane** (`clients/libs/rust/src/transfer_receiver.rs`), which a
legacy coin and a carrier still travelling flat both take:

1. The **exit branch**: every branch tx is consensus-valid (scripts + signatures verified locally),
   links parent→child, and terminates at an on-chain, unspent, **confirmed** root — a mempool-only
   root is rejected. The branch is also a **tree** — no outpoint consumed twice
   (`reject_non_tree_branch`) — every branch tx is immediately broadcastable, and value is conserved
   at every hop.
2. The backup chain: the latest backup pays the receiver, and locktimes decrement by exactly the
   SE's `interval` per hop. That `interval` is **not** taken from the coordinator: it comes from
   `TesrParams::flat_ladder_params` compiled into the client (mainnet, testnet and signet
   10,000 / 100; regtest 1,000 / 10 — 100 hops of ladder capacity either way), because the decrement
   *is* the defence against a padded backup vector and a coordinator that supplied it would define
   the defence.
3. **Every structural ancestor the branch consumes is terminal at the SE**, one named ancestor per
   structural *input* across the whole branch (`required_terminal_ancestors` /
   `verify_terminal_parents`, Σ inputs) — so a combine of N carriers forces all N to be named and
   terminal (`sdk31`). This makes double-spend prevention receiver-verified: a malicious *sender*
   cannot double-spend a parent to invalidate the branch.
4. For tokens: the RGB consignment validates off-chain against the same branch, and the balance is
   booked under the consignment's **verified** contract id (`sdk02`, `rgb13`).

The one honest gap on this lane: the terminal-ancestor *ids* are not cryptographically bound to the
branch's outpoints (the SE is blind and cannot attest to the mapping), so the count check defeats
*omission* but not *substitution* by a sender who controls other terminal coins. Settling on-chain
closes it, and the automatic pass does that near the deadline. The tier chain is structurally
exempt — there is no id to substitute (point 4 above) — so retiring the plain un-laddered lane
shrinks the population this gap applies to rather than closing the gap itself.

## What you can always do alone

- **Exit.** Every exit path is pre-signed and SE-free. A refusing SE freezes only the *cooperative*
  paths; freeze is not seizure (`sdk50` walks the whole tier chain with no SE call).
- **Withdraw a transfer you opened but never conveyed.** That half is unilateral. The conveyed half
  deliberately is not, and it is the one item on this list that stops at your own signature — because
  the party on the other side may already be holding claimable material. Re-sending such a coin does
  **not** quietly redirect it: the open-transfer lock refuses the second conveyance by name.
  Cancellation is its own operation, authorized from coordinator-observable state by
  `decide_transfer_cancel` (`lib/src/transfer/cancel.rs`): the **sender alone** while the mailbox
  message was never posted; **sender and recipient together** once it is posted and unclaimed
  (`preview_cancel_consent` → `cancel_consent` → `cancel_with_consent`,
  `clients/libs/rust/src/transfer_sender.rs`), with the consent bound to *that* transfer's conveyed
  bytes so a consent given for a superseded instance cannot withdraw its replacement
  (`transfer_consent_digest`, `CancelDecision::RecipientConsentStale`); and **never** once the claim
  rotated the key (`AlreadyClaimed`) or while the coin is a batched Lightning latch (`Batched`,
  released by the latch expiry instead). `tm01` drives all three steps against the live coordinator:
  the silent overwrite refused, the recipient minting consent from its own key, and the redirect
  succeeding only after the consented cancellation releases the lock.
- **Audit.** Terminality is publicly readable per node, and the count the census rests on is
  enclave-signed and pin-verified.

Nothing in this protocol ever pays the operator by timeout. Missed liveness is never
confiscation-by-design.

## The boundaries that remain

They are numbered B1–B11 in [TRUST-MODEL.md §7](../spec/TRUST-MODEL.md); the short list is: SE share
deletion and SE-plus-old-owner collusion (the irreducible statechain unit — and note it gives **no
race head start**, since the collusive spend is un-timelocked and so is your own trigger);
ancestor-id substitution on the flat lane only; indexer honesty; deadline liveness on both
clocks; the onboarding window before the first backup is co-signed; un-conveyed ancestor locktimes;
loss of local state (`wallet.db` and the RGB data directory — the mnemonic alone is not a backup);
payment atomicity; one live wallet instance per database; split/combine commit ordering; and the
pinned enclave identity, whose residual is that a malicious enclave can attest anything.

## See also

[Exits](exits.md) · [Transfers](transfers.md) · [Tokens](tokens.md) · [Lightning](lightning.md) ·
[Invalidation](invalidation.md) · [TRUST-MODEL.md](../spec/TRUST-MODEL.md) (the full matrix,
including B1–B11 and the privacy table).
