# Old-state invalidation and UTXO granularity — design and comparison

> Short comparison page. For the full explainer — lifecycle walkthroughs, over-time behaviour,
> failure scenarios, UX and FAQ — see [invalidation-deep-dive.md](invalidation-deep-dive.md). The
> normative sources are [PROTOCOL.md](../spec/PROTOCOL.md) (the ladder, races, exit costs),
> [SPEC.md](../spec/SPEC.md) (REQ / INV / ERR), [CHILDREN.md](../spec/CHILDREN.md) and
> [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) (every per-payment cost).

How do you stop a previous owner — or a malicious current owner — from using old off-chain state?
Every L2 answers differently. This page compares the designs we reviewed (Spark, Ark and Second's
implementation, SuperScalar, vanilla Mercury) and states ours: **replace-by-lower-timelock at the
consensus level, over a relative-timelock (CSV) exit ladder, with the receiver's exact-equality
census as the independent second layer.**

## One protocol, two coin shapes

Read this first — it decides which mechanic below applies to a given coin.

There is **one protocol**. `claim()` establishes a TES-R exit ladder — **T**rigger → **E**xtension →
**S**tate, all relative CSV, all pre-signed and **un-broadcast** — for every fresh confirmed **root**
coin, unconditionally. There is no protocol-version field and no escape hatch. But not every coin is
laddered, by design:

- **Laddered** — every plain BTC deposit. Old state is invalidated by **replace-by-lower-timelock at
  the consensus level**, and because relative locks do not tick until their parent confirms, the
  ladder's **tiers never age**: no CSV-side expiry, **0 vB of idle rent**.
- **Un-laddered** — an RGB **carrier** is never given a *plain* ladder (a plain tier spend would
  sweep the sats and destroy the allocation — the terminal-freeze rule of
  [PROTOCOL.md §5.10](../spec/PROTOCOL.md), pinned by `sdk52`), and a **split sub-coin whose funding
  output is un-broadcast** cannot root a trigger. These keep the **signed-once, absolute-nLockTime
  backup chain** and transfer by backup-chain handover.

The un-laddered shape is load-bearing for RGB assets. Decrementing absolute locktimes, root deadlines
and "materialize before the deadline" are all real mechanics — but the second and third belong to
that shape. A coloured ladder that would collapse the two shapes into one exists in code
(`build_colored_tier`, `renew_colored_ladder`, `colored_reanchor`), and
`SdkConfig::colored_ladder` ships **`false`**, so the default configuration is the one described
here.

**One correction that matters more than any other on this page.** Laddering removes the *CSV-side*
ageing and nothing else. Every coin that has been **received** also retains its **flat backup chain**,
whose absolute locktimes decrement by `interval` per hop, so it sits on a real approaching height
`min(L_k)` held by its prior owners. `sdk86` measures exactly that: the tip advances toward the height
and each hop spends `interval` of it. "An idle coin never ages" is true of the tiers and false of the
root.

## The designs

| System | Invalidation mechanism | Lifetime / renewal | Failure mode |
|---|---|---|---|
| **Spark** | Relative decrementing timelocks per transfer + operator key-deletion (honest 1-of-n); split nodes are zero-timelock, spent once by operator policy | Off-chain re-sign with the operator group — churn, needs the group | Full operator collusion can sign old state; the timelock race decides |
| **Ark / Second** | VTXOs expire at round end; old state dies by **expiry**; the server co-signs each round's tree | Refresh each round — mandatory participation | Miss the exit window → funds sweep to the server; liveness-critical |
| **SuperScalar** | Decker-Wattenhofer decrementing nSequence (bounded update counter) + laddered timeout trees + operator reclaim | Ladder epochs; a limited number of in-place updates | Update counter exhausts; the dying period lets the LSP claim the UTXO |
| **Mercury (vanilla)** | Absolute decrementing nLockTime backups: the current owner's backup unlocks first | The coin ages on the calendar; an on-chain re-anchor is required to survive | Old owner + SE collusion signs anything; the ladder only orders honest broadcasts |
| **Ours — laddered** | **Relative-CSV ladder (TES-R)**: a transfer co-signs a fresh state one δ *lower* than the one it replaces, so the current owner's state matures first; renewal replaces the whole extension horizontally at a lower CSV, making every older extension **unconfirmable** — **plus** the receiver's exact-equality census over the enclave's attested signature count | **Unbounded and off-chain**: lower-CSV extension renewal (576 hops per depth level), then off-chain self-split rollover at epoch exhaustion. The tiers never age | SE collusion with an old owner — the irreducible statechain trust unit, and it buys **no race head start**, since the collusive spend of `F` is un-timelocked and so is the owner's own trigger (`sdk15`). Otherwise a *race*, but one that cannot start until a **public on-chain trigger** gives ≥144 blocks (~1 day) of notice. The retained `min(L_k)` root height is a separate, real clock |
| **Ours — un-laddered** | Mercury's absolute nLockTime ladder (as above) **+ SE terminal-spend budget per structural node + optional single-use + optional epoch deadline** | A fresh `initlock` ladder per sub-coin — depth does NOT consume lifetime; the root deadline is real and must be beaten by materialization | Old owner + SE collusion; plus a real clawback window if nobody materializes before the deadline |

**Why relative locks at all.** Absolute timelocks age while un-broadcast, so the defence has to be
renewed on the calendar: ~112 vB per coin per epoch is **~5,840 vB per coin-year**, which is ~11% of
all Bitcoin block space at 1M coins and physically impossible at 10M. Relative (BIP-112) locks on
un-broadcast transactions **do not tick until a parent confirms**, so the clock starts on *attack*
rather than on deposit. That single substitution is what deletes the CSV-side rent. Full arithmetic:
[PROTOCOL.md §2](../spec/PROTOCOL.md).

**What it does not delete.** It does not delete `min(L_k)`, and it does not make the ladder free at
the *payment* layer — see [Exit economics](#exit-economics-the-honest-ledger), which is the section
to read before quoting any ratio.

## Our model, precisely

### Laddered coins (every plain deposit)

The funding UTXO **F** is the only thing on-chain. Above it sits a pre-signed, un-broadcast tree:

```
F  (on-chain)
└─ T   TRIGGER    — v3/TRUC + P2A, NO timelock, signed ONCE at deposit, never re-signed
   └─ X_m  EXTENSION  — relative CSV E_m = 720 − m·36; renewal replaces it horizontally
      └─ S_k  STATE   — relative CSV Δ_k = 1440 − k·36; decrements once per transfer
```

Those are `TesrParams::mainnet()` in `lib/src/tesr.rs` verbatim — `d0` 1440 / `delta` 36 / `d_floor`
144, `e0` 720 / `delta_e` 36 / `e_floor` 144, `m_max` 15, `committed_fee_rate` 3.0 sat/vB — and
testnet and signet run the same schedule. Only regtest keeps a test-scale preset (24/6/6, 12/3/3,
`m_max` 2) so a full lifecycle fits in a test's mining budget. `sdk44` drives a whole lifecycle off
that schedule alone.

1. **A transfer invalidates the old state at consensus.** The new owner's state carries a CSV one
   δ = 36 blocks (~6 h) **lower** than the state it replaces, so it always matures first
   (replace-by-lower-timelock, Decker-Wattenhofer at one dedicated tier). The superseded state is
   **disclosed** to the receiver and counted: `verify_bundle_bound`
   (`clients/libs/rust/src/tesr.rs`) enforces the exact equality
   `se_num_sigs == flat_backups + tiers + superseded`, so a hidden extra co-signed state shows up as
   a count mismatch. Evidence: `sdk40` PART 2 (a stale ladder dies outright at consensus once its
   prevout is gone), `sdk41` (after a transfer the receiver co-signs and exits a full ladder over the
   same funding outpoint while the sender is locked out), `sdk46` / `sdk47` (the census against the
   SE's real counter, rejecting a hidden extra signature).
2. **The count is the enclave's, not the coordinator's.** A census against a number the adversary
   writes proves nothing, so `/info/statechain` must carry a `utexo/sig_count/v2` attestation over
   the statechain id, `num_sigs`, the spend budget and a client-chosen nonce, verified against a
   **pinned** enclave identity — `TesrParams::attestation_identity` resolves compiled-in pin →
   configured value → **refuse**, never a fallback to a served key. An unattested, half-stated or
   replayed answer is refused rather than recorded.
3. **Renewal invalidates a whole epoch, off-chain.** When the state CSV would fall to its floor, the
   SDK co-signs a fresh extension `X_{m+1}` at a strictly lower CSV. It undercuts every older
   extension in the race for `T.out[0]`, so every pre-renewal state now hangs on a parent that can
   never confirm. **Zero on-chain bytes.** At `m_max` the coin rolls over off-chain into a fresh level
   with a fresh 576-hop budget (36 hops per epoch × 16 epochs). `sdk43` drives renew → rollover →
   renew past exhaustion with the funding outpoint untouched throughout.
4. **No tier matures until someone broadcasts T.** T has no timelock, and BIP-112 relative locks only
   start counting once the parent confirms. So an idle ladder — and an entire idle split DAG — never
   ages on the CSV side. `sdk40` PART 1 shows real consensus enforcing it: an extension is rejected
   before `E_m` confirmations of T, a state before `Δ_k` of X.
5. **But the coin still has a root clock.** `min(L_k)` over the retained flat backup chain is an
   absolute height held by prior owners; when it passes, an ancestor's matured rung can spend `F`.
   `deadline_safety_due` (`clients/libs/rust-sdk/src/refresh.rs`) is the only scheduled defence: at
   `auto_refresh_margin_blocks` = 144 it re-anchors cooperatively first and, if refused, **severs
   from `F`** by broadcasting the already-co-signed trigger — which carries no timelock and so beats
   every retained *timelocked* rung by being valid first. A defence the adversary can decline by not
   co-signing is not a defence, hence the fallback.
6. **The honest defence is a walk, not a race to broadcast one tx.** A unilateral exit broadcasts the
   tiers in order, waiting out each relative timelock (`sdk50`; the keyless tower does the same walk
   in `sdk45` / `sdk51`). If someone hostile broadcasts T first, the owner answers with a
   **cooperative de-trigger** — a fresh spend of `T.out[0]` at no relative timelock, one tier-shaped
   v3 transaction of **125 vB** (`TIER_VBYTES`), confirmed unopposed inside the ≥144-block window
   during which no adversary tier is valid. `UtexoWallet::detrigger_to_owner` drives it and `sdk89`
   proves it end to end: the value lands at an address the **owner** named, and the griefer's
   pre-signed extension is then refused by the node with `bad-txns-inputs-missingorspent`. Otherwise
   the tower simply broadcasts the strictly-lowest-CSV current state and wins by ≥36 blocks per tier
   (`sdk51`, against a real hostile trigger).
   **Two limits, stated:** the de-trigger has **no restoration half** — it does not spend into a
   fresh `F′` and does not rebuild `T′/X′_0/S′_0`, so getting back off-chain afterwards is a fresh
   deposit; and griefing is **not economically losing for the attacker**, because both transactions
   pay out of the *coin* at their committed rate, costing the coin ~2 × (committed fee + 240-sat
   anchor) while costing the attacker nothing.
7. **`refresh` is the re-anchor primitive, not a deadline reset.** One on-chain transaction moves the
   coin to a fresh funding outpoint and mints a new ladder at depth 0 (`sdk30` covers the user-paid
   and operator-sponsored fee models). It buys a fresh epoch and resets depth; it does not buy
   CSV-side lifetime, because there is none to buy.

### Un-laddered coins (RGB carriers, split sub-coins over un-broadcast funding)

Mercury-native: the first backup unlocks at `tip + initlock`; every transfer hands the new owner a
backup unlocking `interval` earlier, so the current owner always wins an honest exit race. Each
sub-coin gets a **fresh** `tip + initlock` ladder at creation — splitting does not spend the
children's lifetime, unlike a tree that shares one decrementing budget.

The pair is compiled into the client, not served by the SE:
`TesrParams::flat_ladder_params` (`lib/src/tesr.rs`) gives **10,000 / 100** on mainnet, testnet and
signet and **1,000 / 10** on regtest — 100 hops of ladder capacity either way. It has to come from
somewhere neither the sender nor the coordinator chooses, because the per-hop decrement *is* the
defence against a padded backup vector: `/info/config` still publishes both, but only as a
cross-check the client refuses to proceed past on mismatch.

Because these locktimes are absolute, this shape **does** keep a root deadline: a received carrier
owes one materialization — broadcast the coloured branch only, never the sats-sweeping backup — before
an ancestor's stale backup matures. `auto_exit_due` (default on) does it automatically, using the
deposit-anchored `exit_deadline_block` from `estimate_exit_cost`. Evidence: `sdk34` (a watchtower
materializes a carrier before its deadline), `sdk32` (the residual clawback window if nobody does),
`sdk39` (depth-2 coloured branch exit, allocation preserved).

The margin is **derived, not chosen**:
`auto_exit_margin_blocks_for(k_max, interval, d) = k_max·interval + tesr_exit_txs(d)·144`
(`clients/libs/rust-sdk/src/config.rs`) — **2,120 blocks on mainnet**, **860 on regtest**. The second
term is one confirmation window per *sequential* transaction of the exit walk, not a single window for
the whole thing.

And when the pass cannot compute a deadline it says so rather than concluding "nothing is due":
`ExitCostEstimate::exit_deadline_blind` separates "this coin genuinely has no deadline" from "I could
not tell", `deadline_is_unknown()` is the predicate, and a blind pass emits
`WalletEvent::WatchtowerBlind`, retains a `WatchtowerFault`, and is refused from an exported watch
bundle.

### Structural nodes (split and combine parents) — two independent layers

1. **SE terminal-spend budget** (`POST /statechain/spend_budget`, owner-signed and irreversible;
   public `GET /statechain/spend_budget/<id>`). Right before co-signing a split, the SDK sets the
   parent's budget to exactly one more co-signature. The split consumes it; from then on the SE
   refuses everything on that parent — a later withdraw, transfer, renewal or fresh backup cannot be
   signed even by the legitimate owner. The budget may only **tighten**: `set_sig_budget`
   (`server/src/database/deposit.rs`) writes `min(count_finalized + remaining, existing)`, so an owner
   who already spent a node terminally cannot re-open it. This is Spark's "one spend per node" made
   explicit and **publicly verifiable**. Live evidence: `sdk04` (a second split over a terminal parent
   is refused, with the cause pinned negatively so a plumbing error cannot make the refusal pass
   vacuously); `sdk58` asserts the parent **is** terminal after an in-ladder split and rejects a
   bundle whose claimed parent is not.
   On the ladder the receiver does not take that flag on the coordinator's word: `attested_terminal`
   (`clients/libs/rust/src/tesr.rs`) derives terminality from the enclave-signed payload — budget
   present **and** `num_sigs ≥ budget` — for the parent and every intermediate segment, keeping the
   coordinator's answer only as a cross-check that refuses on disagreement.
2. **The ladder as fallback.** Even if the SE misbehaved, the parent's remaining pre-signed state is
   ordered *below* the child's in the timelock race (laddered), or locktimed above the locktime-free
   branch (un-laddered) — an honest receiver who exits in time wins. Refusal (instant, no race) plus
   timelocks (race, bounded).

**What is deliberately not claimed:** there is no enclave "single-active-state" refusal. The enclave
cannot know which state is current and *must* co-sign rivals, because a renewal is exactly that — a
lower-CSV extension over the same outpoint. What it enforces is **one signature per secnonce** (the
sealed secnonce is loaded and consumed in the same row-locked transaction; a second partial signature
over a different challenge finds it NULL and is refused — `lockbox/src/server.cpp`) and the budget
check above. Anything stronger is the receiver's census, not the SE's promise.

### Received split children are first-class, and deliberately not terminalized

A claimed child completes the standard SE **key handover**: the child's aggregate key is invariant
across the share rotation — which is exactly what keeps its pre-signed exit chain valid — and the
sender is **permanently locked out**. The child is therefore payable onward off-chain: whole via
`child_retransfer` (a replacement state over the same outpoint, zero sats, zero added depth) or split
again via `child_in_ladder_pay`. Each hop costs one co-signature and discloses exactly one superseded
state, counted by the receiver's census. Evidence: `sdk60` (alice → bob → carol, funding outpoint
unspent throughout) and `sdk17` (a partial second hop). Details in
[CHILDREN.md](../spec/CHILDREN.md).

The **one** exception is the Lightning-latched piece: it sits unclaimed past the pending-transfer
lock's window, because the SSP settles on its own schedule, so it is terminalized instead — a
permanent lockout. See [LIGHTNING.md](../spec/LIGHTNING.md); Lightning runs both directions on the
ladder via a HODL-invoice latch (`sdk63` pay, `sdk64` / `sdk67` receive, `sdk65` non-exact pay,
`sdk66` / `sdk68` failure and rollback, `sdk53` the latch guard).

### Optional per-coin hard bounds (SE-side, off by default)

- **Epoch deadline.** A coin MAY carry an `epoch_deadline`; past it the SE refuses *new*
  co-signatures with HTTP 410 `Gone` (`server/src/endpoints/sign.rs`; `RGB_E2E=7`). Like Ark's round
  expiry it bounds state lifetime — but unlike Ark, **unilateral exit stays live forever**, because
  it needs no SE signature, so funds can never be swept by missing a window. Coins with
  `epoch_deadline = NULL` — the default — are co-signable indefinitely.
- **Single-use coins.** A deposit-time flag for one-shot nodes (one co-signature ever) — the
  strictest form, used by the RGB DAG flows (`RGB_E2E=4`).

## What we took from each system

- **Spark**: zero-timelock structural nodes; one-spend-per-node, now SE-enforced and publicly
  queryable; and, independently, its zero-idle-footprint benchmark — which TES-R matches on the CSV
  side with **consensus-level** invalidation instead of 1-of-n key-deletion trust.
- **Ark / SuperScalar**: explicit lifetime bounds are *available* (the epoch deadline) but never
  mandatory, and never with expiry-sweep risk. Nothing in this protocol pays the operator by timeout.
- **Decker-Wattenhofer**: replace-by-lower-timelock, applied at one dedicated tier, so tree depth
  stays constant across all epochs instead of exhausting an update counter.
- **Mercury**: the pre-signed, SE-independent exit as the trustless floor under everything, and the
  absolute-locktime ladder itself, which remains the invalidation mechanism for the un-laddered shape
  and the retained root clock on every received coin.
- **Rejected**: revocation keys (they grow the collusion surface), mandatory round refresh (a liveness
  cliff), bounded DW update counters, and shared-UTXO factories — an operator-chooseable n-of-n root
  that never rotates, whose cohort can fresh-spend the root and confiscate every leaf below, including
  received-and-untouched ones ([PROTOCOL.md §8.1](../spec/PROTOCOL.md)).

## UTXO granularity

Spark leaves are fixed denominations; exact amounts need swap pools. Ark re-mints change each round.
Here, **exact amounts are a native off-chain operation** — one SE-co-signed transaction mints any
piece plus change, chainable to depth, each piece a full coin. The mechanics differ by shape:

- **Laddered — the in-ladder split.** A split is a **state tier** `SP` spending `X_m.out[0]`: a
  *descendant* of the trigger, never a rival for the funding outpoint `F`. That is what closes the
  theft vector a naive split would open — a past owner's retained no-timelock trigger has nothing to
  race. Each child hosts its **own** extension and state tiers, so the admission floor is
  `mercurylib::tesr::min_child_value(rate, dust)` = `2·(committed_fee + P2A) + dust`, checked **before**
  the parent is terminalized. At the shipped 3.0 sat/vB that is 2·(375 + 240) + 330 = **1,560 sat**;
  the sender's change leg is a one-rung spine tip and takes the smaller
  `min_spine_tip_value` = **945 sat**. Both take the rate as an argument — those are evaluations, not
  constants, and a reader who lifts one is quoting a rate rather than a floor. Attack-proven by
  `sdk58` (one real child accepted, every tampered one rejected for the *named* reason it targets)
  and driven end to end by `sdk59`.
- **Depth is capped, and the cap is derived rather than a literal.**
  `max_split_depth(base, per_level, epoch_blocks)` (`lib/src/transfer/receiver.rs`) searches against
  the admission rule the receiver actually applies, so the build side can never mint a depth no
  receiver could adopt. On mainnet it is **8**, giving `max_exit_txs` = **19 transactions**; on
  regtest **54** and 111. `enforce_split_depth_cap_shaped` (`clients/libs/rust/src/tesr.rs`) is the
  gate, and every input it uses is receiver-derived — a schedule the sender declares would let it
  inflate its own cap.
- **Un-laddered — the coloured / backup-chain split.** 1-sat resolution above the 330-sat
  `DUST_LIMIT`, with each piece additionally funding its own backup. Token pieces are packaged at
  `tokens::TOKEN_PIECE_SATS` = **4,074 sat**, derived so a received piece can still carry a full
  coloured rung rather than stranding at the floor.

Exact bounds, token packaging and pricing: the [granularity deep dive](granularity-deep-dive.md) and
[PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md).

## Exit economics: the honest ledger

**Payments are arbitrary amounts.** An arbitrary amount matches a coin the sender already holds only
by coincidence, so essentially every payment is an in-ladder split and the payee receives a **leaf**.
Leaf economics are the only economics that describe a real user, and the leaf lane has a side on which
we lose:

| leaf lane, per payment | block space | against ~154 vB on chain |
|---|---:|---|
| spent onward off-chain | **0 vB** | this is the product |
| swept and settled | **~105 vB** | **1.47× better — the cap without the discharge round** |
| walked out unilaterally | **250 – 2,719 vB** | worse than on-chain |
| **shipped default** | **418 vB** | **2.7× worse** |

The two ends of that range measure different things, and conflating them is how a flattering number
gets quoted. A **standalone** walk is the leaf's whole exit chain,
`tesr_exit_vbytes(d) = 293·d + 375` vB over `tesr_exit_txs(d) = 3 + 2d` sequential transactions
(`clients/libs/rust-sdk/src/config.rs`, pinned by `exit_cost_scaling_model`): **668 vB at depth 1**,
and 2,719 vB over 19 transactions at the mainnet cap of 8 — the top of the range. The **250 vB** at
the bottom is a leaf's own *tail* — its extension plus its state, `2 × TIER_VBYTES` — the marginal
cost when the `T + X_m + SP` prefix above it is already on chain and shared, which is what a batched
leaf-combine pays per leaf (`clients/libs/rust/src/combine.rs`, driven by `sdk83`). Read as a
marginal, 250 vB is still **1.62× worse** than doing the payment on chain; read as a lone leaf's
exit, 668 vB is 4.3× worse. Neither reading wins. And the on-chain alternative is not
"N × 154 vB": a one-to-many payout is one transaction with N+1 outputs, ~44 vB per recipient, so any
ratio derived against N separate payments is measuring an opponent that does not exist.

What the design sells is **velocity, not granularity** — every payment *after* the first costs another
~154 vB on chain and zero off chain. The design rule follows: a piece received and immediately cashed
out should never have been an off-chain split. The **discharge round**
([SPEC.md §5.4](../spec/SPEC.md)) is what would change this by an order of magnitude, and it is
**design, not built**: its SE enforcement point is empty.

The per-coin exit shape, for reference:

| Coin | Exit txs | vsize | Fee model | Wait |
|---|---|---|---|---|
| **Laddered, flat** | 3 pre-signed tiers (T → X_m → S_k) | **375 vB** (3 × `TIER_VBYTES`); up to ~834 vB with a P2A fee child on each tier in a spike | each tier carries a **committed** fee at `committed_fee_rate` (3.0 sat/vB, fixed at signing) and relays standalone; in a spike attach a ~153-vB P2A child per tier at the market rate | sequential relative CSV, `E_m` then `Δ_k`: worst **2,160 blocks ≈ 15 d** on a fresh mainnet coin, plus one confirmation per tier, shrinking 36 blocks per hop and per renewal. The clock starts only when T is broadcast |
| **Laddered, in-ladder child at depth d** | `3 + 2d` | `293·d + 375` vB | as above | `720·d + 2,160` CSV blocks plus one confirmation per transaction; depth cap 8 on mainnet |
| **Un-laddered, flat carrier** | 1 (backup) | decoded from the stored pre-signed tx | committed at co-sign; CPFP-bumpable from the backup's own output | absolute nLockTime: ≤ `initlock` (10,000 blocks on mainnet), −`interval` per handover |
| **Un-laddered, depth-N sub-coin** | N + 1 (N branch txs + backup) | decoded from the stored pre-signed txs | branch fees pre-committed by the splitter; backup as above | the branch is locktime-free and confirms immediately; the backup then waits from the split tip |

`estimate_exit_cost` returns the real numbers rather than a model — it decodes the stored pre-signed
transactions and reports `branch_txs`, `branch_vbytes`, `backup_vbytes`, `total_vbytes`,
`fee_sats_at(rate)`, `wait_blocks` (when the exit *completes*) and `exit_deadline_block` (the *safety*
deadline, the number a watchtower must act on), with `exit_deadline_blind` naming why a deadline could
not be computed. `unilateral_exit` handles both shapes without the caller choosing: on a laddered coin
it walks the tier chain idempotently, advancing as far as maturity allows and reporting the blocks
left until the next tier matures; on an un-laddered coin it broadcasts the locktime-free branch
immediately and reports the remaining backup wait instead of failing.

## The traded property, stated plainly

For laddered coins, an unconditional multi-day no-watch window is exchanged for *perpetual but
alarm-driven* watching. Nothing expires and nothing sweeps to the operator, so missed liveness is
never confiscation-by-design; and no theft transaction can become valid until ≥144 blocks (~1 day)
after a **publicly visible** on-chain trigger spends `F` — strictly more defender notice than a silent
calendar maturity. Towers are keyless, delegable and cheap.

Two limits belong in the same breath, because they are protocol facts and not implementation gaps: a
keyless tower **cannot fee-bump** a tier stuck under a risen relay floor (a CPFP child needs an input
it does not hold), so above that floor the defence falls back to the owner being online or to an
operator's funded-tower variant — and `SdkConfig::fee_bump` ships as `None` on both presets. And a
keyless tower **does not cover the root clock**: a laddered entry is exported with
`deadline_block: u32::MAX`, so a delegate watches only the event of `F` being spent, while
re-anchoring needs the owner's keys.

See [TRUST-MODEL.md](../spec/TRUST-MODEL.md) for the full boundary list and
[PROTOCOL.md §5.13](../spec/PROTOCOL.md) for the watchtower's normative limits.
