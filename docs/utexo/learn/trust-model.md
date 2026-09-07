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

**Every coin is laddered — at first sight, not at confirmation.** `claim()` builds a TES-R ladder for
every un-laddered root coin it sees, admitting `IN_MEMPOOL`, `UNCONFIRMED` and `CONFIRMED` alike: a
pre-signed, **un-broadcast** chain of three tiers — **trigger** `T` (no timelock, signed once at the
first sighting of the funding transaction, in place of the flat `tx1` that used to be signed there)
→ **extension** `X_m` (relative CSV `E_m`) → **state** `S_k` (relative CSV `Δ_k`), all
v3/TRUC with a P2A anchor. Nothing exit-related waits for a confirmation: the trigger needs only the
funding outpoint, its value and the aggregate key, and the coordinator never gates a co-sign on the
chain. An RGB **carrier** is no longer the standing exception: it may never be
given a *plain* ladder (a plain tier spend is sats-only and would sweep the carrier out from under
its allocation — terminal-freeze), so it is given a **coloured** one, every tier carrying its own
valid RGB state transition (`build_colored_tier`, `renew_colored_ladder`, `colored_reanchor`).
`SdkConfig::colored_ladder` (`clients/libs/rust-sdk/src/config.rs`) selects it by **reading the
enclave pin** — `TesrParams::attestation_identity_const(network).is_some()` — because a ladder whose
terminality cannot be verified against a pinned identity is not worth building. Regtest is pinned and
ships on; **mainnet and every public testnet are off solely because no enclave is provisioned there
yet** (`attestation_identity_const` returns `None` for `bitcoin`/`mainnet`, `testnet`, `testnet3`,
`testnet4` and `signet`), and pinning a real identity flips it with no other change
([TRUST-MODEL.md](../spec/TRUST-MODEL.md) B11, [SPEC.md](../spec/SPEC.md) §0.4 rows V-1 and V-6).
Where a carrier gets no ladder it is named (`LadderSkipReason::RgbCarrier`), and a `single_use`
terminalized carrier is likewise skipped.

**And the pin gates more than carriers — this is the sentence to get right.** The SDK's establish
pass calls `get_statechain_info` for every coin it is about to ladder, and that call resolves the
attestation identity pin → configured value → **refuse**. On a network with no pin and no configured
identity it fails, the pass records `LadderSkipReason::AttestationIdentityUnpinned`, and it ladders
**nothing** — plain coins included. With no flat backup underneath any more, such a deposit is booked
but has *no exit material*: it cannot be conveyed and it cannot be unilaterally exited, and
**cooperative withdrawal is the only route out** (`withdraw` no longer reads backup rows at all, so
that route does work). The flat backup used to supply that unilateral exit without any attestation;
it no longer exists. This is a not-yet-deployable state rather than a live regression — mainnet has
no enclave provisioned in the first place. The lane distinction is load-bearing: `mercuryrustlib`'s
own `update_coins` (`LadderAtSight::Plain`) ladders inside the deposit pass via `tesr::establish_auto`,
which calls no attested endpoint and therefore needs no pin; it is the SDK `claim()` pass
(`LadderAtSight::Defer`), the one a wallet user takes, that stops.

**Un-broadcast funding is permanent, and is not a second shape.** A split sub-coin — a received
child, a spine tip — is funded by an output of the un-broadcast `SP`, so it has no on-chain outpoint
to root a **trigger** of its own. Colouring a tier cannot change that; nothing can. Its exit material
is the chain reaching back to its parent's confirmed `F`, and for a **coloured** sub-coin that chain
is itself the RGB witness: walking `T → X_m → SP → ext_child → state_child` moves the allocation to
the owner's own key (`sdk39`, re-derived onto this lane, pending run). *(Before 2026-09-06 a coloured
sub-coin settled instead through a retained signed-once backup plus a verified exit branch; neither
exists now.)* That property is load-bearing for tokens and is where 0 vB of idle rent comes from.

**What is gone is the un-laddered lane — all of it (2026-09-06).** `ParentShape::Unladdered`,
`split_coin`, the plain off-chain split and `ManyRoute::PlainSplit` are deleted, and so is the flat
conveyance lane with its licence classifier (`assert_flat_conveyance_is_legitimate` and the
`PermanentLicence` verdicts no longer exist as code; `is_legitimate_flat_reason` answers `false` for
every recorded reason, `clients/libs/rust/src/transfer_sender.rs`). Nothing licences a flat
conveyance any more, because there is no flat backup to convey: `transfer_sender::execute` refuses a
coin with no ladder row by name, and a coin in that state is a fault to repair (`ladder_skip_reason`),
not a shape to route. The token half went with it: the legacy coloured split/combine over a carrier's
funding output `F` is refused outright by `refuse_legacy_colored_split_lane`, on both settings of
`colored_ladder` and **before any SE co-sign**, and the migration hatch that used to let an
un-colourable carrier through is closed with the lane — a child carved there would have been exited
by the flat backup chain, and there is none.

## What you trust, exactly

| Property | Guarantee | Mechanism |
|---|---|---|
| SE cannot steal | cryptographic | 2-of-2 MuSig2: the SE holds one share, blindly, and never the full key |
| You can always exit — **once the coin is laddered** | cryptographic (+ timeliness) | *laddered*: walk the pre-signed tier chain `T → X_m → S_k`, waiting out each relative timelock (`sdk50`; `sdk45` shows a keyless third party can drive it). The walk calls no SE. But the promise is now conditional on the ladder existing, and that is the honest statement of it: a coin with **no ladder** has no exit material at all (2026-09-06) — there is no flat backup to fall back to, and `unilateral_exit` refuses it by name rather than broadcasting anything. Four populations sit there: a carrier the coloured builder could not take, a plain ladder over a carrier, a sub-floor carrier no coloured ladder can ever be built for, and — on a network with no pinned enclave identity — **every SDK deposit**. For a plain coin the only route out is then cooperative withdrawal, i.e. an SE that answers; for a carrier not even that (`withdraw` hard-errors on a carrier), leaving only `materialise_carrier` on a legacy branch, which settles the ALLOCATION and does not exit the coin |
| The ladder costs nothing to hold | structural | BIP-68 relative timelocks only start counting once the **parent** confirms, and `T` carries no timelock — so no tier matures until someone broadcasts `T`. The tiers never age: 0 vB of on-chain rent, and renewal is off-chain and unbounded (`sdk43`). Since 2026-09-06 that is a statement about the whole coin, not only its tiers: no flat backup chain sits beside the ladder, so nothing on a coin matures on its own (INV-27, unconditional) |
| Current owner wins the exit race | timelock + receiver-verified | *laddered*: replace-by-lower-timelock — each transfer co-signs a fresh state one `δ` **below** the one it replaces, so the new owner's state matures first, and every superseded state is disclosed and counted by the receiver's census. *(The clause that stood here — decrementing absolute locktimes on a retained backup chain — is RETIRED 2026-09-06: no coin carries a flat backup.)* |
| A received payment is final | cryptographic | the claim completes the SE key-share and auth-key rotation, so the receiver co-owns the coin (or child) and the **sender is permanently locked out** (`sdk60`, `sdk17`) |
| No double-spend of off-chain state | receiver-verified over an attested count | the enclave checks a per-coin `sig_budget` against its lifetime `sig_count` before consuming a secnonce, and terminality is publicly readable (`GET /statechain/spend_budget/<id>`, `sdk04`). On top of that the *receiver* proves it independently — the ladder census on the tier chain, plus key-derived ancestor checks reaching back to the on-chain `F` — against a count the **enclave signs**, not one the coordinator asserts. *(The terminal-ancestor check on a conveyed **branch** went with the branch lane on 2026-09-06.)* |
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

A laddered coin has **exactly one clock, and it is reactive** (since 2026-09-06 — it used to have
two). Getting this wrong in either direction is the most common way to misread the design.

- **Reactive (the tiers).** Nothing in your ladder matures while it sits un-broadcast, so an
  un-triggered coin costs nothing to watch and carries no calendar date. The obligation starts *if
  someone broadcasts the trigger*: from that moment a defender must walk the tiers within their CSV
  windows. Your own pass is `defend_ladders()`, wired unconditionally into `start_background` and
  gated to one pass per new block; or hand the keyless `TesrBundle` to any machine — a third-party
  tower with no key material defends an offline owner against a hostile trigger end to end (`sdk45`,
  `sdk51`).
- **Absolute (the root) — RETIRED 2026-09-06.** *(Was: a flat backup chain retained alongside the
  ladder, with absolute locktimes `L_k = L_0 − k·interval` and a lowest value `min(L_k)` held by the
  coin's prior owners, defended by `deadline_safety_due` and measured by `sdk86`.)* No coin carries a
  flat backup — none at deposit (`create_tx1` is deleted), none at any hop — so there is no
  `min(L_k)`, no ancestor's matured rung, and `coin.locktime` is `None` for life. `deadline_safety_due`
  (`clients/libs/rust-sdk/src/refresh.rs`) still runs and has no laddered subject: its due-predicate
  reads `coin.locktime`. `sdk86` is re-derived to assert no calendar on a received coin, pending run.
- **In-ladder children and spine tips inherit no calendar either.** Their tiers hang off the
  un-broadcast `SP`, and the parent has no flat backup for them to inherit a deadline from
  (`CHILD_V2_BASELINE = 0`); their whole exposure is the parent's trigger being broadcast — an event,
  never a height — and `defend_ladders()` covers them from the block the deposit is first seen in.
  `auto_exit_due` (default on) has a legacy subject only, `branch-` rows from before the rule; its
  leaf near-deadline loop is deleted. *(`sdk34`, which materialised a carrier before a deadline, is
  re-derived to the event-driven defence, pending run.)*
- **Renewal and rollover are off-chain and unbounded.** A lower-CSV extension replaces `X_m`
  horizontally, and a rollover mints a fresh level — neither touches the chain (`sdk43`).
- **`refresh` is the re-anchor primitive, not a deadline reset.** It spends one on-chain tx to move a
  coin to a fresh funding outpoint and mint a new ladder (`sdk30`). Routine *background* refreshing is
  default-**off** (`background_auto_refresh = false`): that flag governs maintenance, never safety —
  the safety passes above run either way.
- **Epoch deadline (optional, opt-in at deposit).** A coin created with `epoch_deadline` stops getting
  new SE co-signatures after that wall-clock time (HTTP 410 `Gone`, `server/src/endpoints/sign.rs`).
  Unilateral exit still works after, because it needs no SE.

Margins are **derived, not chosen** — and the derived one now measures nothing on any coin the code
can mint. `auto_exit_margin_blocks` is still
`auto_exit_margin_blocks_for(k_max, interval, d) = k_max·interval + tesr_exit_txs(d)·144`
(`clients/libs/rust-sdk/src/config.rs`) — **2,120 blocks on mainnet** and **860 on regtest** — but its
`k_max·interval` term was the ancestor-locktime gap of the retired flat chain, and the pass it sizes
has no laddered subject. The margin the design does need is the CSV edge itself: one confirmation
window per *sequential* transaction of the exit walk, because each must confirm before the next
tier's relative lock starts counting.

## What delegation does and does not cover

Availability is the one obligation that cannot be removed. It is delegable, redundant (multiple
towers hold the same pre-signed txs and can never conflict) and never custodial — but it has two
stated limits, and both are protocol facts rather than implementation gaps:

- **A keyless tower covers the whole duty, because the duty is one event.** Every entry — root,
  adopted child, spine tip — is exported with `deadline_block: u32::MAX`, which disables the height
  predicate by construction, and a trigger on `F`: a delegate watches the *event* of `F` being spent,
  and since 2026-09-06 there is no root clock left outside that coverage. The height-driven export
  arm is deleted with the clock it read, and one consequence is worth stating: a legacy coin whose
  only material is a `branch-` row is **omitted from the bundle** rather than exported with its
  branch — `flat_only_coins` reports it, and materialising it stays the owner's own business. What a
  keyless tower still cannot do is re-anchor; that needs the owner's keys.
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
   the exact equality `se_num_sigs == tiers + superseded` (flat term zero by construction — no coin
   carries a flat backup, and a conveyed one is refused before the count), so a hidden extra co-signed
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
conveyed-bundle adversarial suites (sdk55 re-derived — a conveyed flat backup is refused before the
census runs — pending run).

**On a conveyed branch — the flat lane — RETIRED 2026-09-06.** No conveyance carries branch material
or a backup chain any more: the shape is not in `ADMISSIBLE_PROTOCOL_VERSIONS = [2, 4]`, and
`refuse_branch_material` / `verify_flat_backup_lane` (`clients/libs/rust/src/tesr.rs`) refuse the
material by name on both acceptance paths. The four checks below are what that lane used to verify,
kept as history:

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
   `verify_terminal_parents` — both symbols now **DELETED**, surviving only inside doc comments, so a
   reader who greps for them finds nothing; Σ inputs) — so a combine of N carriers forced all N to be named and
   terminal (`sdk31`). This makes double-spend prevention receiver-verified: a malicious *sender*
   cannot double-spend a parent to invalidate the branch.
4. For tokens: the RGB consignment validates off-chain against the same branch, and the balance is
   booked under the consignment's **verified** contract id (`sdk02`, `rgb13`).

The one honest gap that lane had — terminal-ancestor *ids* not cryptographically bound to the
branch's outpoints, so the count check defeated *omission* but not *substitution* — is RETIRED with
it (TRUST-MODEL B2): the tier chain never had an id to substitute, and no conveyance carries branch
material now, so the population the gap applied to is empty.

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

They are numbered B1–B12 in [TRUST-MODEL.md §7](../spec/TRUST-MODEL.md); the short list is: SE share
deletion and SE-plus-old-owner collusion (the irreducible statechain unit — and note it gives **no
race head start**, since the collusive spend is un-timelocked and so is your own trigger);
ancestor-id substitution (B2 — RETIRED with the branch lane); indexer honesty; deadline liveness on
the one reactive clock (B4); the onboarding window before the ladder is co-signed at first sight;
un-conveyed ancestor locktimes (B6 — RETIRED: there are none); a coin with no ladder has no exit and
no second lane (B12, added 2026-09-06 — and on a network with no pinned enclave identity that is
*every* SDK deposit, which is why B11 now gates receiving and holding as well as attesting; the one
population inside it with no remedy at all is a plain ladder over a carrier, since `colored_reanchor`
refuses a plain ladder and `refresh` would destroy the allocation);
loss of local state (`wallet.db` and the RGB data directory — the mnemonic alone is not a backup);
payment atomicity; one live wallet instance per database; split/combine commit ordering; and the
pinned enclave identity, whose residual is that a malicious enclave can attest anything.

## Status of the evidence cited here

The unit and CI-guard suites are green and the E2E crate compiles, but **no E2E flow has been re-run
against the regtest stack since the flat backup was removed on 2026-09-06**. Every `sdk*` / `tm*` /
`tb*` flow named above is evidence *pending a run*, and the measurements attributed to individual
flows (`sdk90`/`sdk91`'s HTTP codes, for instance) are the ones recorded before that change. Several
flows were re-derived in the same commit precisely because the property they used to measure no
longer exists (`sdk34`, `sdk39`, `sdk55`, `sdk82`, `sdk86`, `sdk87`, `sdk88`); those are pending a
first run in their new form.

## See also

[Exits](exits.md) · [Transfers](transfers.md) · [Tokens](tokens.md) · [Lightning](lightning.md) ·
[Invalidation](invalidation.md) · [TRUST-MODEL.md](../spec/TRUST-MODEL.md) (the full matrix,
including B1–B11 and the privacy table).
