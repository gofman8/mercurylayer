# Transfers

> This page is the guided tour. The normative accounts are [PROTOCOL.md](../spec/PROTOCOL.md)
> (the tiers, replace-by-lower-timelock, the in-ladder split, the census),
> [CHILDREN.md](../spec/CHILDREN.md) (first-class received children) and
> [SPEC.md](../spec/SPEC.md) §5–§6 (the requirements). What a payment costs is priced in
> [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md).

## A transfer is a key handover

Nothing moves on chain. The coin's aggregate key `A` and its on-chain funding output `F` are
**invariant** across a transfer: the SE rotates its own share so that
`sender_share + SE_old == receiver_share + SE_new == A`. Every transaction anyone pre-signed under
`A` therefore stays valid — which is exactly what hands the receiver a working exit — while the
sender's share is dead and only receiver + SE can co-sign from that moment on. No confirmation, no
fee, sub-second, fully async.

Three steps, and **the order is load-bearing**:

1. **The sender pre-signs everything first.** `transfer_sender::execute`
   (`clients/libs/rust/src/transfer_sender.rs`) builds the receiver's flat backup
   (`create_backup_transactions` → `create_tx1`) and, on a laddered coin, co-signs the
   receiver-paying state `S'` (`presign_receiver_state`). None of that needs `x1`.
2. **The sender opens the transfer** — `POST /transfer/sender` (`get_new_x1`) — then
   `create_transfer_update_msg` posts the ECIES-encrypted conveyance to the coordinator's mailbox
   through `POST /transfer/update_msg`. Opening arms the **pending-transfer lock**: from here the
   coordinator refuses further co-signatures on that coin.
3. **The receiver claims**, whenever it next comes online: it validates everything from public data,
   then `POST /transfer/receiver` completes the rotation.

Only `t1 = o1 + x1` needs `x1`, which is why the pre-signs can all precede the open. If `get_new_x1`
ran first, the lock would block the sender's own legitimate pre-signs and no transfer would
complete. With the pre-signs first, **no co-signature requested after the open is legitimate**, so
refusing all of them is safe by construction.

*Evidence:* `sdk41` (laddered transfer), `sdk49` (Model-A transfer — the receiver adopts the ladder
and unilaterally exits it, which is what makes the handover self-custodial), `sdk47` (the full
receiver-verification set across a transfer), `sdk01`.

## The message carries a shape tag, not a version

`TransferMsg.protocol_version` selects a message **shape**. There is one protocol.

| tag | shape |
|---|---|
| `0` | branch + backup-chain message — the flat lane |
| `2` | a conveyed TES-R ladder — a whole laddered coin |
| `4` | a split-child bundle carrying the key handover (`SHAPE_CHILD`) |

`ADMISSIBLE_PROTOCOL_VERSIONS = [0, 2, 4]` is an **exact set**, checked by `admissible_shape`
(`clients/libs/rust/src/transfer_receiver.rs`), so an unknown tag is refused *by name* rather than
read as "at least". The child gates — the pre-pay census `prepay_child_census` and the claim path's
`validate_encrypted_message` — each call it and then require exactly `SHAPE_CHILD`.

Tag `0` is still an admissible *receive* shape, and deliberately: a receiver must keep understanding
a message a legacy or pre-SDK wallet emits. What has changed is the *send* side. The two licences
that let an ordinary coin travel flat — `licence_rgb_carrier` (an RGB carrier) and
`licence_funding_not_onchain` (a sub-coin over un-broadcast funding) — are **retired**, along with
their `PermanentLicence` variants (`clients/libs/rust/src/transfer_sender.rs`), because a carrier is
laddered now and the plain split that produced the other is deleted. What still licences a flat
conveyance is narrower, and each is established from *positive* evidence rather than from a recorded
string: a `single_use` terminalized carrier (the coin's own flag), a pre-migration-0009 coin the
coordinator answers about with no aggregate on record (re-proved live, that call), and a wallet
carrying no ladder artefact of any kind and therefore provably never through the SDK's ladder pass.

Residual, and stated in [CHILDREN.md](../spec/CHILDREN.md): the **sender** declares the tag. The
uniffi FFI strips `protocol_version`, `tesr_ladder` and `child_tesr_bundle` on its way through, and
exact-set dispatch is what makes a stripped tag fail **closed** instead of landing silently on the
flat census. What is missing is a floor the *receiver* sets.

## Whole laddered coin: replace-by-lower-timelock

```
F   (on-chain funding, key = owner + SE — the only thing resting on chain)
└─ T      TRIGGER    no timelock, signed once at deposit, never re-signed
   └─ X_m EXTENSION  relative CSV E_m   (renewal replaces it horizontally, off-chain)
      ├─ S_k  STATE  relative CSV Δ_k   ← the state the sender held (now disclosed as superseded)
      └─ S'   STATE  relative CSV strictly lower, pays the RECEIVER   ← co-signed at transfer
```

Each hop costs **exactly one co-signature** and discloses **exactly one superseded state**. Because
`S'` sits below the state it replaces, the new owner's exit matures first and a stale owner who
broadcasts an old state loses the race by construction.

The margin is not "one δ below my own state". `next_rival_state_csv`
(`clients/libs/rust/src/tesr.rs`) takes one δ below the **lowest rival over the current extension's
payload output** — the sender's own live state, every disclosed superseded state, *and* every state
still outstanding on a conveyance the sender made earlier. On mainnet δ = 36 blocks (≈ 6 h). At
`d_floor` the call **refuses**, and the coin must be renewed, rolled over or re-anchored before it
can be handed on.

Nothing here touches the chain and nothing starts ticking: BIP-68 relative locks only begin counting
once the parent confirms, and `T` has no timelock at all.

## What the receiver checks

Claiming re-derives the coin from public data (the R′ set, [PROTOCOL.md](../spec/PROTOCOL.md) §5.11):
`F` on chain and unspent paying `A`; `T` spending it with no timelock; every tier paying the right
public tweak of `A`.

Two checks are worth knowing by name.

**The band binding.** Each tier's *signed* nSequence must be a BIP-68 block relative lock inside the
band its kind allows — `[e_floor, e0]` for an extension, `[d_floor, d0]` for a state, exactly
`SPINE_CSV = 0` for a split tip — and the tier's *declared* `csv` field is bound to that same signed
number by `bind_declared_csv` (`lib/src/transfer/receiver.rs`). A schedule that contradicts the
signatures is refused rather than believed. The band's endpoints are not the sender's to choose
either: `cap_schedule` (`clients/libs/rust/src/tesr.rs`) runs **before** the census on both receive
paths and measures every conveyed `TesrParams` field against the receiver's **own** compiled-in
network preset, refusing by name on the first disagreement. The SE never publishes the tier schedule
and does not need to — `/info/config` serves only `initlock`, `interval`, `batchtimeout` and
`version` (`server/src/endpoints/utils.rs`).

**The census.** `verify_bundle_bound` enforces an *exact equality*:

```
se_num_sigs == flat_backups + Σ conveyed tiers + Σ disclosed superseded tiers
```

Any undisclosed rival state raises the left-hand side and nothing on the right, so it shows up as a
count mismatch and the claim is rejected. Each disclosed superseded tier must be parsed, linked to
the ladder, signature-checked and carry a strictly *higher* CSV than the tier replacing it — a
`.len()`-only count would be paddable.

The count only proves anything if it is the **enclave's**, so it is not taken on the coordinator's
word: `GET /info/statechain/<id>` must carry a `utexo/sig_count/v2` attestation over
`(statechain_id, num_sigs, sig_budget, nonce)`, verified against an enclave attestation identity the
client resolves for itself. `TesrParams::attestation_identity` (`lib/src/tesr.rs`) is that
resolution, in three steps: a compiled-in pin for the network
(`TesrParams::attestation_identity_const`) if one exists and is **not** overridable — a configured
value that disagrees with it is a refusal, not a preference; otherwise a configured
`ClientConfig::attestation_identity`, read from `UTEXO_ATTESTATION_IDENTITY` or the client settings;
otherwise **refuse**, because a client with no key to check the signature against must not degrade
to accepting the key the coordinator serves alongside it.

Two things to say plainly about the shipped state. `attestation_identity_const` pins **regtest's**
identity and returns `None` for mainnet, testnet and signet, where no enclave is provisioned. Regtest
can be pinned precisely because the out-of-band channel is the repository itself — the dev seed is in
the tree, so `TesrParams::REGTEST_ATTESTATION_IDENTITY` is a fact about the source rather than about
whichever server happens to answer, and a unit test re-derives it and fails if either the seed or the
constant moves. On the other three the identity is the configured one, obtained out of band from the
enclave's own `GET /attestation_identity` (`lockbox/src/server.cpp`, over
`enclave::attestation_identity_pubkey`); a mainnet pin is not withheld out of caution but because
there is no mainnet enclave yet, and a *wrong* pin refuses every attestation while inviting the
"fix" of trusting the coordinator's served key — the exact hole the pin exists to close. And an
identity is pinned rather than chain-anchored because a deep split ancestor's funding output is
deliberately un-broadcast, so there is nothing on chain to bind to.

*Evidence:* `sdk46` (the formula against the real SE counter), `sdk47` (R′ across a transfer),
`sdk54` (adversarial `verify_bundle`), `sdk55` (backup-chain adversarial), `sdk70` (verifier
binding), `sdk76` (the ancestor census on a *received* parent that is then split), `sdk56` (a
repeated `sign/second` returns the cached partial and does not advance the count, so a retry cannot
brick the equation).

## The calendar that survives laddering

Laddering deletes the **CSV-side** ageing and nothing else. Alongside the ladder every coin keeps
its **flat backup chain**: one signed-once backup per owner, at absolute locktimes
`L_k = L_0 − k·interval` (INV-5, enforced on both shapes). Mainnet `initlock` is 10,000 blocks and
`interval` is 100 — 100 hops of capacity — from `TesrParams::flat_ladder_params`
(`lib/src/tesr.rs`), which is compiled in per network precisely because a coordinator that could
choose `interval` would be choosing the defence against backup-vector padding.

So a coin that has been received `k` times sits on `min(L_k)`: a real, finite, approaching height
held by its **prior owners**, whatever its ladder is doing. `sdk86` measures both clocks on one coin
across three owners — the ladder fingerprint is byte-identical after 300 idle blocks, while the
absolute locktime is 300 blocks nearer and each whole-coin hop spends another `interval` of it.
Defending that height is [exits](exits.md)' business.

## Paying an arbitrary amount: the in-ladder split

`transfer(address, amount)` never asks you to think about coins.

- **Exact subset.** If some subset of your coins sums to the amount, each is handed over whole.
  This is rare in practice: `min_child_value` is the finest piece the protocol will mint, so an
  arbitrary amount's residue essentially never lands on a subset sum
  ([PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) §1.2).
- **In-ladder split** — the common case, and therefore the real payment path.

A split `SP` is a **spine state tier** spending `X_m.out[0]` at `SPINE_CSV = 0`. It is a
*descendant* of the trigger, never a rival for `F`, and strictly below the state it replaces on that
output. It carries one resting output per child plus the P2A anchor; `establish_child` then hangs
each child's own extension and state tiers off `SP.out[j]`. The whole thing is one SE-co-signed,
**un-broadcast** transaction: zero on-chain footprint, and a payment that never completes simply
rolls back.

Descending from the trigger is the load-bearing detail. A plain split of a laddered coin would
compete with a past owner's retained, no-timelock trigger for `F`; the in-ladder split has nothing
to race. Value is conserved exactly, and `tier_out_total` is the one place the identity lives:
`Σ children == X_m.out[0] − committed_fee_for_outputs(n, rate) − P2A_VALUE`, where the fee grows by
`P2TR_OUT_VBYTES = 43` vB per extra child so the tier still relays standalone.

*Evidence:* `sdk58` (accept, plus 12 adversarial cases all rejected), `sdk59` (end-to-end split
payment, receiver completes the handover), `sdk81` (the split
survives a hard process kill taken between the parent's terminalization and the child co-signs).

## Two legs, two floors, and both are functions of the fee rate

A child is not a bare output — it funds its own rungs before it can clear dust — so a split is
refused **up front**, before the parent's spend budget is consumed. Up-front matters:
`establish_child` runs *after* `SP` is co-signed, so admitting an undersized child would terminalize
the parent and *then* fail, stranding it to unilateral-exit-only.

The legs are floored independently, by lane. `split_output_floors`
(`clients/libs/rust-sdk/src/transfer.rs`) is the one place either number is derived; it returns a
`SplitFloors { piece, change, lane }` carrying the lane with the numbers, and what the change leg's
shape is on that lane is `change_leg_role`'s answer (`clients/libs/rust/src/tesr.rs`), never a
constant chosen at the call site:

| leg | shape | floor | at the shipped rate |
|---|---|---|---|
| **piece** | two rungs (its own extension + state) | `min_child_value = 2·(committed_fee + 240) + 330` | **1,560 sat** |
| **change** on the plain-root, spine-batch and coloured lanes | one cap rung over `SP.out[K]` | `min_spine_tip_value = committed_fee + 240 + 330` | **945 sat** |
| **change** on the plain-child lane | two rungs | `min_child_value` | 1,560 sat |

Both are `mercurylib::tesr` functions taking the rate as an **argument**, and
`TesrParams::mainnet()` ships `committed_fee_rate = 3.0` sat/vB with `TIER_VBYTES = 125`, so
`committed_fee = 375`. Quoting one of these numbers without its rate is quoting a rate, not a floor.

## One planner, three parent shapes

`transfer()` and `quote_transfer()` call the same `plan_payment`
(`clients/libs/rust-sdk/src/transfer.rs`), so a quote saying `fundable: true` followed by an
executor refusal is not expressible. `parent_shape` resolves each candidate and the split routes on
it:

| `ParentShape` | route | what it carves |
|---|---|---|
| `Root` | `in_ladder_pay` | `SP` over `X_m.out[0]` |
| `Child` | `child_in_ladder_pay` | `CSP` over `ext_child.out[0]` |
| `SpineTip` | `spine_batch_pay` | the next batch `SP_{i+1}` over `SP_i.out[K]` |

**There is no fourth row, and its absence is the point.** `ParentShape::Unladdered` used to sit here,
routing to `split_coin` — a plain N+1-output split of the funding outpoint. Both are deleted. That
route spent `F` directly, which is the very output a prior owner's retained, un-timelocked trigger
also spends, so the prior owner could void the split and destroy the payee's sub-coin and the payee
had no way to see the exposure ([B1]). The hazard is now closed **by construction**: every remaining
route carves out of the ladder, so nothing a payment builds competes for `F`. `parent_shape` itself
became a *refusing* function to match — a coin with no root bundle, no child bundle and no tip is a
coin to repair, not a shape to route, and it says so and names `ladder_skip_reason` as the place the
wallet already recorded why. `parent_shape_opt` is the probe form, for the one caller
(`has_exit_material`) where absence is data rather than a fault.

`parent_shape` probes the **spine tip first**, and that has a consequence worth internalising:
`in_ladder_pay` gives its change leg `ChangeLeg::LastIsTip`, so after your first partial payment
your change *is* a spine tip, and the second and every later payment out of it take the
`spine_batch_pay` arm. The `SpineTip` arm was added because a tip carries neither a `tesr-` row nor a
`ctesr-` row, so before it existed a tip fell through three consecutive absences to `Unladdered` —
simultaneously the cheaper cost model, the lower floor and the route to a plain split of a coin that
*is* laddered, three wrong answers from one missing arm. With the fall-through now a refusal rather
than a lane, the tip arm's job is to answer correctly rather than to prevent a loss.

`transfer_many` (sats) and `batch_transfer_tokens` (assets) carve N recipient pieces plus change in
a single split, and `transfer_many` dispatches on exactly the same three shapes — `in_ladder_pay_many`,
`child_in_ladder_pay_many` and `spine_batch_pay_many`. `ManyRoute::PlainSplit` is gone with them, so
that match is exhaustive with no tail. Width is far cheaper than
paying N people in sequence. A batch is bounded by the derived-slot allowance —
`DERIVED_SLOTS_PER_STATECHAIN = 64` slots, i.e. `MAX_BATCH_RECIPIENTS = 63` recipients, refused by
name through `SdkError::BatchTooManyRecipients` — and is **not atomic** across recipients.
*Evidence:* `sdk69` (the multi-child in-ladder route).

## The piece you receive is a first-class coin

A received in-ladder split child is not an exit-only claim. Its claim runs `verify_child_bundle`
(the parent `F` read on chain through the conveyed branch, then the census) and **then completes the
key handover**: the SE rotates its share so `A_child` is invariant, every pre-signed child tier
stays valid, and the sender is permanently locked out. From there you pay it onward entirely
off-chain:

- **whole** — `child_retransfer` (`clients/libs/rust/src/tesr.rs`) co-signs a fresh state over
  `ext_child.out[0]` at a strictly lower CSV paying the next recipient and discloses the state it
  replaces. It spends zero sats, adds zero depth and never touches `ancestors`;
- **partially** — `child_in_ladder_pay` / `child_in_ladder_pay_many` split the child at its own
  level, terminalizing the child and handing that terminalized segment to the grandchildren as an
  ancestor.

The rule is uniform at every level: **the node being split is terminalized; the piece being conveyed
is not.**

The receiver's protection generalises to N hops with the same three terms, summed across the
conveyed ancestor chain — and `CHILD_V2_BASELINE = 0`, because a derived child slot never ran
`create_tx1` and therefore has no flat backup of its own. *Evidence:* `sdk60`
(alice → bob → carol, whole re-transfer, `F` unspent throughout, carol exits to her own key),
`sdk17` (multi-hop with a partial second hop), `sdk84` (a leaf that has spent its transfer budget
gets it back for zero on-chain bytes and no depth).

A child is deliberately **not** terminalized. The one exception is a Lightning-latched piece, which
sits unclaimed past the lock's window and is terminalized instead — see
[lightning](lightning.md) and [LIGHTNING.md](../spec/LIGHTNING.md).

## The pending-transfer lock, and its one hour

Between the receiver's census check and its handover completion, the still-owner sender would
otherwise be able to co-sign a lower-CSV rival: `server/src/endpoints/sign.rs` refuses on single-use,
spend budget and epoch, none of which covers an in-flight transfer. The lock closes it.
`has_open_transfer` (`server/src/database/transfer_sender.rs`) is checked in `sign_first` /
`sign_second`, and `has_open_transfer_to_other_auth` splices a key-inequality into the **same**
window query so an open transfer cannot be re-addressed to a different recipient.

It is a *releasable* lock, not a monotonic budget, so it does not fight the budget clamp in
`set_sig_budget` (`server/src/database/deposit.rs`). It is also **temporary**, and its window is
`OPEN_TRANSFER_WINDOW_SQL`: the batched branch honours a configurable timeout, while the non-batch
branch — every ordinary payment — is a hard-coded `updated_at > NOW() - INTERVAL '1 hour'` with no
setting that shortens it.

That hour is measured, not assumed. `sdk91` drives a payer that bypasses its own client and POSTs
`/sign/first` directly: **inside** the window the coordinator answers `409 Conflict`, *"coin has an
open transfer"*; once the row is aged past an hour the same request returns `200` with a
`server_pubnonce`. So the window is the **only server-side gate on that path**. `sdk90` measures the
two independent gates that stop an *honest* client first — the wallet's own coin lookup and the
sender-side outstanding-conveyance refusal — but both are the payer's own software, so it never
reaches the server-side gate.

The practical reading: terminality and the completed handover are **permanent** lockouts; the lock
is a **temporary** one. A non-terminal child conveyed without a handover, or held past the window
without completing it, can be out-raced by a rival the still-owner sender co-signs. The four moves —
leaving the child's budget open, `verify_child_bundle` not requiring child terminality, the
conveyance carrying the handover material, and the receiver completing it — are one indivisible unit.

## Two ordering rules the lanes must obey

- **All sender pre-signs happen before `get_new_x1`** (above).
- **A durable arm-down precedes every superseding co-sign.** The watchtower's child loop in
  `defend_ladders_inner` (`clients/libs/rust-sdk/src/wallet.rs`) filters on the coin's *durable*
  status alone and has no supersession check. So any lane that co-signs a superseding state must
  move the coin out of `CONFIRMED` durably **before** the co-sign and before any conveyance —
  otherwise your own tower may broadcast a state your recipients' chains supersede, voiding their
  pieces. This binds `execute_ex`, `child_retransfer`, `cosign_colored_child_retransfer` and the
  `in_ladder_pay` / `child_in_ladder_pay` / `child_in_ladder_pay_many` lanes. `sdk80` measures the
  window with markers the wallet does not write and asserts zero admitted samples; `sdk79` covers
  the coloured lane; the CI guard `ci-guards/tests/deny_armed_tower_during_conveyance.rs` pins the
  ordering in source.

## Replays and balances

- **A receiver refuses a conveyance of a `statechain_id` it has already adopted, by name**
  (REQ-45) — "adopted" meaning a row this wallet still holds and can spend. It deliberately excludes
  `TRANSFERRED` (sending a coin away and getting it back later is legitimate) and `IN_TRANSFER` (the
  sender's own row during a self-transfer).
- **A wallet's balance is a function of distinct statechain ids, not of rows** (REQ-46). A second
  live row under one id is one coin counted twice, which turns an upstream lapse into spendable
  value that is not there.

## Depth: what a receiver will adopt

A conveyed child is admitted by `check_exit_headroom_with_margin`
(`lib/src/transfer/receiver.rs`): the payee's exit walk must fit inside the epoch it inherits **with**
`exit_slack_margin` of headroom, not merely by a bare latency comparison. The build side runs the
same rule (`enforce_split_depth_cap_shaped`, `clients/libs/rust/src/tesr.rs`), because a builder
using the looser rule mints children no receiver can adopt — after terminalizing the parent, so each
one is a stranded piece.

The cap is **derived, not a literal** (`max_split_depth`): it moves with the network profile. On
mainnet it is depth **8**, 19 transactions to walk; on regtest depth 54, 111 transactions. The window
is measured against the parent's **own conveyed backup chain**, never a freshly-read epoch — a
wallet holding a conveyed child has never held the root, so a local lookup finds nothing. `sdk82`
drives the gate against a live SE.

## Maintenance folded into a transfer

Long-lived coins need upkeep, and all of the off-chain kinds run unattended inside `transfer()`:

- **Renewal** replaces the extension horizontally with a lower-CSV one and resets the state ladder.
  `mercuryrustlib::tesr::renew` is exactly two `cosign_tier` calls over the ordinary
  `/sign/first` + `/sign/second` pair. Zero on-chain bytes. Older extensions become consensus-dead:
  the new one strictly undercuts them in the race for `T.out[0]`.
- **Rollover** at `m_max = 15` converts the current state into a 1-in-1-out self-split whose child
  output hosts fresh extension and state tiers — a fresh hop budget for +1 depth level and zero
  on-chain bytes.
- **Auto-refresh before spend.** `transfer()` calls `auto_refresh_before_spend` first, re-anchoring
  any coin near its **flat** ladder floor before coin selection, so a payment never hands the
  receiver a coin with no calendar left.

`sdk43` drives renew → rollover → renew past epoch exhaustion and then exits through the whole deep
chain with `F` untouched. `sdk44` pins the schedule arithmetic.

## Token transfers

`transfer_tokens(asset, address, amount)` is a **coloured** off-chain split — the piece carries
exactly `amount` of the asset, change keeps the rest — plus the same handover. Which coloured split
it is follows from `SdkConfig::colored_ladder`, which now **reads the enclave pin** rather than
stating a bool (`TesrParams::attestation_identity_const(network).is_some()`,
`clients/libs/rust-sdk/src/config.rs`): where an identity is pinned the carrier holds a coloured
ladder and the payment is a coloured **in-ladder** split conveying a coloured child (shape `4`). The
RGB consignment travels in the transfer message and the receiving wallet validates it client-side.

Where none is — mainnet, testnet and signet, because no enclave is provisioned there yet — the
carrier is not laddered, and the legacy flat coloured split is what would build. Note what it no
longer *clears*, because it is the same retirement as above seen from the token side: that split's
piece is a sub-coin over un-broadcast funding, so its conveyance goes through
`assert_flat_conveyance_is_legitimate`, and the two licences that covered exactly this pair — an RGB
carrier, and a sub-coin whose `F` is not on chain — are the two that were retired. That function
holds a single `Ok`, reachable only from a proven licence, so the send refuses **before** the
conveyance rather than degrading to a censusless lane. Those networks are waiting on an enclave, not
on a flag.

When no single carrier holds the amount, `colored_combine_transfer` spends N carriers of one asset
into an exact piece plus change in a single SE-co-signed colored combine (N inputs → 2 outputs);
every combined carrier is made terminal first, and the receiver requires **all N** to be terminal.
`batch_transfer_tokens` is the fan-out form. See [tokens](tokens.md).

## What a payment costs

Per payment on the leaf lane — which is the lane essentially every payee is on:

| | block space | against ~154 vB on chain |
|---|---:|---|
| spent onward off-chain | **0 vB** | this is the product |
| swept and settled | ~105 vB | 1.47× better — and this is the cap without the discharge round |
| shipped default | 418 vB | 2.7× worse |
| walked out unilaterally | 250 – 2,719 vB | worse than on-chain |

The walked range is the leaf's own chain, `293·d + 375` vB over `3 + 2d` sequential transactions,
topping out at the mainnet depth cap of 8. Both the sweep and the discharge round that would change
these numbers are **design, not built** — see [exits](exits.md) and
[PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md).

Read next: [exits](exits.md) for what happens when the SE is gone, [tokens](tokens.md) for the
coloured lane, [lightning](lightning.md) for the HODL latch.
