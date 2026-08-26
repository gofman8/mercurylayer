# Mercury Utexo — Protocol Specification

**Status: normative.** Requirements are labelled **REQ-n**, invariants **INV-n**, error semantics
**ERR-n**; keywords MUST/SHOULD/MAY per RFC 2119. Every labelled statement maps to a verifying test
in [§12 Traceability](#12-traceability) or is marked UNPROVEN in place.

---

## 0. How to read this document

### 0.1 Authority order — the DESIGN is normative, and the CODE follows

**Where this specification and the implementation disagree, the implementation is what changes.** A
divergence is therefore a defect with an owner, not a licence to weaken a sentence here — and not
something a reader may discover by accident, so every one that is known is listed in §0.4.

Two things follow:

* A section MAY specify behaviour that is not yet built, PROVIDED §0.4 records that it is not. What a
  section MUST NOT do is describe an unbuilt thing in the present tense.
* A measurement MAY NOT be overruled by a design statement. Where the design says one thing and a
  measurement says the design is not achievable — the depth cap of §6.1, the payment granularity of
  §5.1, the coloured lane's economics — **the measurement wins and the design changes.** Design
  authority is over choices, not over arithmetic.

### 0.2 What a normative statement in here rests on

1. **Every claim names its evidence, and evidence means a test that RUNS.** A test that asserts a
   DESCRIPTION can pass while the CONSTRUCTION underneath it is wrong; a source scan may assert
   presence, absence, ordering and window shape but never reachability, binding or behaviour —
   behaviour is proven by planting the defect and running the real checker.
2. **A measurement carries its target.** Test counts in this document say what was counted; a bare
   count of a set nobody named is neither wrong nor checkable.
3. **A rewritten assertion is a NEW assertion and must be run before it is cited.**

### 0.3 Scope, and the ONE shape a coin has

In scope: the SE (Mercury coordinator + lockbox), the client libraries (`mercurylib`,
`mercuryrustlib`, `mercury-rgb`), the wallet SDK (`mercury-utexo-sdk`), the SSP service
(`mercury-ssp`), and their Bitcoin/RGB/Lightning interactions. Companion normative documents:
[PROTOCOL.md](PROTOCOL.md) (tiers, renewal, terminal-freeze), [CHILDREN.md](CHILDREN.md) (first-class
split children), [LIGHTNING.md](LIGHTNING.md) (the Lightning latch), [TRUST-MODEL.md](TRUST-MODEL.md)
(the trust unit and the named residuals),
[PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md) (the block-space and value arithmetic).

**There is ONE protocol.** `claim()` establishes a TES-R exit ladder (§2.6) for every fresh confirmed
**root** coin, unconditionally **on a network whose enclave attestation identity is pinned — see §0.4
V-6, which is not a footnote: regtest now ships a pin, mainnet and the public testnets do not, and on
those NO ladder is established at all**. There is no `deposit_protocol_version` field and no
`UTEXO_PROTOCOL_DEFAULT` escape hatch that could opt a deposit into a pre-TES-R shape.

**There is ONE COIN SHAPE, and the build now takes it wherever it can be taken.** A coin is
**laddered**: its exit is the relative-CSV tier chain (§9.2), 0 vB of idle rent, and it never matures
while idle (INV-27). It is NOT deadline-free — the flat backup chain is retained, so the coin keeps an
absolute-locktime calendar which each whole-coin hop shortens by `interval`. INV-27 states the exact
scope.

An RGB **carrier** is laddered like any other coin. The mechanism is **CTES-R** — colour every TES-R
tier, so a tier spend carries the allocation forward instead of destroying it, and terminal-freeze
retires with the un-coloured lane (INV-29). Its gate passed against the live stack, and it is now the
DEFAULT wherever it can be established: both `SdkConfig` constructors READ
`TesrParams::attestation_identity_const` rather than stating a bool, so `colored_ladder` is on for
every network with a pinned enclave identity and off for one without. Today that means regtest ON and
mainnet OFF — and mainnet is off ONLY because no mainnet enclave is provisioned (§0.4 V-6), not
because a coin has a different shape there. Reading the pin is what makes those two facts one fact:
`colored_ladder` true with no pin is not a newer default, it is a wallet whose token lane refuses
forever.

**What is NOT a second shape: un-broadcast funding.** A split sub-coin's funding output is
un-broadcast, so it cannot root a trigger — a trigger would have no prevout to spend. That fact is
PERMANENT, and colouring a tier cannot change it: every in-ladder split CHILD and every spine-tip
change leg has un-broadcast funding, and producing exactly that is what the whole design exists for
(0 vB of idle rent, G12). Such a coin is laddered all the same — its exit material is the pre-signed
child ladder or the one-rung tip cap hanging under its parent's tiers (§2.2, §9.2) — and where a
sub-coin instead carries a branch, its `branch-<statechain_id>` rows ARE its exit material (§2.3): a
sub-coin with no `branch-` row cannot be exited at all.

**A coin with NO ladder is now a coin to REPAIR, not a lane to route.** `ParentShape::Unladdered` is
gone from the enum, and with it `split_coin`, the plain off-chain split, `ManyRoute::PlainSplit`,
`ensure_exact_coin`'s minting fallback and the daemon's `split_coin` RPC. `parent_shape`
(`transfer.rs`) now REFUSES when a coin carries no root bundle, no child bundle and no spine tip,
naming `claim()` and the recorded `ladder_skip_reason` as the repair; `parent_shape_opt` is the probe
form, kept for one caller — `has_exit_material`, where absence is data rather than a fault, because a
coin carrying only flat backup rows can still be exited and handed over WHOLE and erroring there
would drop a spendable coin out of the wallet's balance. A read FAILURE still propagates on both
([B3]).

> **[B1] IS NOW CLOSED BY CONSTRUCTION, and that is the sentence to keep.** The plain split spent the
> coin's funding output `F` directly — the same outpoint a prior owner's retained, un-timelocked
> trigger `T` spends — so that owner could void the split after the pieces were handed over, and the
> receiver had no way to detect the exposure. What stopped it before was a refusal INSIDE one
> function; the route is now DELETED, and a deleted route cannot be taken by a caller who forgets to
> check. The variant was removed BEFORE its users on purpose, so the compiler rather than a reviewer
> enumerated every route into the lane. `sdk29` and `sdk69` record the change from refusal to
> absence, and `sdk69` keeps the positive half: the same payment goes through in-ladder, where the
> retained trigger it sets aside has nothing to race, because `SP` DESCENDS from `T` instead of
> rivalling it (REQ-39, INV-18).

Material scoped to the retired shape — backup-chain handover, terminal-parent proofs, the
carrier-depletion arithmetic — is expected to be **deleted**, not migrated. That is a TARGET, not a
report: while the legacy coloured lane (§6.2) can still mint a sub-coin, the `branch-` writer, its
readers and every exit path over those rows stay, and the dated notes below record exactly where the
deletion stopped and why. Absolute deadlines are NOT on that list: INV-27 keeps the flat backup chain
on a LADDERED coin, so the absolute-locktime calendar survives the flip and was never
un-laddered-only material.

> **CTES-R removes ONE of the second shape's two cases, not both — measured 2026-08-17 by flipping
> the default and running the lanes.** Colouring a tier cannot broadcast a funding output, so "a
> split sub-coin whose funding is un-broadcast" survives the flip unchanged. It is the permanent
> shape of every off-chain child and every change tip, which is what the design exists to produce.
> Three consequences, each measured rather than reasoned, because a plausible deletion order here
> strands live coins:
>
> * **The flat-lane licence `PermanentLicence::FundingNotOnChain` must NOT be retired.** Its
>   `branch-` arm is LOAD-BEARING for the plain, non-RGB split sub-coin lane — written when a
>   receiver adopts an off-chain sub-coin, read when conveying it onward. A counterfactual with the
>   row removed refuses the coin; with the row corrupted the error is raised from inside that arm,
>   which is what proves the arm decides it. Its `ctesr-` arm is defensive (children route to
>   `child_retransfer` first, and a child that does reach the flat lane dies on an absence rather
>   than on the licence) and its `spinetip-` arm is dead (tips are refused by name in `execute_ex`
>   BEFORE the classifier, with a green CI guard) — so those two are retirable. **They are the two
>   that look load-bearing, and the one that looks legacy is the one carrying the lane.**
>
>   **RESOLVED by deleting the producer.** That paragraph used to read "the `branch-` shape is not
>   even legacy-only: `ensure_exact_coin` falls back to `split_coin`, so the lane is still a
>   PRODUCER". It no longer is. `split_coin` is DELETED, `ensure_exact_coin` refuses instead of
>   minting, and `ParentShape::Unladdered` is gone from the enum so the compiler — not a reviewer —
>   found all eight routes into it. The residual flagged here (the coloured exact Lightning lane
>   losing its last route) is now the stated behaviour rather than an open question: the exact lane
>   refuses and REQ-42's non-exact in-ladder fallback carries sats. **The RGB arm of Lightning pay
>   still refuses the in-ladder lane outright, so coloured EXACT pay has no route — that is a real
>   gap, and it is the LN lane's to close, not the split lane's.**
> * **`FLAT_RGB_CARRIER` GAINS a producer at the flip rather than losing one.** The `was_colored`
>   error arm is unreachable while `colored_ladder` is false; with it true, a carrier the coloured
>   builder refuses records the reason. The class is measurably non-empty: every pre-flip
>   1 560-sat-floor token piece is below both coloured floors and cannot even be carved, and for a
>   wallet's own booked-but-consignment-less issuance the recorded string is the coin's ONLY licence.
>   Retiring it strands those coins. They need a value migration, not a deletion.
> * **`is_legitimate_flat_reason` is not the gate.** It drives the `transferable` flag on
>   `flat_only_coins`; the decision is made by the `PermanentLicence` variant. Dropping a reason from
>   the former mis-reports a genuinely transferable child and changes no gate.
>
> So the order is: close V-6 → flip → migrate the sub-floor carriers → THEN retire licences and
> delete. Deleting first is what this note exists to prevent.

> **PROGRESS, 2026-08-17.** The flip is APPLIED and the licence retirement is DONE — at the gate,
> not at the label: `flat_conveyance_licence` no longer calls the RGB-carrier or funding-not-on-chain
> probes, both are deleted, and their `PermanentLicence` variants with them. Editing only
> `is_legitimate_flat_reason` would have changed reporting and no gate, which is why both halves were
> cut. Suite green at 810.
>
> **The deletion stops at the producer, and V-6 is why.** `split_coin` is reached from `transfer`
> through exactly one dispatch arm, `ParentShape::Unladdered`. Once every coin is laddered that arm
> is unreachable and the whole `branch-` lane — producer, writer, reader, and the backup-chain
> handover with it — deletes as dead code. But laddering needs a pin, no network ships one, so on the
> SHIPPED default nothing ladders, the arm is live, and deleting it removes a working path rather
> than a retired one. The same gate, reached from the other side.
>
> What is therefore NOT yet deletable, and must not be deleted before V-6 closes: `split_coin` and
> its `ensure_exact_coin` caller, the `branch-<id>` writer and reader, and every exit path that reads
> that row — a sub-coin with no `branch-` row **cannot be exited at all**. Absolute deadlines are a
> separate case and §0.3's "delete" is wrong for them as written: INV-27 keeps the flat backup chain
> on a LADDERED coin, so the absolute-locktime calendar survives the flip and is not un-laddered-only
> material.

> **SUPERSEDED IN PART, 2026-08-18 — the gate the paragraph above waited on MOVED.** `split_coin`,
> its `ensure_exact_coin` minting fallback, `ParentShape::Unladdered`, `ManyRoute::PlainSplit` and the
> daemon's `split_coin` RPC ARE deleted. The reasoning above was "on the SHIPPED default nothing
> ladders, so the arm is live" — and the shipped default changed when regtest's enclave identity was
> pinned (`TesrParams::REGTEST_ATTESTATION_IDENTITY`, re-derived from the seed this repository commits
> for its own dev stack by `regtest_attestation_identity_is_derivable_from_the_committed_dev_seed`, so
> the literal cannot rot into a lie). The REST of that list stands unchanged and is still the operative
> sentence: the `branch-<id>` writer and reader stay, and so does every exit path that reads that row.
> Two things were cut too far and restored by the build rather than by review — `split_amounts_floored`
> (the executable dust-boundary spec; the boundary was never un-laddered-only) and `SplitFloors::binding`
> (test-used, and `cargo build` does not compile `#[cfg(test)]`). Suite green at 812.
>
> **What the deletion costs on an UNPINNED network, stated rather than discovered.** `plan_payment`
> resolves `parent_shape` — the REFUSING form — for every candidate coin, and both `transfer` and
> `quote_transfer` call it and nothing else. So where `claim()` ladders nothing (V-6), the refusal is
> no longer confined to the token lane: a plain sats payment refuses at PLANNING. That is the intended
> direction — a coin that cannot be laddered is a fault to report — but it makes V-6 the gate on the
> whole payment surface of an unpinned network, not merely on carriers.

### 0.4 Divergence register — where the code does not yet meet this document

Each row is a defect in the CODE by §0.1. A row is removed only when the divergence is closed, never
when the sentence is softened. Nothing here is a hidden caveat: each is also stated where it bites.

| # | This document specifies | The shipped build does | Consequence, and what closes it |
|---|---|---|---|
| **V-1** | one coin shape: every coin, carrier or not, carries a coloured ladder | **DONE where an enclave is provisioned, 2026-08-17/18.** `SdkConfig::colored_ladder` no longer states a bool — both constructors READ `TesrParams::attestation_identity_const`, so it is **true on regtest** and **false on mainnet and the public testnets**, which have no provisioned enclave (V-6). The plain-split lane the old shape routed through is DELETED, not merely unused (§0.3) | **The gate is V-6, not economics — CORRECTED 2026-08-17, measured by flipping it.** This row used to read "what gates the flip is not safety but measured economics". That is wrong, and the correction is kept rather than replaced because the old sentence invites exactly the change that breaks. Flipping the two literals turns on `colored_ladder` for every wallet, which RETIRES the legacy coloured-split lane (`tokens.rs`, the `if self.inner.config.colored_ladder` gate). A carrier that CAN be coloured is then refused with "a later `claim()` pass will do it" — but `claim()` cannot ladder anything without a pinned attestation identity, and at the time of that measurement `TesrParams::attestation_identity_const` returned `None` for EVERY network, mainnet included. Measured live on those defaults: `transfer_tokens` refused permanently, wearing a transient error message. **What closed it was pinning the identity, not softening the coupling**: reading the pin makes true-without-a-pin unexpressible, which is why `colored_ladder_is_never_on_without_a_pinned_attestation_identity` enforces it in both directions. **What REMAINS is V-6 for every public network** — the row stays open until a mainnet enclave publishes an identity to pin, at which point this turns true there with no further edit. Economics (one coloured partial payment per carrier, a long unilateral exit for the child) is a real cost but it was never what blocked the flip |
| **V-2** | every client verifies a conveyed ladder | the nodejs and web clients **refuse** any transfer that DECLARES a ladder, and fall through to the FLAT `num_sigs == backups.length` check against a coordinator-supplied `interval` | Those clients are the UN-DEFENDED population, not an exempt one — the refusal keys on three SENDER-supplied fields, so it is a refusal of declared ladders, not a structural one. Closed by porting `verify_bundle` to wasm/JS and Kotlin |
| **V-4** | the `statechain_id ↔ aggregate` binding is attested, like the count | it is coordinator-supplied and unattested | A coordinator serving NULL leaves any coin with no ladder at all — which, since the plain-split lane was retired, is now a coin the planner REFUSES rather than one it routes cheaply (§0.3); serving a wrong value is not detectable in-protocol. The COUNT's half of this is closed ([TRUST-MODEL.md](TRUST-MODEL.md) B11); the binding is the half that remains |
| **V-6** | every fresh confirmed root coin is laddered by `claim()` | **regtest ships a pinned enclave attestation identity; no PUBLIC network does** — so on mainnet and the testnets EVERY laddering claim still refuses and the coin stays flat | `TesrParams::attestation_identity_const` now returns `TesrParams::REGTEST_ATTESTATION_IDENTITY` for regtest and `None` for bitcoin/mainnet and testnet/testnet3/testnet4/signet, because no enclave is provisioned there and a pin is only an anchor if it came from out of band — for regtest that channel is this repository (the dev seed it commits), for mainnet there is nothing to read, and inventing one is worse than absence: a wrong pin refuses every attestation and the tempting fix is to trust the key the coordinator serves, which is the hole D69 closed. `SdkConfig::regtest` and `SdkConfig::mainnet` both still ship `attestation_identity: None`; the only other source is the `UTEXO_ATTESTATION_IDENTITY` environment variable, which an embedder has not set — and where a pin EXISTS it is no longer overridable: a contradicting configured value now REFUSES at the resolver, because a pin a configuration file can override is a default, not a pin. On an unpinned network the pass records `LadderSkipReason::AttestationIdentityUnpinned` and continues — correctly, since verifying an attestation against a coordinator-served key proves nothing — but the effect is now WIDER than the token lane: with no ladder to resolve, `parent_shape` refuses inside `plan_payment`, so `transfer` and `quote_transfer` refuse a plain sats payment too (§0.3). Closed by compiling in a pin per network at release, or by an operator setting one |
| **V-5** | the two closed forms of the granularity model are evaluated and published | UNEVALUATED | An external dependency, not unfinished work: both are queries over DEPLOYED coins and regtest has none that mean anything |
| **V-7** | every coin's disclosure is checked against an aggregate the SE derived for itself (REQ-68) | the aggregate is stored ONLY when the client sends `user_public_key` at `/get_public_key`, and the check FAILS OPEN when no aggregate is stored. **MEASURED on the live regtest lockbox: 70 of 14 716 key slots carry one — 99.5 % of coins are unbound** | The unbound set is not merely legacy, it is still GROWING: the coordinator maps an empty `user_public_key` to "omit the field" (`server/src/endpoints/deposit.rs`), and the shipped wasm and Kotlin bindings predate it entirely — so any of them mints a fresh unbound coin on demand, and no later route can bind one (`se_aggregate` is written once, inside `/get_public_key`). What CLOSES it, in order: rebuild the wasm/Kotlin bindings from current `lib/`; then refuse an empty `user_public_key` so the set stops growing and becomes finite; then make the check mandatory. This shares its shape with **V-4** — that row is the coordinator's half of the same binding, this one is the SE's |

### 0.5 What this document does NOT claim

* **No in-protocol payment atomicity for a plain transfer.** A transfer is a one-way handover — a
  gift, not an escrow (TRUST-MODEL B8). Delivery-versus-payment needs the Lightning latch (§8) or an
  invoice.
* **No defence against the statechain trust unit itself** (SE + a past owner with a retained
  pre-rotation share, TRUST-MODEL B1). A fresh co-signature needs no backup, so no timelock reaches
  it. What the ladder changes is the notice period, not the possibility.
* **No claim that a sub-economic piece is final.** An ancestor's lowest backup rung voids an entire
  tree for the cost of one transaction, and does so whether or not its holder means to. **Read L-2
  with it:** a piece holder holds the parent's `T`, which spends the same `F` and carries no
  timelock, so a payee who acts can PRE-EMPT the backup across the whole window and always keeps
  their money. The option is free only BELOW break-even, where the walk costs more than the piece is
  worth — which is what "sub-economic" means.

---

## 1. Roles and trust

- **Owner** — a wallet holding one key share of a coin. Can spend only with the SE; can always
  exit unilaterally without it.
- **SE (statechain entity)** — server + lockbox holding the other key share of every coin. Blind
  MuSig2 co-signer: never sees amounts/addresses. Enforces single-use, spend budgets, epoch
  deadlines, the pending-transfer lock (REQ-36) and one signature per server nonce (INV-23) — it
  does NOT and cannot adjudicate rival states, because it blind-signs 32-byte hashes and never
  learns what it signed (INV-6). Cannot move funds alone; cannot block a unilateral exit.
- **SSP** — an application-level party (owner + Lightning node) bridging Mercury↔Lightning. Not
  trusted with custody: swaps are atomic (§8).
- **Issuer** — any owner that issues an RGB asset. No privileged runtime role beyond holding the
  contract issuance rights.

**REQ-1** The SE MUST NOT be able to move a coin's funds without the owner's co-signature (2-of-2).
**REQ-2** An owner MUST be able to exit to L1 without any SE cooperation (pre-signed material only).
**REQ-3** Trust reduces to: *the SE refuses to co-sign past a terminal budget, a passed epoch, a
single-use spend or an open transfer, and reports its co-signature count honestly* — and that count
is not taken on trust: it arrives under the enclave's `utexo/sig_count/v2` signature, verified
against the **PINNED enclave attestation identity** — not a chain-anchored per-coin key, because a
deep in-ladder-split ancestor has no chain anchor by design — over a nonce the receiver itself chose
(§3.3, REQ-38). Plus a liveness duty on the owner (or a delegated tower) — and with one coin shape it
is TWO CLOCKS on one coin, not one duty per shape. The **flat backup chain**, which a laddered coin
retains (INV-27), carries an ABSOLUTE calendar: the coin must be re-anchored, materialized or exited
before its backup locktime floor / epoch deadline (§9.4). The **tier chain** has no deadline at all,
because nothing matures while it sits un-broadcast; what it demands instead is that the defender react
within the CSV edge once someone publicly broadcasts the trigger (§9.5, INV-28). A coin that carries no
ladder — one whose `claim()` could not run (§0.4 V-6) — has only the first clock. No custody rests on
the SE in any case, and nothing ever expires to the operator.

### 1.1 Adversary model

Eleven adversaries are modelled. The two columns that matter are what each one CONTROLS and what it
provably cannot do.

| # | Adversary | Provably cannot | Can still do — and this is the residual |
|---|---|---|---|
| **X-1** | Prior owner of THIS coin | produce any new co-signature under `A` (the handover rotates the SE share and re-points the auth key); win the CSV race from behind; hide a co-signed rival from the census | broadcast the no-timelock trigger `T` purely to grief, choosing the moment (§9.5) |
| **X-2** | Prior owner of an ANCESTOR / the splitter | co-sign anything further over the parent (the budget ratchet makes it terminal); mint a rival to a child already handed over | **void a sub-economic piece with ONE 112-vB backup, at zero marginal cost per extra piece, and no operator can stop it** — the transactions are already signed. See §0.5 |
| **X-3** | The paying sender at conveyance | substitute a decoy funding output or sid; declare a soft CSV; drop an ancestor segment; pad the flat backups or the superseded set; skim value out of the tier chain; widen the schedule (`cap_schedule`); forge the `fee_rate` — `verify_bundle_ex` binds `bundle.fee_rate` to `TesrParams::for_network(..).committed_fee_rate` ahead of every value law, on BOTH synchronous verifiers | convey at a version that carries no key handover (A-12) |
| **X-4** | Receiver / payee | claim twice, claim after cancellation, or reverse a claim (the enclave keyupdate is irreversible and the counter monotonic) | nothing the model defends against — note the asymmetry in §0.5: a transfer is a gift, not an escrow |
| **X-5** | The blind SE enclave alone | steal (it never holds a full key); see ANY value; forge exit material after the fact; un-terminate a node; produce a second partial from a replayed session | refuse to co-sign — which is a freeze, not a seizure: the unilateral tree is pre-signed and SE-independent |
| **X-6** | The coordinator alone | under-report `num_sigs` or the budget — closed by the attested count, whose verifying key is pinned rather than served | serve a wrong or NULL `aggregate_xonly` (§0.4 V-4); serve a wrong `x1_pub`, which BRICKS the coin for everyone; drop the pending-transfer lock; withhold or reorder the mailbox |
| **X-7** | **SE + a past owner with a retained pre-rotation share** | — | fresh-co-sign an immediate spend. **No timelock reaches this**: a fresh signature needs no backup. This is the statechain trust unit (TRUST-MODEL B1) and it is irreducible, not a defect |
| **X-8** | Watchtower (delegated) | move funds — a keyless tower holds no key and broadcasts only the owner's own pre-signed material | fail to act, which costs a race; and a KEYLESS tower cannot fee-bump at all |
| **X-9** | Miner / mempool adversary / pinner | make a tier invalid | keep it unconfirmed. The P2A anchor slot is an AUCTION, not a race: an under-paying squat is refused and an over-paying one raises the tier's effective feerate at the attacker's expense |
| **X-10** | RGB counterparty (consignment sender, issuer, proxy) | forge a consignment the client validates — client-side validation is the only authority, and no proxy, SE or issuer is trusted for token rules | withhold a consignment (a liveness failure, not a theft) |
| **X-11** | Lightning SSP | take custody — the swap is atomic (§8) and the pre-payment gate binds recipient and amount | see every value in the swap |

**One thing the model does NOT have: an adversarial test that plays the COORDINATOR.** Every
adversarial test in this repo plays a malicious sender. X-6's "can still do" column is therefore
argued, not exercised, except where the attestation and terminality tests cover it.

### 1.2 Security goals

Twelve properties. Each states its own scope limit, because a goal stated without its limit claims
more than it can deliver.

| id | kind | property | scope limit — read this with the property |
|---|---|---|---|
| **G1** | safety | **VALUE CONSERVATION.** On every acceptance path, Σ(payload outputs) equals the tier total derived from the PARSED value of the output the tier spends; the residual is exactly one P2A anchor plus at most one zero-value opret; the chain is anchored hop by hop back to the funding value read FROM CHAIN | the yardstick is receiver-derived: `verify_bundle_ex` binds `bundle.fee_rate` to `TesrParams::for_network(..).committed_fee_rate` before any value law, and the child lane inherits it because `verify_child_bundle` re-verifies its embedded parent through the same function. The residual is the child lane's inverted DIRECTION — there INFLATION is the attack, because the amount comes from an un-broadcast `SP.out[j]` |
| **G2** | safety | **NO UNDISCLOSED SPENDING PATH.** No co-signature under the coin's aggregate exists that the receiver was not shown. Three conjoined obligations, never the equation alone: (a) exact equality against an ATTESTED count; (b) a per-item battery on every superseded entry; (c) slot uniqueness over the union of live and disclosed tiers | bounds spending PATHS, not VALUE — G1 carries the value claim. Treating it as arithmetic admits junk padding, and a genuine tier disclosed twice inflating the expected total for free |
| **G3** | safety | **OLD STATE DIES.** Every superseded state is disclosed and provably out-raced: the live state carries a strictly-lower CSV over the same outpoint, and every pre-renewal state hangs on a parent that can never confirm | **race-conditional, not axiomatic.** The live rival must also be RELAYABLE — co-signing a rival at a rate that cannot relay loses a race the verifier believes it wins |
| **G4** | liveness | **EXIT AVAILABILITY.** The current owner can always reach L1 with pre-signed material alone — no counterparty, no SE call, no key held by anyone else — in `3 + 2d` transactions at depth `d` | bounded four ways, all measured: FEE (above the committed 3.0 sat/vB a tier needs a CPFP child, and a keyless tower cannot build one); VALUE (below `V_min` the walk costs more than the piece); DEPTH (mainnet cap **8**, 19 transactions); COLOUR (a coloured coin's re-anchor is a manual call nothing schedules) |
| **G5** | safety | **ALLOCATION INTEGRITY (RGB).** The allocation the receiver validated is the one that settles. RGB transitions anchor only in signed-once transactions or coloured tiers; a PLAIN tier spend of a carrier destroys the allocation | rests entirely on a carrier never being laddered plainly. That is why the automatic passes exclude carriers, and why the sever route exists |
| **G6** | safety | **FINALITY.** A completed claim is irreversible: the keyupdate cannot be undone, the counter is monotonic, and claim/cancel are mutually exclusive | finality is at CLAIM, not at conveyance. A conveyed-but-unclaimed transfer is reversible for the lock window — and is already presented to an SSP as if received |
| **G7** | safety | **NON-CUSTODY UNDER OPERATOR COMPROMISE.** A hacked operator may take FUTURE deposits and FUTURE transitions; pre-hack state left untouched is safe. Every spend needs both shares, and the hacked SE holds only the post-rotation one | the surviving path is X-7. This is the requirement that disqualifies a shared-root factory architecture, where one confirmation confiscates every coin under the root |
| **G8** | safety | **NO CONFISCATION BY DESIGN.** Nothing expires, nothing sweeps to the operator, no output pays the operator by timeout. Missed liveness costs a race, never a forfeiture | under pressure at exactly one place: the sub-economic leaf is not confiscated by the OPERATOR, but it is forfeit to the party who split it |
| **G9** | safety | **SE BLINDNESS.** The SE signs 32-byte sighashes; a coloured sighash is byte-indistinguishable from a plain one; consignments stay P2P | covers CONTENT, not traffic — the SE still learns sids, auth keys, counts, flags, timing and the caller's endpoint. Blindness is also why every operator-side value fix is impossible: "the SE refuses to co-sign a piece below a floor" is a WRONG proposal and must not be proposed |
| **G10** | safety | **AUTHORIZATION INTEGRITY.** Only the current owner authorizes an irreversible operation, single-use and endpoint-bound | the single-use nonce is deployed on **FOUR** endpoints — `withdraw/complete`, `deposit/get_derived_token`, `statechain/spend_budget` and `transfer/cancel` — and every OTHER mutating endpoint takes a static, replayable signature |
| **G11** | safety | **ADMISSION SOUNDNESS.** A receiver never admits a coin whose exit provably cannot complete, and every term of an admission test is receiver-derived — a serde field is not admissible | enforced in TIME and in STRUCTURE; the uncovered dimension is VALUE. `min_child_value` is not a floor that ignores economics — it IS `V_min` evaluated at the shipped rate (1 560 sat at 3.0 sat/vB), correct at that rate and no other |
| **G12** | liveness | **ZERO IDLE COST ON THE CSV SIDE.** All tiers are un-broadcast, `T` carries no timelock, and CSV does not tick until the parent confirms — so the TIER CHAIN adds 0 vB of rent and no deadline, however long the coin or the DAG sits idle | **this is NOT "the coin never touches the chain".** A laddered coin RETAINS its flat backup chain, whose locktimes are ABSOLUTE and do age — so the flat calendar, not the CSV hop budget, is what sets the real maintenance cadence. See §14.3's cadence entry for the measured numbers. A leaf is worse: its clock is the parent's lowest backup rung, a height belonging to the splitter |

### 1.3 Assumptions

Every goal above holds only under these. A specification that states goals without stating these
claims more than it can deliver.

| id | assumption | if false |
|---|---|---|
| **A-1** | **FEE MARKET** — the market rate stays at or below the committed **3.0 sat/vB** long enough for each tier to relay and confirm inside its head start; where it does not, someone can attach a ~153-vB v3 child to the tier's 240-sat P2A anchor and submit a 1P1C package | the CSV edge degrades into a fee race a fixed-fee tier cannot win. The remedy is BUILT and live-verified but not universal: it needs a funded UTXO, a signer and a Core RPC endpoint (electrum has no `submitpackage`), so a keyless tower has no move and the child watch lane has no bump variant |
| **A-2** | **CHAIN LIVENESS AND RELAY POLICY** — blocks are produced; v3/TRUC, P2A, 1P1C and sibling eviction are honoured; no reorg deeper than `confirmation_target` | v3/P2A/1P1C are relay POLICY, not consensus: a policy change can make a pre-signed tree un-relayable. TRUC's one-ancestor rule already bites — a second rescue funded from the first's unconfirmed change is refused at any price |
| **A-3** | **ENCLAVE BEHAVIOUR** — old shares are destroyed at every transfer, the secnonce is one-shot, the budget ratchet only lowers, and attestations come from the enclave's pinned identity | this is X-7 / TRUST-MODEL B1 and it cannot be verified: any proof of erasure attests one instance of the data. There is no client-facing attestation of the enclave itself, and production runs the plain-C++ lockbox container, not SGX |
| **A-4** | **COORDINATOR HONESTY** for the facts only it holds — the sid ↔ aggregate binding, `x1_pub`, mailbox behaviour, the pending-transfer lock, both sign gates, batch atomicity | nothing in the protocol detects a violation of most of these (§0.4 V-4). The one item that IS closed is the count and budget |
| **A-5** | **RGB VALIDITY** — client-side consignment validation is sound and complete | R8 collapses and a forged consignment books a wrong asset or amount; the receiver has no other authority |
| **A-6** | **OWNER OR DELEGATE AVAILABILITY** — someone is awake within the CSV window once a trigger is broadcast, and inside the margin before a coin's absolute FLAT-backup deadline — which a laddered coin has too (INV-27), so this is not an obligation the ladder retires | delegable and redundant, but NOT removable: timelock security is defined by acting before maturity. There is no unconditional no-watch window — a received coin is watched from receipt, forever |
| **A-7** | **THE USER'S CHAIN VIEW** — the indexer honestly reports tip, spentness, confirmations and fee rates, and delivers broadcasts | it can blind and delay but not steal. Worse in practice: the dev defaults are plaintext, and an on-path attacker is strictly stronger than a lying indexer |
| **A-8** | **PARAMETER PROVENANCE** — the CSV schedule and the flat-ladder parameters are COMPILED IN per network, never coordinator-served | the coordinator would define the defence. The tempting fix — derive the interval from the conveyed chain — is CIRCULAR and accepts exactly the padding it exists to stop. Cost of the fix, stated plainly: a config typo is a fleet-wide outage rather than a quiet weakening |
| **A-9** | **LOCAL STATE DURABILITY** — the owner retains `wallet.db` and, for token wallets, the whole RGB data directory | loss of local state is loss of funds, and the mnemonic alone is NOT a backup. The SE is blind and cannot re-serve exit material — that is the privacy design, not an oversight |
| **A-10** | **SINGLE LIVE INSTANCE PER WALLET** | two processes on one wallet can broadcast stale state against each other. The lock is in-process only and the blind SE cannot arbitrate. Bundle restore is disaster recovery, not device sync |
| **A-11** | **THE RETAINED CHECKS SUBSUME the unverified blinded-MuSig commitments on the laddered lane** | UNANALYSED — commission the analysis. What keeps the two lanes' residuals from composing is that the legacy arm still runs the full legacy verifier |
| **A-12** | **CLIENT CONFORMANCE** — every client that can receive a coin either verifies the ladder or refuses it BY NAME, and a decoder rejects a version it does not implement | the unknown-version reject arm EXISTS and is exact-set — `ADMISSIBLE_PROTOCOL_VERSIONS = [0, 2, 4]` and `admissible_shape` refuse anything outside it BY NAME (numeric ordering carries no meaning, so an unknown value cannot be "at least" anything), at the top of `validate_encrypted_message` (the claim path) and of `prepay_flat_census`. What remains is narrower and real: the CHILD lane reaches neither call — `prepay_child_census` has no shape check and gates with `<`, so every value in `[4, u32::MAX]` clears it, and `validate_encrypted_message`'s child block returns before that function's check. Inert today (v99 selects the arms v4 does) and exactly the ordinal reading the exact-set rule forbids. The FLOOR is the worse half: the sender picks it, and a low-version conveyance carries neither the key handover nor the transfer signature |

---

## 2. Data model

### 2.1 Coin
A statechain coin is a Bitcoin P2TR UTXO whose key is the MuSig2 aggregate of `owner_pubkey` and
`se_pubkey`, plus SE-side state `{statechain_id, auth_pubkey, single_use?, epoch_deadline?,
sig_budget?}` and client-side state (**amount, locktime**, backup txs).

> **The SE holds NO `amount` and NO `locktime` — a schema fact, and load-bearing.** `statechain_data`
> carries `id, token_id, auth_xonly_public_key, server_public_key, statechain_id, enclave_index`, plus
> `single_use`, `epoch_deadline`, `sig_budget`, `user_public_key` and `aggregate_xonly` added by later
> migrations. No amount column and no locktime column is added by ANY migration. This is G9
> (blindness) in the schema: it is why the SE cannot enforce a value floor (L-3) and why every value
> defence in this document is receiver-side.

Coin status lifecycle (client): `INITIALISED → IN_MEMPOOL → UNCONFIRMED → CONFIRMED →
{IN_TRANSFER → TRANSFERRED | WITHDRAWING → WITHDRAWN | DUPLICATED | INVALIDATED}`.
A split child routed to a unilateral exit (§9.2) is booked `WITHDRAWING` with **no** withdrawal tx
and no withdrawal address — its progress is the pre-signed exit chain, not one watched txid — and
status polling MUST accept that combination; treating it as an error makes every later poll fail for
the life of the coin.

**INV-1** A coin's `amount` equals the sats of its funding output.
**INV-2** A `CONFIRMED` coin has ≥ `confirmation_target` confirmations of its funding UTXO (or, for
an off-chain sub-coin, a validated exit branch — §2.3).

### 2.2 Sub-coin (off-chain)
A sub-coin is a coin whose funding tx is **un-broadcast**: it is an output of a split/combine tx
that the SE co-signed but nobody broadcast. Its `utxo_txid:vout` points at that un-broadcast tx.
An **in-ladder split child** (§6.1) is also funded by an un-broadcast tx, but its exit material is
the pre-signed child ladder (`ext_child` + `state_child`) hanging under the parent's tiers, not a
branch + absolute-locktime backup.

### 2.3 Exit branch
An exit branch is the chain of fully-signed split/combine txs from a spend of an **on-chain**
outpoint down to the tx that funds a sub-coin, stored root-first under `branch-<statechain_id>`.

**INV-3** Every tx in a branch is consensus-valid against its predecessor's outputs; the branch
root spends an on-chain, unspent, confirmed outpoint. (Enforced by `validate_branch`.)
**INV-4** Branch (structural) txs carry no relative/absolute locktime — they are immediately
broadcastable.

### 2.4 Backup ladder (absolute locktime)
Each coin has ≥1 pre-signed backup tx paying the owner's address, at absolute locktime
`h + initlock − interval·k`. The first backup (`k=0`) is at `deposit_height + initlock`; every
transfer hands the new owner a backup one `interval` lower.

**INV-5** For any coin, the current owner's latest backup locktime is strictly lower than every
previous owner's backup locktime (current owner wins the exit race), and each hop decrements by
EXACTLY `interval`. `initlock`/`interval` are **compiled in per network**
(`TesrParams::flat_ladder_params`: 10 000/100 on mainnet, testnet and signet; 1 000/10 on regtest —
100 decrements of capacity either way, of which **99 are usable**; hop 100 lands on the co-sign
anchor and the receiver's `lock_time <= tip` rule refuses it). The coordinator's own copy is a
cross-check only: `info_config` REFUSES the call outright if the two disagree. Taking `interval`
from the coordinator would let the coordinator define the defence, and deriving it from the conveyed
chain is circular — a padded chain of uniform `interval/2` hops validates against itself, which is
the padding INV-5 exists to stop.

For a coin that carries **no ladder** this chain IS the exit material — together with the branch
above it, where there is one (§2.3, INV-17) — and it is a finite budget (`initlock` blocks, spent by
each transfer and by wall-clock time). TWO cases reach it, and only one of them is a defect: a
**branch-carrying sub-coin**, excluded from ladder establishment BY DESIGN for as long as its funding
tx stays un-broadcast (REQ-37 — a trigger would have no prevout to spend), and a coin whose `claim()`
could not run at all (§0.4 V-6), which is a coin to repair (§0.3). When it nears the floor the coin
must be moved to L1 by exit (§9), materialized if it is a carrier (REQ-33), or COOPERATIVELY
re-anchored on-chain via `refresh` (§9.4, REQ-31).

For a **laddered** coin the backup chain is still built, conveyed and structurally validated on
every transfer — its COUNT is a term in the receiver's census (REQ-38) and INV-5 is the only defence
against a sender inverting the ladder or padding it with duplicates — but it is not the exit path:
`unilateral_exit` walks the tier chain and broadcasts NO absolute-locktime backup (`sdk50`), and the
coin's exit is not calendar-bounded — but the retained chain's own locktime still is, and that is
what bounds how long the coin may sit un-re-anchored (INV-27, `sdk86`).

### 2.5 Ancestor record
For each sub-coin, its structural ancestors (the split/combine parents) are stored under
`parents-<statechain_id>` (parent id + inherited ancestors).

### 2.6 TES-R exit ladder (laddered shape)
Above the funding UTXO `F` sits a pre-signed, **un-broadcast** tier tree (PROTOCOL.md §5.2): a
**trigger** `T` (spends `F`, no timelock, signed once at deposit detection), **extensions**
`X_0…X_m` (mutually exclusive spends of `T.out[0]`, input nSequence = relative-CSV `E0 − m·δE`), and
**states** `S_0…S_k` on `X_m.out[0]` (nSequence = CSV `D0 − k·δ`) paying the current owner's own
seed-derived key. A split state `SP` (§6.1) is a state tier too, but it is a SPINE tier: it is
pinned at `SPINE_CSV = 0`, which is how it out-races the `S_0` it replaces over the same output —
and the builders refuse the split outright unless that `S_0` sits strictly above it (`s0_csv <=
SPINE_CSV` is a hard refusal, `tesr.rs`). Every tier is nVersion=3 (TRUC), carries a committed fee
(`committed_fee(rate)` over `TIER_VBYTES = 125`, the MEASURED signed vsize — TES-R signs
`SIGHASH_ALL`, so every tier carries the explicit 65th witness byte) so it relays standalone, and a
240-sat P2A anchor for live-rate fee-bumping.

> **A multi-child split state pays MORE than that constant.** `TIER_VBYTES` prices a one-payload
> tier; an `SP` carrying `n` children is charged `committed_fee_for_outputs(n, rate)` over
> `TIER_VBYTES + (n − 1)·P2TR_OUT_VBYTES` — 43 vB per extra child — and `build_split_state_from`
> refuses unless `Σ children` equals `tier_out_total` computed on exactly that. Quoting
> `committed_fee(rate)` for an `SP` understates its fee by `(n − 1)·43·rate`.

**INV-27 (idle coins never age — ON THE CSV SIDE)** No tier is on-chain, and a BIP-112 relative
lock does not tick until its parent confirms, so no tier anywhere matures until someone broadcasts
`T`. An idle laddered coin — and an idle split DAG — therefore costs **0 vB of rent** and its exit
chain is unchanged by the passage of time.

**This is a statement about the tiers, not about the coin.** A laddered coin also retains its
flat backup chain, whose locktimes are ABSOLUTE, and that chain is what sets the real maintenance
cadence. The coin therefore has a finite calendar deadline `L`, consumed from two directions:

| | mainnet | regtest |
|---|---|---|
| `L` at deposit | tip + `initlock` = tip + 10 000 | tip + 1 000 |
| cost per whole-coin hop (`interval`, INV-5) | 100 | 10 |
| whole-coin hops the ladder affords | 100 | 100 |

Verified by **`sdk86`**, which measures BOTH clocks on the same coin across two hops: after 300 idle
blocks the received coin's exit chain is byte-identical and `F` unspent (the CSV half), while the
same coin has lost 300 blocks of calendar and each hop cost exactly `interval` more (the flat half).
`sdk30` (a) and `sdk40` verify the CSV half only — `sdk30` (a) idles a k=0 deposit, which is
structurally unable to witness the hop cost.
**INV-28 (lower CSV wins)** Every transfer and every renewal co-signs a state (extension) at a
strictly LOWER CSV than the one it supersedes, so the current owner's tier matures first and each
superseded tier's parent becomes unconfirmable — invalidation at the CONSENSUS level. There is no
second, independent SE-side layer under it: a blind co-signer cannot tell a rival state from a
renewal, and the code does not try (INV-6). What stands beside consensus is the receiver's
census (REQ-38) — every co-signature the SE ever issued must be accounted for, against an
enclave-ATTESTED count — plus the pending-transfer lock (REQ-36). Verified by `sdk40` PART 2
(a stale ladder is defeated by a cooperative de-trigger) / PART 3 (a renewed extension supersedes
the old one at consensus level), `sdk41`, `sdk51`.

Renewal and rollover are **off-chain**: when the next state would fall below `D_floor` the SDK
co-signs a fresh extension `X_{m+1}` (two blind co-signs, zero on-chain bytes); at extension
exhaustion it rolls over into a fresh level via a self-split. Off-chain state transitions are
therefore unbounded with no mandatory chain touch (`sdk42` lifecycle + persistence, `sdk43`
rollover). No SE endpoint is added for this — renewal is ordinary blind co-signing (§3.2).

---

## 3. SE API (normative)

All endpoints are HTTP JSON on the Mercury server. Encrypted transfer messages are opaque to the
SE (owner-encrypted); the SE never deserializes `TransferMsg`.

### 3.0 CENSUS COMPLETENESS, and the shape obligation that discharges it

The receiver's anti-theft census is an EXACT equality —
`se_num_sigs == flat_backups + tiers + superseded` — and its soundness rests on **A11**: every
co-signature the SE issued for this coin is accounted for by exactly one disclosed item. A11 is not
an assumption; it is a **theorem with four premises**:

1. **A3** (the SE signs only what a valid request asks it to sign);
2. **no CO-1** (the enclave's key material and the counter it attests are not both held by the party
   the receiver is being protected from). **Of the four, this is the one that is NOT discharged**:
   it is published as an accepted bound in the named-limitations section rather than proved here,
   and a theorem whose unmet premise is buried is worse than an assumption stated plainly;
3. **blind-signing concurrency is 1 per key** (serialised `sign/first`, one signature per server
   nonce, INV-23);
4. **the counted categories are PAIRWISE DISTINCT** — no object can be counted as a flat backup and
   as a tier, or as two tiers.

**Premise 4 is discharged by SHAPE, not by a runtime check** — and the specification must be exact
about WHICH parts of the shape a verifier tests, because a warning attached to a rule nobody checks
protects nothing.

A flat backup is nVersion 2, nSequence 0, a height `nLockTime` above tip, exactly one
non-`OP_RETURN` output. A tier is nVersion 3, `nLockTime` 0, exactly one 240-sat P2A anchor, a CSV
inside its bound band, and provably unconfirmable once superseded. No transaction satisfies both
descriptions, so the categories cannot overlap and nothing can be double-counted.

Of those shape properties, **at least seven are enforced on an acceptance path and two are not**. The
ones enforced only in code bodies rather than in a doc comment are easy to miss, so they are named
here:

* **flat `nSequence == 0`** — `validate_backup_chain_v2` calls `verify_transaction_sequence` on every
  backup, which errors on any non-zero sequence;
* **flat height `nLockTime` above tip** — the next call in that same loop,
  `verify_if_locktime_is_reasonable_tx_version_and_output_size`, refuses a non-height locktime, one at
  or below the tip, and one above `tip + initlock`;
* **tier CSV inside its bound band** — in `verify_child_bundle`, and (so it is not one lane
  generalised) in the ordinary superseded-tier battery, which selects `(e_floor, e0)` or
  `(d_floor, d0)` by tier kind and also requires the value to sit on the state grid;
* **provable non-confirmability of a superseded tier** — the step adjacent to that band check.

Both flat checks sit on real receive paths — three call sites in the claim path plus the
conveyed-child verifier — not on orphan helpers.

| property | enforced by | where |
|---|---|---|
| flat: nVersion **2** | `if tx_n.version != 2 { … }` | `lib/src/transfer/receiver.rs`, flat validation |
| flat: exactly one non-`OP_RETURN` output | `if payment_outputs != 1 { … }` | same |
| tier: exactly one 240-sat P2A anchor | `bind_single_p2a_anchor` | `clients/libs/rust/src/tesr.rs` |
| tier: nVersion **3** | **NOTHING.** Set by the builders (`lib/src/tesr.rs`); read only by `assert_eq!` fixtures. A repo-wide search for a production `version != 3` returns nothing | — |
| tier: `nLockTime` **0** | **NOTHING** on the tier side (INV-4 is a build-time invariant) | — |

**The separation that actually holds is `payment_outputs != 1` against the anchor rule**: a flat
backup has exactly one payment output and no anchor; a tier carries a payload output **plus** a
240-sat P2A anchor, so it fails the flat side's output test and passes the anchor rule, and neither
can be mistaken for the other. That is what discharges premise 4.

⚠️ **Two of them — tier nVersion 3 and tier `nLockTime` 0 — could be relaxed today with nothing
in the tree failing.** "Re-check against premise 4" therefore means re-check against the rows marked
enforced; changing a builder convention is a change to what this document DESCRIBES, and it will not
be caught by a test.

**Those rules carry a CENSUS obligation and not only a relay/race one.** Every one of them also
exists for a transport reason, and the failure mode is that a future change relaxes one for a
perfectly good relay-side reason, the two categories stop being distinguishable, and the census
silently begins counting one thing as another. A change to any shape rule above MUST be re-checked
against premise 4.

A runtime distinctness check is deliberately NOT specified: it would re-derive at every claim a
property the shapes already guarantee, paying forever for a premise that is structurally true.

### 3.1 Deposit / keygen
- `POST /deposit/init/pod` `{token_id, auth_key, ...}` → `{server_pubkey, statechain_id, ...}`. There is
  no bare `/deposit/init`; the only other mounted deposit routes are `GET /deposit/get_token` and
  `POST /deposit/get_derived_token`.
  Registers a new coin key-share. **REQ-4** MUST require a valid deposit token.
  `single_use` and `epoch_deadline` MAY be set at init.
- `POST /deposit/get_derived_token` `{statechain_id, auth_sig, count}` → `{token_ids}` — FREE
  **derived-slot** vouchers for slots created by SE-co-signed flows over the named EXISTING
  statechain (split pieces/change, combine outputs, refresh re-anchors). `auth_sig` is the
  single-use endpoint-bound owner challenge (`"<nonce>:<sig>"`); one consumed nonce authorizes the
  whole `count` batch. Never routed to the token server; works on any network.
  See REQ-35 / ERR-13.

### 3.2 Signing (blind MuSig2)
- `POST /sign/first` `{statechain_id, signed_statechain_id, ...}` → `{server_pubnonce}`.
- `POST /sign/second` `{statechain_id, session, server_pub_nonce, ...}` → `{partial_sig}`.

**REQ-5** `sign/first` MUST reject if `single_use` and the coin already has ≥1 finalized signature
(ERR-1).
**REQ-6** `sign/first` MUST reject if `epoch_deadline` is set and the SE clock ≥ it (ERR-2).
**REQ-7** `sign/first` MUST reject if `sig_budget` is set and finalized signatures ≥ budget (ERR-3).
**INV-6 (there is no single-active-state rule)** The SE does NOT refuse a second, conflicting state
for a coin that is within its budget, epoch and transfer lock: `sign/first` re-serves a pending
nonce while its challenge is NULL and otherwise issues a FRESH one, gated only by REQ-5/6/7 and
REQ-36 (`server/src/endpoints/sign.rs`). It could not do otherwise — it blind-signs a 32-byte
sighash and never learns that a tier is a tier (§3.2), so "conflicting" is not a predicate it can
evaluate. What IS enforced per coin is one signature per server nonce (INV-23, a key-leak defence,
not a rival defence), serialisation of concurrent `sign/first` calls, and the terminality gates
above. Rival prevention lives at consensus (INV-28) and in the receiver's census (REQ-38). Any
document that cites an SE single-active-state refusal as a second independent layer is describing a
mechanism this system does not have.
**REQ-36 (pending-transfer lock)** While a transfer of a coin is OPEN — conveyed, not yet completed
by the receiver, and still inside its open window — `sign/first` and `sign/second` MUST refuse
every co-signature for that coin, and `/transfer/sender` MUST refuse to re-address an open transfer
to a different auth key (both ERR-14; fail CLOSED on a database error). Every legitimate sender
pre-sign (the backups and the receiver-paying state `S'`) happens BEFORE the transfer is opened, so
no honest co-sign falls inside the window. This is a RELEASABLE lock, not a monotonic budget
(INV-24): it closes the window in which a still-owner sender co-signs a lower-CSV rival that
out-races the state it just conveyed, and it is what lets a received split child be handed over
WITHOUT terminalizing it (§6.3). *Coverage:* the honest-path half is well covered (every transfer
and in-ladder-split E2E runs with the lock live, which is what proves the pre-sign re-ordering
correct); the adversarial refusal itself — a sender co-signing a rival, or re-addressing, inside the
open window — is driven by `tb05`, which conveys a coin, leaves it open and unclaimed, and asserts
that a second `transfer_sender::execute` for the same id to a DIFFERENT recipient fails with "coin has
an open transfer".

The tier co-signs of ladder establishment, renewal, rollover and in-ladder splits all use these two
endpoints unchanged — the SE blind-signs a 32-byte sighash and never learns that a tier is a tier —
and each one increments the public `num_sigs` the receiver's census reads (REQ-38).

### 3.3 Transfer relay
- `POST /transfer/sender` → `x1` (receiver-binding scalar).
- `POST /transfer/update_msg` `{statechain_id, auth_sig, new_user_auth_key, enc_transfer_msg}` —
  stores the encrypted message. **REQ-8** MUST validate the sender's auth signature.
- `GET /transfer/get_msg_addr/<auth_key>` → encrypted messages for a receiver.
- `POST /transfer/receiver` — rotates the SE key share to the new owner; **REQ-9** after this the
  previous owner's share MUST be unusable.
- `POST /transfer/unlock` — releases a batch-locked coin (owner or SE side).
- `GET /info/statechain/<statechain_id>?attestation_nonce=<32B hex>` → `{num_sigs,
  aggregate_pubkey, sig_budget, has_sig_budget, sig_count_attestation,
  sig_count_attestation_pubkey, enclave_public_key, …}` — the counter every receiver's census
  (REQ-38) is checked against. It is **attested, not asserted**: the count AND the budget travel in
  one enclave signature over `sha256("utexo/sig_count/v2" ‖ statechain_id ‖ u32_be(num_sigs) ‖
  u8(has_budget) ‖ u32_be(budget) ‖ nonce32)`, verified against the **PINNED enclave attestation
  identity** — never against the served `attestation_pubkey`, which the coordinator chooses — and
  over a nonce the CALLER generated, so a genuine older attestation cannot be replayed.

  > **The verifying key is pinned, not chain-anchored, and the difference is the whole point.** A
  > chain-anchored `enclave_public_key` bound to `tx0` works only for a coin that IS on chain, and a
  > depth-≥2 in-ladder-split ancestor's funding output is **deliberately un-broadcast** — for those
  > ancestors there is nothing to bind to, and the verifying key would arrive in the same response as
  > the signature. The enclave signs every attestation with one long-term identity
  > (`utexo/attestation-identity/v1`, published at `GET /attestation_identity`) and the client pins
  > it, so the check is independent of the coordinator's word AND of whether the coin is on chain —
  > it holds at every split depth. Resolution is **pin → config → refuse**: a compiled-in pin is not
  > overridable, and "neither" is a refusal, never a fallback to the served key. A missing
  > attestation, a mismatched key, or a `has_sig_budget` the enclave cannot state is REFUSED, not
  > defaulted (`get_statechain_info`, `verify_sig_count_attestation`); there is no phased rollout.
  > Without it a coordinator that under-reported `num_sigs` by `k` would hide `k` co-signed rival
  > states while the exact-equality census still balanced.

### 3.4 Withdraw
- `POST /withdraw/complete` — **the SE does NOT co-sign here.** The route validates a single-use,
  endpoint-bound signature and then RETIRES the statechain: an HTTP DELETE to the lockbox's
  `delete_statechain/<id>`, then DELETEs across `statechain_transfer`, `statechain_data` and
  `statechain_signature_data`. The co-signature that produces the withdrawal transaction is taken on
  the ordinary sign path BEFORE this call; this route is the retirement step that follows it.

### 3.5 Lightning latch (SE-minted preimage + external hash)
- `POST /transfer/paymenthash` `{statechain_id, auth_sig, batch_id}` → `{hash}` — SE generates a
  preimage, returns `sha256(preimage)`; the coin transferred under `batch_id` is claim-locked.
- `GET /transfer/paymenthash/<batch_id>` → `{hash}` — the batch's hash (external hash if set, else
  `sha256(SE preimage)`).
- `POST /transfer/transfer_preimage` — returns the SE preimage **iff the batch is unlocked**
  (`locked=false`). **REQ-10** MUST NOT reveal the preimage while locked (ERR-4).
- `POST /transfer/paymenthash/external` `{statechain_id, auth_sig, batch_id, payment_hash}` — bind
  a latch to an EXTERNAL 32-byte hash (BOLT11). **REQ-11** MUST validate `payment_hash` is 32-byte
  hex and the auth signature.
- `POST /transfer/unlock/preimage` `{batch_id, preimage}` — unlock the batch iff
  `sha256(preimage)` equals the stored external hash. **REQ-12** MUST reject a non-matching
  preimage (ERR-5); on match, MUST unlock every coin in the batch (sender-side confirm).

### 3.6 Spend budget (terminal nodes)
- `POST /statechain/spend_budget` `{statechain_id, auth_sig, remaining∈{0,1}}` → `{sig_budget}` —
  owner-signed; sets an absolute co-signature ceiling. **REQ-13** MUST reject `remaining ∉ {0,1}`
  and a bad auth signature. Irreversible (budget only tightens).
- `GET /statechain/spend_budget/<id>` → `{sig_budget, finalized, terminal}` — public;
  `terminal = budget set ∧ finalized ≥ budget`.

---

## 4. Deposit

**Flow.** `get_deposit_address(amount)` → `/deposit/init/pod` → P2TR aggregate address. Owner funds it.
The background watcher detects the UTXO (`update_coins`), creates the first backup tx (`create_tx1`, locktime
`h+initlock`) IMMEDIATELY on first sight — in the `INITIALISED` block, as the coin flips to
`IN_MEMPOOL`, and skipped only for a `single_use` slot — then in a SEPARATE, later block counts
confirmations, flips the coin to `UNCONFIRMED` and finally `CONFIRMED`, emits `DepositConfirmed`, and
establishes the coin's TES-R ladder (§2.6) with `LadderEstablished`.

> **The order is load-bearing.** The backup exists from the mempool sighting, NOT from confirmation,
> so a deposit that never confirms still leaves a signed backup — and the ladder, which does wait for
> `CONFIRMED`, is the only part gated on confirmations.

**REQ-14** A deposit slot MUST consume a deposit token; if payment is required the SDK MUST surface
`SdkError::TokenPaymentRequired` rather than silently proceeding (ERR-6).
**REQ-37 (ladder establishment)** `claim()` MUST establish a TES-R ladder for every CONFIRMED,
non-duplicate, non-`single_use` ROOT coin that has none — unconditionally, and idempotently (a coin
that already carries a ladder is skipped, so repeated passes never double-sign). The exit payee MUST
be the coin's own seed-derived `backup_address`, never an out-of-wallet address. Two exclusions are
BY DESIGN, not leftovers: an RGB **carrier** is excluded from the PLAIN ladder and takes the coloured
one instead (INV-29), and a coin whose funding `F` is not on-chain is excluded outright (a sub-coin's
trigger would have no prevout to spend, leaving it unexitable). Both exclusions
MUST fail CLOSED: if the carrier set or `F`'s on-chain status cannot be resolved, skip the pass and
retry next claim — a missed ladder is harmless, a PLAIN-laddered carrier or a laddered sub-coin is
not. An establishment failure leaves the coin WITHOUT a ladder and still exitable by its signed-once
backup — which since the plain-split lane retired is a coin to repair rather than a coin to route
(§0.3).
`sdk48` (auto-established, exits to the seed-derived key, a second `claim()` does not
double-establish), `sdk52` (carrier never PLAIN-laddered — read INV-29's citation caveat with it).
**REQ-35 (derived slots)** A slot minted by an SE-co-signed flow over an existing statechain — an
off-chain split piece/change, a `transfer_many` recipient/change, a combine output, a refresh
re-anchor — is a **derived slot**: it re-houses value already inside the SE, so the SDK MUST fund
it with a FREE derived token (`deposit/get_derived_token`, vouched by the parent statechain) and
MUST NOT draw on pooled/prepaid onboarding tokens (in a token-server deployment those cost the
onboarding fee — a 2-output split must not cost 2× it). The SE MUST gate issuance on (i) the
parent's CURRENT-owner auth (single-use nonce, consumed only on a valid signature), (ii) a
per-parent LIFETIME cap (`max_derived_tokens_per_statechain`, default 64; 0 disables), and (iii)
the global outstanding-token cap, and MUST mark issued tokens with their parent
(`tokens.derived_from`). Fresh ON-CHAIN onboarding (a deposit address, a token-issuance carrier)
still consumes a normal token per REQ-14.

**Fallback is narrower than "when the allowance runs out".** `get_derived_tokens` returns the
fallback signal (`Ok(None)`) for exactly two statuses: `NOT_FOUND` (route absent) and `FORBIDDEN`
(issuance disabled, `cap == 0`); only in those two does the SDK fall back to onboarding tokens. The
per-parent lifetime cap answers `TOO_MANY_REQUESTS`, which becomes an `Err` — so a wallet that
exhausts its allowance FAILS the deposit rather than silently paying for an onboarding token.

The blind SE cannot verify how a slot is later funded (TRUST-MODEL §7 records the residual).
**INV-7** After a deposit confirms, `get_balance().available_sats` increases by the deposit amount.

---

## 5. Transfer (sats)

**Flow.** Sender: pre-sign everything first (the receiver's backup at locktime = previous −
interval, and — on the laddered lane — the receiver-paying state `S'`), then `/transfer/sender` (get
`x1`, which OPENS the transfer and arms the pending lock, REQ-36), then
`create_transfer_update_msg[_with_branch]` → `/transfer/update_msg`. Receiver (async): fetch
messages, validate, `/transfer/receiver` (SE rotates its share). The aggregate key `A` and the
funding UTXO `F` are INVARIANT across the rotation — that is what keeps the pre-signed exit material
valid for the new owner while locking the old one out (`sdk41`).

**Laddered lane (Model A).** A whole-coin handover of a laddered coin additionally conveys the
ladder (`tesr_ladder`): the sender co-signs the receiver-paying state `S'` one δ BELOW the LOWEST
rival over the current extension's payload output — its own live state, every disclosed superseded
state and every still-outstanding conveyed state, not merely its own retained one
(`next_rival_state_csv`; at the `d_floor` the call REFUSES and the coin must be renewed, rolled over
or re-anchored) — so the receiver out-races all of them (INV-28), and discloses the state it
supersedes. The receiver runs `verify_bundle` — the census (REQ-38) — and rejects unless the final
state pays the RECEIVER's own seed-derived key. `sdk47`, `sdk49`.

`TransferMsg.protocol_version` is a **message-shape tag, not a protocol version of the system**
(there is one protocol): `0` = branch/backup message (a coin conveyed with no ladder), `2` = a
conveyed TES-R ladder,
`4` = a split-child bundle carrying the key handover. `3` — a child conveyance with no key handover
— is not admissible (`ADMISSIBLE_PROTOCOL_VERSIONS = [0, 2, 4]`). The receiver dispatches its
validation on this tag.

**REQ-15** `transfer(address, amount)` MUST move exactly `amount`: either an exact subset of coins
(§5.1) or an off-chain split minting the exact piece (§6). No dust or overpayment.
**REQ-16** The receiver MUST validate: transfer signature binds the coin to its key; tx0/branch is
valid; latest backup pays the receiver; backup locktimes decrement correctly (INV-5, enforced on
BOTH lanes); and the co-signature count reconciles — `num_sigs == backups` on a coin conveyed FLAT,
the census (REQ-38) on a laddered one.
**REQ-17 (G1)** For a branch-carrying (sub-coin) transfer, the receiver MUST verify every
`terminal_parents` ancestor is terminal at the SE (`GET spend_budget`, `terminal==true`) and
reject otherwise (ERR-7). That endpoint is the COORDINATOR's own answer, computed from its Postgres;
this branch-carrying lane still rests on it (`verify_terminal_parents`). On the laddered/child lane
terminality is instead derived from the enclave-signed `(num_sigs, sig_budget)` payload
(`budget exists ∧ num_sigs ≥ budget`, `attested_terminal`), and the coordinator's answer is kept
only as a cross-check that REFUSES on disagreement — the two stores hold the same absolute quantity,
so a mismatch means one was written behind the other's back.
**REQ-38 (census)** A receiver of a laddered coin, or of a split child (§6.3), MUST reject unless
the SE's ATTESTED co-signature count (§3.3 — an unattested count MUST be refused, since the census
rests entirely on it) equals EXACTLY the tiers it was shown:
`se_num_sigs == flat_backups + Σ conveyed tiers + Σ disclosed superseded tiers`, summed over every hop
of the conveyed ancestor chain (N-hop for a re-transferred child). Each disclosed superseded tier
MUST be parsed, linked to the ladder, signature-checked, and carry a strictly HIGHER CSV than the
tier that replaces it — a `.len()`-only count is paddable, and an unparsed `csv: None` skips the
race check. Any hidden co-signed state shows up as a count mismatch and MUST reject (ERR-15). The
count is retry-safe: a repeated `sign/second` returns the cached partial signature and does NOT
advance `sig_count` (`sdk56`), so an in-flight retry cannot brick the equation. Verified by `sdk46`
(count formula against the real SE), `sdk54`/`sdk55` (padding/spoof attacks REJECT), `sdk58`
(12 child-bundle attacks REJECT).
**INV-8** Claiming is idempotent: repeated `claim()` passes book each transfer at most once.

### 5.1 Coin selection
`select::plan(coins, target)` returns `Exact(subset)` if a subset sums to `target`, else
`WithSplit{whole, split, split_amount}`, else `Insufficient{available}`.
**INV-9** `Exact(s)` ⟹ `Σ coins[s] = target`. `WithSplit` ⟹ `Σ whole < target ∧ split_amount =
target − Σ whole ∧ coins[split] > split_amount`. `Insufficient` ⟸ `Σ coins < target` — but the
reverse does **not** hold: the planner also returns `Insufficient` when the
remainder can only be minted as an unviable piece (no split candidate covers
`remainder + fee_reserve + min_split_output`, where `min_split_output = 330` (dust) `+` the sub-coin's
own backup fee at the live rate = `330 + ceil(112 · fee_rate)`; the planner also requires
`remainder ≥ min_split_output` so the minted piece can fund its own backup —
`select::plan_with_floor`, `transfer::min_split_output`). That fee arithmetic describes the COLOURED
branch split (§6.2) — the plain off-chain split it also used to describe is deleted, and
`split_amounts_floored` survives it as the executable dust-boundary spec, because the boundary was
never un-laddered-only: every split leg still has to clear dust and fund what it owes. The in-ladder
split has its own model, §6.1.

**Executor floor (laddered parent).** `min_split_output` is a TERM in the planner's floor, not the
floor itself. `plan_payment` resolves a `ParentShape` per candidate and hands
`select::plan_with_floor` the MINIMUM over those shapes of `SplitFloors::planning()` (itself
`piece.min(change)`), so for a laddered root the number the planner actually uses is
`max(min_split_output, min_spine_tip_value)` = **945** at the shipped rate — never the bare dust
default. When the chosen parent is laddered, the in-ladder executor applies a second, strictly
larger floor and the larger of the two binds: a child funds its OWN extension + state tier before it
can clear dust, so `min_child_value = 2·(committed_fee(rate) + 240) + 330` — **1 560 sat** at the
shipped `committed_fee_rate = 3.0` (`2·(375 + 240) + 330`).

**The floor is a function of the RATE — quoting one of these numbers without its rate is quoting a
rate.** The two legs are floored INDEPENDENTLY and by LANE — `SplitFloors { piece, change }`, not one
number for both. The piece always funds two rungs, so its floor is
`max(min_split_output, min_child_value)` = **1 560** at the shipped 3.0 rate. The change's floor is
whatever `change_leg_role(lane)` says that lane's builder gives it: `SpineTip` on the plain-root,
spine-batch AND coloured lanes — one rung, `min_spine_tip_value` = **945** — and `Piece` only on the
plain-CHILD lane. Either leg falling short refuses the split UP-FRONT (ERR-16), naming that leg's own
floor. Up-front is load-bearing: `establish_child` runs AFTER the parent's spend budget is consumed
and `SP` is co-signed, so admitting a child below the floor terminalizes the parent and THEN fails,
stranding it to unilateral-exit-only.

---

### 5.2 The transfer mailbox

A conveyance travels as an ECIES ciphertext in a coordinator-held mailbox, addressed to the
receiver's auth key. The coordinator is therefore in the path of every payment, and the specification
must say exactly what that buys it. Adversary by adversary:

| | class | coordinator acting ALONE can | verdict |
|---|---|---|---|
| **M-1** | withholding | delay a conveyance indefinitely | **denial only** — the read is non-destructive, so a later poll still gets the message. One escalation is worth naming: withholding past the coin's epoch expiry converts a delay into a PERMANENT failure, because admission requires the exit walk to fit inside the epoch the payee inherits |
| **M-2** | deletion | destroy the message | **denial only.** The coin stays the sender's. The LOSS arm — "the sender then re-conveys to a second payee" — is NOT coordinator-alone: the cancel that frees the coin needs a single-use, endpoint-bound signature under the SENDER's auth key. Coordinator + sender, the same adversary as L-7 |
| **M-3** | reordering | serve messages in any order | **no ordering dependence.** A message is bound to a coin by KEYS, not by mailbox position: the ciphertext is ECIES to the auth key and the transfer signature commits to `(tx0_txid, tx0_vout, new_user_pubkey)`. A misrouted message fails validation and the loop moves on, costing a wasted pass |
| **M-4** | duplication / replay | serve one ciphertext twice | **refused BY NAME.** See REQ-45 |
| **M-5** | cross-addressed injection | serve a message addressed to someone else | **fail-closed** — ECIES decryption fails under this coin's auth key. Nothing further is relied on |
| **M-6** | serve-then-renege around an irreversible leg | serve a valid message to a pre-pay census and then withhold at claim time | **real loss, Lightning lane only.** See §8 |

**REQ-45 A receiver MUST refuse a conveyance of a `statechain_id` it has already adopted, by
name.** "Already adopted" means a coin row this wallet still holds and can spend
(`IN_MEMPOOL`/`UNCONFIRMED`/`CONFIRMED`/`WITHDRAWING`). It deliberately does NOT include
`TRANSFERRED` — sending a coin away and receiving it back later is legitimate — nor `IN_TRANSFER`,
which is the sender's own row during a SELF-transfer, where the receiving slot in the same wallet
must still be able to adopt.

The reason this is a requirement and not an observation: a duplicate takes the same path as an honest
re-serve, and every check that binds the message to the coin passes. Without the by-name refusal,
what rejects a replay is `validate_tx0_output_pubkey` failing because a completed handover has
rotated the SE's share — a CONSEQUENCE of an unrelated subsystem, not a rule. **A protection that
holds only because something else rotates a key is not a protection a specification can state.**

**REQ-46 A wallet's balance MUST be a function of DISTINCT statechain ids, not of rows.** A
second live row under one id is one coin counted twice. This does not make a replay safe — REQ-45
does that — it removes the SILENT failure mode: without it, a lapse upstream shows up as spendable
value that is not there, and a merchant crediting on the balance over-credits with nothing in the
log. With it, the same lapse shows up as a coin that fails to arrive.

### 5.3 Sweep at claim — absorbing leaves out of circulation

> **Status: THE DECISION IS BUILT; THE ABSORPTION IS NOT.** The sweep's arithmetic — the four
> admission limits (REQ-50), the settlement trigger (REQ-51) and the fairness floor (REQ-52) — is
> built as pure functions with the derived defaults, and every boundary is pinned by unit tests,
> including the ones a live stack reaches only by luck. **Nothing calls them.** No absorption path
> exists, and REQ-49 requires the swap default-OFF until the cooperative exit it depends on is
> demonstrated end to end, which it has not been.
>
> That order is deliberate: shipping the decision without the mechanism leaves an operator unable to
> absorb, which costs nothing; shipping the mechanism first leaves one holding leaves it cannot
> settle. Build order for the remainder is in
> [PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md) §0.7.

The sweep is an **optimisation, not the settlement path**: it is how a leaf is absorbed cheaply.
Settling leaves one at a time buys one P2TR input each and cannot beat ~1.5× an ordinary on-chain
payment, so the sweep does not by itself make the economics work — a CLOSE (§5.4.4) is what retires a
whole tree for one transaction. Read §5.4 with this section.

**This paragraph said "an optimisation inside the discharge round … during R1" until the round was
deleted.** The sweep does not depend on a round: absorption happens whenever a leaf is absorbed, and
the thing that retires a tree cheaply is now a close triggered by its root owner, not a scheduled
migration.

**A leaf is a worse coin than a root in every respect** — it inherits a deadline it does not control,
carries depth, has no one-transaction cooperative exit, and burns 1 230 sat of its own value if it is
ever walked out. The sweep replaces it, at the moment it is first seen, with an ordinary root coin.

Full derivation, parameters and build order in
[PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md) §0.7. Normative requirements:

**REQ-49 (sweep point) The swap MUST happen in `claim()`, not in a background pass.** At claim the
runway is maximal, the payee is online because they are already transacting, and no extra
coordination round exists. A payee whose leaf is swept receives a root coin and never handles a leaf.
It MUST be default-OFF until the cooperative exit it depends on is demonstrated end to end.

**REQ-50 (absorption predicate) A leaf MUST NOT be absorbed unless ALL of:**

| | default | why |
|---|---|---|
| `market_fee_rate ≤ sweep_max_fee_rate` | 15 sat/vB | the surplus `1 230 − 57.75·m` reaches zero at 21.3; above the ceiling the payee is better off walking on prepaid tiers |
| `runway_blocks ≥ sweep_min_runway` | 903 | `e_csv + confirmations` = 723, +25 %. Below it the leaf CANNOT be settled — absorbing it buys a liability |
| `leaf_value ≤ sweep_max_leaf_value` | 100 000 sat | the surplus is CONSTANT in face, so past this the operator adds balance-sheet risk without adding return |
| `tree_exposure + leaf_value ≤ sweep_max_tree_exposure` | 1 000 000 sat | bounds the loss if one tree's spine cannot be materialised |

All four are configuration, not protocol constants. The defaults are derived, not chosen — each cell
cites its derivation.

**REQ-51 (settlement timing) The holder of absorbed leaves MUST settle when EITHER the batch reaches
`sweep_target_batch` (default 10) with the market at or under the ceiling, OR the earliest inherited
deadline comes within `sweep_min_runway` — and the second path MUST ignore the fee ceiling.**
The risk is asymmetric: settling early forfeits a few hundred sat of batching, settling late voids
the leaf entirely for its full face. An expensive settlement beats a voided one at every fee rate.

**REQ-52 (fairness) A swap MUST leave the payee no worse off than walking the leaf out:
`price_paid ≥ leaf_value − 1 230`.** The payee additionally receives a coin that is strictly better in
kind. A swap priced below that floor takes value from a payee who would have done better alone, which
is the one outcome that makes this a tax rather than a service. The operator's share
(`sweep_spread_bps`) is policy and MUST be disclosed in aggregate.

**Why this is worth building at all:** it is not dust rescue. It is what holds §14.3's break-even at
~0.5 onward payments per recipient instead of ~1.65 — i.e. what keeps the design's block-space claim
true at realistic payment velocities.

---

### 5.4 The payment flow — whole leaves, exact fit, swap, and one close

**The shape of this system, stated positively before any of its history.**

* **A balance is a SET of leaves**, not a single coin with a running total.
* **A payment is a HANDOVER of whole leaves.** `child_retransfer` replaces a leaf's state tier at a
  strictly lower CSV over the SAME output — node id, funding outpoint and value unchanged. One SE
  co-signature, **zero new depth, zero on-chain bytes**. This is already built and exercised.
* **Selection is exact-fit** over the leaf set (`select::exact_subset`, `Plan::Exact`), so an ordinary
  payment never splits anything.
* **When no exact subset fits, the wallet SWAPS** with an SSP at strictly equal value — leaves in,
  leaves the SSP already holds out, no value created or destroyed and no capital at risk. Splitting is
  the fallback, not the mechanism.
* **Amounts below the dust limit ride as a TAIL** (§6.0), so any amount from 1 sat is expressible.
* **A tree CLOSES once**, when its root owner decides: one transaction paying every unreleased leaf its
  full value out of `F`, at any depth, with the un-broadcast splits simply discarded.

Budget: `(d0 − d_floor)/delta = 36` hops per extension epoch × `m_max + 1 = 16` epochs = **576
whole-leaf payments** per depth level, renewable off-chain at two co-signatures and zero depth. Spark's
comparable figure is ~330.

**There is no round, no epoch, no migration window and no absentee.** What follows records why, because
the round was load-bearing in earlier drafts and its removal is the largest change this document has
taken.

#### 5.4.0 Coin renewal — why there is no round

> **THE DISCHARGE ROUND IS RETIRED. OWNER DECISION, 2026-08-20.** The rule it violated is older than
> the round: **this protocol MUST NOT require operator liquidity.** Not "less than Ark", not "0.11 %
> with 900 staggered chains" — none. §5.5 derived a float of ≈ 9 % of TVL and then a way to shrink it;
> shrinking was the wrong answer. An architecture whose capital requirement must be engineered down is
> still an architecture with a capital requirement.
>
> **REQ-53, REQ-54, REQ-59, REQ-64 and the entire operator-liquidity derivation (REQ-70…REQ-75) are
> DELETED**, not marked retired — they described a scheduled migration this system does not perform, and
> leaving them in place was making the document harder to read than the change it recorded.
> **REQ-55…REQ-58, REQ-60…REQ-63 and REQ-65…REQ-68 SURVIVE** and are reproduced verbatim in §5.4.7:
> they govern the SE, the close, and what a holder is owed, and none of them needs a round to exist.
> REQ-53's substance — the closer must own the root — survives as REQ-82.

#### 5.4.1 What the round was actually for, and why nothing needs to replace it

The round batched **on-chain re-anchors**. A coin's exit material is ordered by a RELATIVE (BIP-68)
timelock that must decrease every time a rival state is minted, so the budget is finite; an exhausted
coin had to be re-anchored, and one collapse transaction retiring a whole tree was cheaper than one
transaction per coin. The float was the price of the batching: to migrate a holder onto a successor
root, that root had to be **confirmed first**, so the operator funded it before the old tree paid back.

**That is the ONLY thing the float traces to.** Remove the requirement that a funded successor root
exist before holders can move, and the operator's capital requirement is not small — it is **zero**.
There is no window to carry, because there is no root to pre-fund.

And nothing has to replace the batching, because **the budget it was batching does not need on-chain
work to renew.** Renewal is a re-signing round between the holder and the SE: no broadcast, no funding
UTXO, no fee input, no operator capital. The coin's timelock resets off-chain. When even that budget is
spent, the chain is extended by **one transaction of exit depth** rather than re-anchored.

#### 5.4.2 Measured against Spark, including a correction this document owes

**An earlier draft of this section said Spark achieves its lack of expiry through trusted key deletion
by its operators. That is FALSE, and the correction is kept rather than removed, because it is the
error most likely to be repeated by anyone comparing the two designs.** Spark uses a decrementing
relative timelock per transfer — structurally the same primitive as TES-R — and its own documentation
states that a previous owner *can* broadcast and that the current owner must win the race. Deletion is
not what protects the current owner, in Spark or here.

What Spark has that this design was not using is the off-chain renewal above, plus a **zero-timelock
prepend** when the node budget is exhausted, which buys an unbounded reset for one extra transaction of
exit depth. Their published constants give roughly **330 transfers per depth level**.

**This design's own budget is better: ≈ 576.** So the round was solving a problem this protocol has
less of than the system it was being compared against.

#### 5.4.3 The requirements that replace it

**REQ-76 (no scheduled on-chain re-anchor). GUARDED.** No coin may require an on-chain transaction on a
schedule, and no mechanism may require a funded successor output to exist before a holder can act.
Renewal MUST be off-chain: a re-signing round that resets the relative-timelock budget and moves no
value. An operator MUST NOT be required to hold capital for the correctness or the liveness of any
path in this document.

**REQ-77 (exhaustion extends depth; it does not re-anchor).** When a coin's relative-timelock budget is
spent, the exit chain MUST be extended by prepending a transaction at timelock ZERO rather than by
moving the coin on chain. The prepend is what makes the extension safe: **it fires ahead of every rival
state in existence**, all of which carry a non-zero timelock, so after it confirms every prior rival is
spending an output that no longer exists.

**PROVEN — measured against Bitcoin Core 30.2, not reasoned about** (`scripts/prepend_precedence_probe.py`).
One funding output, three questions, and the claim is exactly the three answers:

| offered to the node | verdict |
|---|---|
| RIVAL spend carrying a relative timelock (`nSequence = 10`), as every superseded state does | refused — **`non-BIP68-final`** |
| PREPEND at no relative lock | **`allowed = true`** |
| the same RIVAL, after the prepend confirmed | refused — **`missing-inputs`** |

The first two lines are the precedence: a superseded state cannot even enter the mempool while the
prepend goes straight in. The third is what the precedence BUYS — the rival is now spending an output
that no longer exists. §0.2 is the reason this had to be run rather than read: "fires ahead of" is a
statement about what a node does, and no amount of reading policy source settles it.

**THE PREPEND IS NOT UNBOUNDED, AND AN EARLIER DRAFT OF THIS REQUIREMENT IMPLIED IT WAS.** Each prepend
adds one transaction to the exit chain, and the chain is capped: `max_split_depth` and `max_exit_txs`
derive their bound from exit LATENCY — a chain is admitted only while `exit_wait_blocks +
exit_slack_margin` still fits the epoch — so depth is limited by how long a unilateral exit may take,
not by taste. When the cap is reached, prepending STOPS.

**What happens then is a re-anchor, and it is not an exit.** The coin is refreshed: ONE transaction,
single-input, the fee drawn from the coin, paid by whoever holds it, and the ladder resets completely.
The coin continues. Nothing is forced on chain, no position is closed, and — the point of this section
— **no operator capital is involved at any step**. Conflating this with a unilateral exit, as an earlier
draft's wording invited, is wrong in the direction that matters: a unilateral exit is the disaster path
and is never the answer to an exhausted budget.

**So the on-chain event is made RARER by REQ-77, not eliminated.** State it that way. The block-space
argument for this design has never rested on the round: it rests on transfers being free, so the
amortisation is one re-anchor per many transfers. The round only ever batched that rare re-anchor, and
it bought the batching with a standing float — the trade §5.5 now rejects.

**How REQ-76, REQ-80 and REQ-81 are enforced.** All three are PROHIBITIONS, and §0.2's evidence rule
makes a source scan the right instrument for one: a scan establishes presence, absence and ordering,
and "this does not exist" is an absence. `deny_round_shaped_mechanisms` holds four of them — the zero
claim never travels without its mechanism, it is never extended to the Lightning legs, no client path
names `collapse_grant` (the background maintenance pass is where a calendar would acquire the power to
close a tree), and no paragraph reintroduces the confirmed-successor ordering that the entire float
traced to. Two of the four fired on their first run against defects in the GUARDS rather than the
document, and one fired on the §5.5 heading, which now carries its mechanism rather than being
exempted — the heading is the line a reader quotes.

**REQ-78 (the retired calendar was also the garbage collector — name what replaces it).** The absolute
locktime chain did more than mark a deadline: a refresh (REQ-31) permanently invalidated every previous
owner's backup and every old tier, which is why rival states did not accumulate without bound. Deleting
the calendar deletes that collection. REQ-77's prepend is the replacement, **and the third row of REQ-77's
probe is that replacement working**: once the prepend confirms, every prior rival is refused
`missing-inputs` — collected, not merely superseded. Any design that adopts REQ-76 without answering
this has moved the exhaustion problem rather than solved it; this one answers it, measured. The rival set the
budget is consumed against is **total** — live tier plus every superseded state plus every outstanding
conveyed state — not merely the live one.

**REQ-79 (quote the SHIPPED depth constants, not the drafted ones). BUILT AND PINNED.** The mainnet
split depth is **8** and the exit-transaction cap is **19** (`3 + 2·depth`), lowered by D53. An earlier
costing of this replacement used 10 and 23.

**Both are now DERIVED from the shipped schedule by a test rather than retyped from a draft**
(`req79_shipped_budget`, two cases: the caps, and the budget). Measured there, on
`TesrParams::mainnet()` at the shipped `lockheight_init = 10000`:

| quantity | derivation | value |
|---|---|---|
| flat hops per extension epoch | `(d0 − d_floor) / delta` = `(1440 − 144) / 36` | **36** |
| usable extension rungs | `m_max + 1` | **16** |
| whole-leaf payments per depth level | `36 × 16` | **576** |
| split depth | `max_split_depth` | **8** |
| exit-transaction cap | `3 + 2 · 8` | **19** |

**The ~6× overstatement was not in the 576 — it was in multiplying two different axes.** Depth and the
hop budget are separate quantities: a coin does not get 576 hops *at each of* 8 levels. Depth is spent
by in-ladder splits AND by rollover levels, from one shared allowance. Any figure quoting them
together MUST say which axis it is on, and the test asserts they are not multiplied.

A stale constant in a doc comment is how a drafted number gets requoted as measured: `max_exit_txs`
carried "23 on mainnet" long after D53 made it 19, and that comment is fixed with this requirement.

#### 5.4.4 Consolidation — how a tree actually closes, and why it is not a round

The round had TWO jobs and §5.4.1 named only one. Batching re-anchors was the first. The second was
making **complete ownership schedulable**: giving the holder of a partial claim a way out that is not
"broadcast your branch and drag every ancestor on chain with it". Retiring the round without answering
that leaves a hole, and this subsection is the answer.

**A tree closes when its ROOT OWNER decides to close it, and it costs one transaction.** The mechanism
is the collapse transaction and its predicate, which are built: `C` spends the root's funding output
`F`, pays **every unreleased frontier leaf its full funding value at its own exit key**, and pays the
remainder to the root owner. Leaves whose holders have released are owed nothing. **The payouts come
out of `F` itself — the tree's own money — so the closer fronts NOTHING.**

Depth is irrelevant to this. The un-broadcast split transactions are simply discarded: nobody has to
broadcast them, because nobody is in dispute about who owns what. A tree ten levels deep closes in the
same single transaction as a flat one.

**Why this is not the round, restated because the transaction is the same one.** The round's float came
from `out[0]`, the successor root, which had to be **confirmed before** holders could migrate onto it.
Remove the migration and the successor root goes with it, and all three terms of the float —
participation, window, epoch — have nothing left to multiply. What remains is a single spend of a UTXO
by its owner, settling with the co-owners who did not sell. **The on-chain footprint is unchanged**
(`155 + 43·N` for `N` unreleased leaves); what changed is that no capital has to exist in advance.

**INV-FREEZE (the freeze now GATES, and until this it gated nothing). BUILT AND TESTED.** A frozen
root MUST admit no new leaf. `collapse_grant` set `frozen` and `is_root_frozen` was defined and called
from **nowhere**, so the flag was inert: a leaf could still be observed after the frontier was computed
and before `C` confirmed, and that leaf would be owed value by a transaction already signed without an
output for it — a holder discharged without being paid, the single outcome the predicate exists to
prevent. `observe_leaf` now checks the freeze **inside its own transaction**, against the root the leaf
would join, and fails CLOSED: an unreadable flag refuses, because "I could not tell whether the tree is
closing" is not permission to join it. Pinned by `test_registry_db` (a leaf joins before the freeze and
is refused after, the refusal NAMES the freeze, the pre-existing leaf survives, and an unfrozen root
still admits leaves so a gate that refused everything could not pass).

**THE GRANT NOW SIGNS, AND SIGNS ATOMICALLY WITH THE FREEZE (#169). BUILT.** `collapse_grant`
returned `partial_sig: null` and said so; it now issues the partial signature, and issues it in the
SAME database transaction that sets `frozen` (`freeze_root_and_store_collapse_sig`). Neither half is
safe alone: sign first and a leaf may still join a tree whose collapse is already signed without an
output for it; freeze first and a failed signature seals the tree with nothing payable.

Two properties the route now enforces, both observable from outside via `collapse_grant_probe.py`:

* **The verdict is COMPLETE before anything is signed.** Every refusal — underpayment, wrong funding
  outpoint, an omitted leaf, a named-but-underfunded next root (REQ-74) — fires `403` on its own gate
  before the signing step is reached. The probe is a differential: each case differs from the granted
  one in exactly one respect, so a refusal cannot be explained by anything else.
* **No signature for a transaction that is not this root's (REQ-68).** The predicate proves `C` pays
  everyone and the bind proves the session reproduces `C` — but both take every input from the caller,
  so neither says the transaction belongs to this root. The grant now checks the disclosed aggregate
  against the one the SE derived at keygen, **before the secnonce is consumed**, so a refusal costs the
  root nothing. Without it a stranger could burn a root's nonce and leave it unable to sign its own
  collapse. This is REQ-82's teeth: the SE issues only its half, and it issues that half only for the
  coin it belongs to.
* **No signature without a bound session (REQ-57).** The session must reproduce the disclosed
  transaction, or the grant refuses `400`. Otherwise the SE would verify the predicate over one
  transaction and sign another — the check defeated at its last step. The secnonce is loaded and
  consumed atomically, so a second grant over a different session cannot reuse it.

**REQ-81 (a close is an owner's operation, never a schedule). GUARDED.** Closing a tree MUST be triggered by its
root owner's decision, never by a calendar, an epoch or a deadline. No holder may be compelled to
migrate, and a holder who does nothing MUST simply be paid their full funding value when the tree
closes. There is no window to miss and no absentee penalty; "absentee" ceases to be a category.

**REQ-82 (consolidation requires the ROOT, and this asymmetry MUST be priced).** `C` spends `F`, and
`F` is a 2-of-2 between the root owner and the SE. `collapse_grant` issues only the SE's half. **A party
that has bought every leaf but does NOT own the root cannot close the tree** — its only route to chain
is materialising a branch, the expensive path this section exists to avoid. Therefore:

* an SSP consolidating a tree it created itself (deposited and split, so it holds the root) can always
  close;
* an SSP buying leaves in a tree it does NOT own has **no exit but materialisation**, and MUST price
  that difference into what it pays;
* buying into a foreign tree SHOULD start with the root, not with the leaves.

**This was REQ-53's substance, and it is the reason REQ-53 was not simply deleted.** REQ-53 said a
round-managed root must have been deposited and split by the SSP. Read as "eligibility for a round" it
dies with the round. Read as "the closer must own the root" it is **load-bearing for the replacement**,
so it is restated here as REQ-82 rather than lost.

**NOT YET RUN.** Every claim in this subsection about behaviour — that discarding the un-broadcast
splits is clean at the SE, that the census stays balanced when a tree closes, that a partially-bought
tree closes for `155 + 43·N` — is derived from the predicate's code and has never been exercised
end to end. §0.2 applies: this is presence and ordering, not behaviour.

#### 5.4.5 Spark's denominated-leaf model — what it costs to adopt, NOT a reason to refuse it

**THE FRAMING OF THIS SUBSECTION WAS WRONG WHEN FIRST WRITTEN, AND THE CORRECTION IS THE POINT.** It
was recorded as a "negative result: Spark's model does not port". The owner rejected that reasoning in
one line: *how can you reject Spark's path when it works and ours does not — if we reject it we must
propose something as good or better.* That standard is correct and it is now binding on this document.

Every reason the first draft gave was a statement about OUR structure being incompatible, not about
Spark's design being unsound — and our structure is the part that does not work. Rejecting a working
design for failing to fit a broken one is defending the status quo, not analysing it. Each item below
is therefore restated as **a price**, with what changing it would take:

Recorded so nobody walks this road twice. Verified against Spark's own source, 2026-08-20.

**What Spark does.** Denominations are literally powers of two **from 1 sat**. A balance is a SET of
leaves. Paying is greedy EXACT-FIT selection with no remainder and no split. When no exact fit exists
the wallet SWAPS with the SSP at strictly equal value (`sum(in) == sum(out)`, fee zero), receiving
leaves the SSP already owns, atomic by adaptor signature. And decisively: **a Spark transfer does not
create a tree node** — the node id, value, tx and vout are unchanged and only the refund transaction is
replaced. Depth does not grow with payment history.

**What adopting it would cost us — three prices, none of them a veto:**

1. **Our leaf floor is ~1560 sat, not 1 sat**, because `min_child_value` makes a UTEXO leaf PREPAY ITS
   OWN EXIT. That is OUR design choice and it is changeable — a leaf that does not prepay its exit
   would have a floor near theirs, at the cost they already pay (their sub-16 348-sat leaves cannot
   exit unilaterally). A Spark leaf does not, and Spark concedes the consequence: leaves below ~16 348 sat
   cannot be unilaterally exited at all. A power-of-two grid starting at our floor spans only
   multiples of that floor, so essentially every real payment amount is off-grid and carves anyway.
   **This is our property, not our defect** — our leaf can always exit and theirs cannot — but it is
   what closes their grid to us.
2. **Their SSP can mint denominations for free and ours cannot — YET.** Spark's SSP alone may split
   leaves off-chain through an operator-internal service. Ours has no such privilege because nobody
   built one, not because one is impossible. Giving the SSP a privileged split is a design option with
   a price to state, not a closed door. Our swap could therefore only PERMUTE existing denominations, never create
   the one a payment needs. **The inventory engine their swap runs on does not exist here.**
3. **Spark did not solve the ancestor drag either.** Their unilateral exit walks the parent chain to
   the root and broadcasts every ancestor — exactly what `materialise_carrier` does. Their exit cost
   is dominated by LEAF COUNT rather than depth, which is a different trade, not a solution.

**THE STANDARD THIS SECTION IS NOW HELD TO.** Any proposal to NOT adopt Spark's model must deliver
partial-holder liquidity that is as good or better, and must say so in those terms. "It does not fit
our current structure" is not an argument; it is a description of the thing being fixed.

**What survives and is worth building regardless:** carve WIDE, never narrow. Width does not enter the exit-depth
cap at all, and `combine_leaves` already exists and has been exercised. The realistic gain is about
**2×**, from spine levels being cheaper — not the ~60× a denomination grid appeared to promise, and it
needs none of that machinery.

#### 5.4.7 Requirements that survive the round's deletion

Everything the discharge round introduced has been DELETED — the R0–R9 sequence, round eligibility,
the grant's re-grantability, the no-suspend rule, and the whole operator-liquidity derivation. What
follows are the requirements that were written inside that section but are NOT about a round: they
govern the SE, the close, and what a holder is owed. They are reproduced verbatim, with the word
"round" now meaning **a close** and "migration" meaning **an ordinary payment**.

**REQ-55 (no third-party input).** `C` MUST have exactly one input. A depositor's signature MUST
never be required to complete a round — otherwise any depositor can stall every round by going quiet.

**REQ-56 (THE SE PREDICATE — the load-bearing rule).** The SE MUST refuse `collapse_grant` unless,
for the **frontier** of the root (every node that is not the parent of another), every node not marked
`released` is paid **its full funding value** to **its own `exit_key`**, in outputs distinct per key:

```python
def collapse_grant(root_sid, disclosure):
    T  = tree[root_sid]                          # absent => REFUSE (fail closed)
    tx = witness_bind(disclosure, disclosure.session)     # INV-W, below
    REQUIRE len(tx.vin) == 1
    REQUIRE tx.vin[0].prevout in {(T.fund_txid, T.fund_vout),
                                  (T.trigger_txid, PAYLOAD_VOUT)}   # R8 branch
    owed = defaultdict(int)
    for n in frontier(root_sid):
        if n.released: continue                  # form (a) / (b)
        owed[n.exit_key] += n.fund_value         # INV-P: FULL un-burned value
    used = set()
    for key, amount in owed.items():             # INV-Q: distinct outputs per key
        got = sum(o.value for i, o in enumerate(tx.vout)
                  if i not in used and o.spk == p2tr_spk(key) and not used.add(i))
        REQUIRE got >= amount
    T.frozen = True                              # INV-FREEZE: prospective, irreversible
    return partial_signature(root_sid, disclosure)
```

**REQ-57 (witness binding, INV-W).** The SE MUST reconstruct the BIP-341 key-path sighash from the
disclosed transaction and byte-compare the resulting blinded session against the session it was asked
to sign; on mismatch it MUST return `400` **without consuming the secnonce**. This is what makes the
disclosure non-lying: BIP-341 commits the prevout amount, so a false value yields a signature that
does not verify against the real UTXO.

**REQ-58 (what the predicate MUST NOT do).** It MUST NOT ask whether `F` exists, is funded, or is
unspent — **not because the SE is incapable of looking, but because nothing it learned by looking
would be trustworthy to an offline holder.** The SE runs in an operator-controlled container on an
operator-controlled network (it already makes outbound HTTPS calls via `cpr`,
`HashicorpApiKeyManager`, `lockbox/src/hashicorp_api_key_manager.cpp`), so an operator-chosen chain
endpoint reduces "the SE verified it" to "the SSP says so".

Form (c) needs only the output vector of a transaction the SE verified byte-for-byte; form (b) needs
only facts the SE authored about a root **the holder verified themselves while online**. Neither asks
the SE for knowledge it cannot honestly hold.

**And existence is not the binding constraint anyway.** Even granting a perfect existence oracle —
e.g. making the successor root `C.vout[0]`, so it exists by the SE's own signature — an offline
holder still cannot be migrated, for a reason no oracle touches: **ownership moves by a key rotation
only the receiver can drive.** `calculate_t2` returns `−k_receiver + t1`
(`calculate_t2`, `lib/src/transfer/receiver.rs`) with `t1` minted per-transfer, so `t2` cannot be
pre-computed and the SE never learns `k_receiver`. Offline, either the SSP completes the rotation or
the holder surrendered the key in advance — **both are custody**. A leaf adopted without the handover
simply *is* `protocol_version = 3`, which this codebase does not admit
(`ADMISSIBLE_PROTOCOL_VERSIONS = [0, 2, 4]`, `clients/libs/rust/src/transfer_receiver.rs`).
**Any proposal to revive offline succession MUST say in those words that it reinstates v3.**

**REQ-61 (offline payee — zero).** A payee MUST NOT need to be online, reachable, or on a clock to be
paid. The mechanism is the **owner latch**: a key read from the money itself, write-once, after which
co-signatures under that sid require a fresh BIP-340 by that key. The latch is the primary defence;
the coordinator's one-hour `OPEN_TRANSFER_WINDOW_SQL` (`server/src/database/transfer_sender.rs`) is
defence in depth only, and its comment MUST say so.

Three corrections to earlier drafts of this requirement, each forced by a MEASURED fact about the
code. All three were wrong in ways that would have produced a latch that looks right and protects
nobody.

**(a) The latch key MUST be derived STRUCTURALLY, never at a client-supplied index.** An earlier
draft said `latch_key := xonly(state_child.vout[0].spk)`. The code has no such constant: the payload
output is reached through `TesrTier::payload_vout`, and that field is **attacker-supplied in every
conveyed bundle** — the file says so itself (`clients/libs/rust/src/tesr.rs`, at the one place the
index is turned into an output). A latch read at an index the attacker chooses is a latch whose key
the attacker chooses. The SE MUST instead identify the payload output by a structural property it can
check alone: **the unique output whose scriptPubKey is P2TR** (`OP_1 <32-byte>`). On an uncoloured
tier the only other output is the P2A anchor, whose script is `OP_1 <2-byte>` and therefore not P2TR,
so the payload output is unique and the index is derived rather than trusted. If a tier does not have
exactly one P2TR output, the SE MUST refuse rather than pick one.

**(a2) NOT EVERY TIER CARRIES A LATCHABLE KEY, and arming from the wrong one BRICKS the coin.**
Of the tier builders in `lib/src/tesr.rs`, four pay `to_address` — the coin's own **aggregate**
(2-of-2) address — and only the state tiers pay `owner_address`, the holder's **unilateral backup
key**. A latch armed to the aggregate can never be satisfied: signing under it requires the SE,
which is the very thing the latch gates. Since the latch is write-once, such a coin is
**permanently unable to be co-signed** the moment enforcement is switched on.

The SE MUST therefore arm only from a tier that hands control OUTWARD, and it can tell without
being told: **a tier that pays back to the key it spends is staying in the 2-of-2; a tier that pays
elsewhere is the one handing control to a unilateral owner.** The prevout is already in the
disclosure, so the rule is `arm iff payload_key != prevout_key` — structural, like REQ-61(a), and
requiring no client declaration of tier type.

MEASURED on a deposit+ladder: of 4 bound co-signatures, **3 pay back to the aggregate and 1 pays the
backup key** — and it is the latter that arms. Before this rule existed the latch armed from
whichever tier bound first and happened to be right by ordering alone; on a run where a trigger
bound first it would have captured the aggregate key and bricked the coin under enforcement.

**(b) The latch CANNOT bind every co-signature under the sid — it binds from establishment onward.**
As written ("every co-signature under that sid") the rule is unsatisfiable: a leaf's tiers are
co-signed by the **PAYER**, under the CHILD's sid, before the payee holds anything
(`cosign_tier` over `child_coins` in the conveyance builder, `clients/libs/rust/src/tesr.rs`). The
payee's key does not exist in the protocol at that moment, so it cannot have signed. The latch MUST
therefore be armed at establishment and enforced on every co-signature **after** the establishing
set, and the spec MUST state the exempt count rather than leaving it to an implementer to discover
that the obvious reading bricks conveyance.

**(c) This requires a capability the SE has never had.** `secp256k1_schnorrsig_verify` — and any
other verify — returns **zero hits anywhere under `lockbox/`** (measured). The lockbox signs; it has
never checked a signature, and all of its routes are unauthenticated. REQ-61 and R2 both therefore
depend on NEW SE code, not on wiring up something already present. There is also **no BIP-340
tagged-hash helper anywhere in `lib/`, `server/` or `clients/libs/`** (measured: no `sha256t`,
`hash_newtype`, `tag_engine` or `impl Tag for`; every existing domain separation is a plain SHA-256
prefix), so `tagged("utexo/leaf_release/v1", …)` in R2 is a primitive to be built and pinned by a
differential against BIP-340 test vectors — not a call to an existing function. Any plan that costs
REQ-61 as "add a check" is mis-costed.

**REQ-62 (offline payer — NOT ACHIEVABLE, stated so).** One payment is four irreversible SE
co-signatures over a transaction that did not exist before the payment was decided. It cannot be
pre-signed, because a pre-signed payment is a fixed-amount payment, and a fixed-amount instrument is
not an admissible mechanism here — payments are arbitrary amounts. **The spec MUST NOT claim offline
sending.** The one adjacent buildable thing is DELEGATED PAY — a holder, while online, signs
`utexo/deleg_pay/v1(parent_sid, payee_exit_key, max_value, expiry, nonce32)`, and the SE later enforces
from the witnessed transactions that the payee leaf pays exactly that key at `value <= max_value`.
The amount stays arbitrary. **This covers standing and recurring payments only. It MUST NOT be sold
as offline sending.**

**REQ-63 (the liveness that remains, stated honestly).** Four facts, tightest last:
1. **Hand-off: zero.** With the latch the payee needs no network, no clock, no deadline.
2. **First-class ownership: one online action, whenever they like.** Until `/transfer/unlock` +
   `/transfer/receiver` complete, the payee holds **exit-only** material — claimable, keyless,
   offline-exitable, but not re-transferable, renewable or splittable. There is no keyless claim
   delegation; a "claim agent" is a custodian. **This MUST be said in the product docs.**
3. **The epoch is the real bound.** A leaf carries no flat backup of its own
   (`CHILD_V2_BASELINE == 0`); its clock is inherited and nothing off-chain moves it. A holder who
   wants to stay off-chain MUST appear roughly once per round.
4. **`initlock` is the one real dial nobody used** (`TesrParams::flat_ladder_params`,
   `lib/src/tesr.rs`). Raising mainnet 10 000 → 52 560 multiplies parked lifetime by ~5× at zero
   on-chain cost, but first requires reconciling the gap between depth *admission* (which reads the
   full `lockheight_init` — `max_exit_txs`, `lib/src/transfer/receiver.rs`) and materialisability
   (which reads the remaining runway, same file).

**REQ-65 (an unclaimed payee is still paid).** A leaf's `exit_key` is recorded by the SE at
`establish_leaf` from the **witnessed payload output of the state tier** — identified structurally as
the unique P2TR output, per REQ-61(a), and NOT at a client-supplied index. That key is **the payee's**
because conveyance builds the child's tiers to the receiver's address. So a payee who was paid and
**never claimed at all** is nonetheless in the frontier, and REQ-56 forces `C` to pay them their full
funding value at their own key. Claiming is required to *spend*, never to *be paid*.

**What "witnessed" does and does NOT mean — corrected 2026-08-17 after measuring the code.**
An earlier draft of this paragraph said the SE reads `exit_key` and `fund_value` "out of a
transaction whose sighash it recomputed and whose session it byte-compared, so both are facts it
verified rather than fields a client asserted." The first half is true. The conclusion was too
strong, and the difference matters to exactly the party this requirement protects.

`witness::bind` takes **no statechain id and no coin key**. It rebuilds the session from the
`agg_pubkey`, `agg_nonce`, `blinding_factor` and `out_tweak` **the caller supplied**, and compares
against the session the caller sent. So binding establishes one thing: *the disclosed transaction,
hashed against the disclosed prevouts under the disclosed keys, produces the session you asked me to
sign.* Every input is the caller's. It is a self-consistency check.

That is genuinely worth having — it is what stops "sign session `S`, which is over this benign
transaction" when `S` is really over a different one, and `sdk92` measures it with a one-satoshi lie
that the session comparison refuses. **It does not establish that the transaction is a tier of the
coin whose sid was named.**

**And a blind SE structurally cannot establish that.** The lockbox stores its own key share
(`public_key` in `generated_public_key`) and never the coin's AGGREGATE key — that is what blindness
means here. With no aggregate key it has nothing to compare `agg_pubkey` against. This is a
consequence of the design, not a missing check, and any proposal to close it MUST say plainly which
part of the SE's blindness it is giving up.

**Consequences that must not be glossed.** `exit_key` and `fund_value` recorded at establishment are
witnessed *in the weaker sense*: they come from a transaction the SE bound, not from one it can tie
to the coin. Until that gap is closed, an entry in the SE's index attests "signed under this sid",
never "is a tier of this coin" — so a **parent edge MUST NOT be resolved through it**, because
REQ-56's frontier decides who is paid in a collapse and an absentee has no recourse afterwards
(REQ-67).

**REQ-66 (a conveyed-but-unreleased migration is a DOUBLE PAYMENT — the SSP MUST clear it).**
R1 and R2 protect different parties and MUST happen in that order: **migration protects the holder**
(they hold and have verified the replacement before giving anything up), **release protects the SSP**
(without it the old leaf is still in the frontier, so REQ-56 forces `C` to pay them on chain *as well
as* the leaf they already hold on `B`).

So a migration that is conveyed and then neither claimed nor released is a leaf the SSP pays for
twice. Before the round's notice the SSP MUST therefore, for every outstanding migration, either
obtain the release or **cancel the conveyance and reclaim it** (`apply_cancel`,
`server/src/database/transfer_cancel.rs`; `reclaim_cancelled_conveyance`,
`clients/libs/rust/src/tesr.rs`). A round announced with unresolved conveyances is an
operator-funded overpayment, not a protocol fault — **the holder is never at risk in either
direction**, which is why the ordering is safe to leave to the operator.

**REQ-67 (THE ABSENTEE'S PROTECTION MUST BE CRYPTOGRAPHIC, NOT THE SE PREDICATE ALONE).**
REQ-56 says the SE refuses a collapse that does not pay every unreleased leaf. **That is not
sufficient on its own.** In production the lockbox and the coordinator are the **same operator**, and
the lockbox is a plain container, not an attested enclave — so REQ-56 is a rule enforced by software
the SSP controls. For an ONLINE holder this is harmless: they verified their successor leaf
themselves and hold its key share. **For an ABSENTEE it is the whole of their protection, and it is
the same shape as trusting an unsigned operator assertion, one level up.**

The consequence is concrete: once `C` confirms, every tier beneath `F` is dead, so an absentee who was
not paid has **no recourse at all**. Their only remedy is to act *before* `C` confirms.

So the absentee's protection MUST rest on something the operator cannot alter:

1. **A watchtower MUST check pending collapses.** `C` is public from the moment it is broadcast. A
   tower holding a `WatchBundle` for a leaf MUST, on seeing any spend of its root's `F`, check whether
   an output pays its holder's `exit_key` at ≥ the leaf's funding value, and if not **broadcast `T`
   immediately** — which invalidates `C` (both spend `F`) and preserves the whole tree. This needs no
   key (`T` is pre-signed) and no trust in the operator.
2. **The `WatchBundle` MUST therefore carry `exit_key` and `funding_value`**, so the check is
   self-contained.
3. **Wallet defaults MUST run this**, and the product docs MUST say plainly that an absentee's
   protection is their tower, not the operator's good behaviour.

With the tower, the SE predicate becomes what it should be — **the mechanism that makes the honest
path cheap and the dishonest path detectable-and-defeatable**, rather than the sole thing standing
between an absent user and their money. Without it, form (c) is custodial in substance for anyone who
is not watching. **State this in those words wherever the round's trust model is described.**

**REQ-56a (a MULTI-INPUT spend has several parents, and the frontier must account for all of
them).** Measured on the live lane, not derived: the migration hatch's combine spends four carriers
into two children, and the SE co-signs it once per input — one transaction, four signatures, four
different sids. A registry that gives each child ONE parent therefore marks one carrier as spent and
leaves the other three looking untouched, so each stays in its own frontier and a collapse is
required to pay coins whose value has already moved into the children. That is an **overpay**: the
operator's loss rather than a holder's, and never a theft — but a wrong answer from the one predicate
whose entire job is exactness, and at scale it is what makes a round unaffordable.

The SE's index now keeps **every** co-signer of a transaction rather than the first, so the evidence
survives; which of them a child calls its parent, and how the frontier excludes a node spent by a
transaction it did not solely fund, is an OPEN question against the close and is **not yet answered**. Until it is, a
round MUST NOT be run over a tree containing a multi-input spend.

**REQ-68 (closing the gap requires a DEPOSIT-protocol change, and the SE cannot do it alone).**
The obvious fix — have the SE compare the disclosed `agg_pubkey` against the coin's aggregate key —
is **not buildable today**, and the reason is structural rather than an oversight: `/get_public_key`
accepts only a `statechain_id`. The SE mints its own keypair, returns its share, and **never learns
the client's public key**, so it can neither store nor derive the aggregate. There is nothing to
compare against.

Two candidate closures, and the difference between them is the whole question:

* **Client asserts the aggregate at deposit.** Insufficient on its own. An adversary can assert a
  VICTIM's aggregate for its own sid, and nothing detects it — the SE has no way to tell whose
  aggregate it was handed. First-writer-wins on a uniqueness constraint only moves the race.
* **Client supplies its OWN public key at deposit; the SE DERIVES the aggregate.** Sound, because
  the aggregate is then a function of the SE's per-sid key share and the declared client key. An
  adversary who declares a victim's client key still gets a *different* aggregate, since its sid's
  server share differs — it cannot make its own coin's aggregate equal anyone else's. This is the
  closure the spec recommends.

**DECIDED 2026-08-17 by the operator: the lockbox DERIVES it.** Option (b), and the cost turned out
to be far smaller than first stated — the correction matters enough to record, because the original
framing would have bought a privacy concession that had already been made.

**MEASURED: the coordinator already holds both.** `statechain_data` stores **`aggregate_xonly`
(UNIQUE)** and **`user_public_key`** (`server/src/database/deposit.rs`). The client already sends its
key at deposit; the operator already stores the aggregate, which *is* the funding address. So:

* against the **operator** — who runs both the coordinator and the lockbox, and whose lockbox is a
  plain container rather than an attested enclave (REQ-67) — the additional privacy cost is **zero**;
* the residual is that the fact moves into a **second** operator-controlled database, widening the
  blast radius of a lockbox-only breach or backup leak;
* it becomes a real cost only in a future where the lockbox is run by a **different party or is
  attested** — which is precisely the deployment in which you would want the SE verifying for itself.

A `NULL` `aggregate_xonly` occurs only for **old clients** (the code says so), and D24 already
decided legacy pre-0009 coins are ignored.

**DERIVE, never accept.** The lockbox MUST compute the aggregate from the client's public key and its
own key share, and MUST NOT take an aggregate supplied by the coordinator — the coordinator is
exactly the party REQ-56's frontier exists to be checked against, so trusting its value would return
the authority the derivation is meant to establish. The derivation is also self-defending: an
adversary who declares a VICTIM's client key still gets a **different** aggregate, because its own
sid's server share differs, so it cannot make its coin's aggregate equal anyone else's.

**Build order, and the test that decides it.** (1) forward `user_public_key` to the lockbox — today
`/get_public_key` receives only a `statechain_id`; (2) derive and store the aggregate per sid;
(3) refuse a binding whose `agg_pubkey` differs; (4) **prove it with an adversary presenting a
VICTIM's tier under its own sid and being refused** — a test exercising only the honest order proves
nothing here; (5) only then resolve parent edges through the index, and only then wire
`collapse_grant`.

**Steps (1)–(4) are BUILT and RUN.** `sdk92` half (b) is the deciding test: a self-consistent
disclosure built from keys unrelated to the coin, submitted under the coin's own sid, is refused
`403 AGGREGATE_MISMATCH`, while the same coin's own tiers are served — measured 4 bound / 4
co-signatures in the same run. The two gates are shown to be INDEPENDENT rather than one masking the
other: the same bytes with a one-satoshi lie are refused `400` by REQ-57's session compare, and the
test fails if (b2) is refused by that compare instead of by the aggregate.

Two properties fell out of the build that were not obvious when it was specified:

* **The aggregate is INVARIANT under `/keyupdate`, so a transfer does not break the binding.** The
  key-update algebra gives `s2 = s1 + o1 − o2`, hence `o2 + s2 = o1 + s1`; write-once storage is
  therefore correct rather than merely convenient. Measured two ways: the raw aggregate point is
  byte-identical either side of a live `/keyupdate`, and a full deposit → transfer → claim →
  split-transfer → claim → exit run co-signed under the new owner's rotated share with the gate armed
  (17 binds, 17× 200, 0 mismatches).
* **REQ-68 is the ENFORCEMENT POINT for that invariance, not a consumer of it.** A `key_update` whose
  algebra drifted by a single scalar produces a different post-rotation aggregate, and the very next
  co-signature is refused 403 — demonstrated live by submitting `t2 + 1`. Every transfer E2E is
  therefore now a regression pin on the key-update algebra.

**What is NOT closed is coverage, and it is the larger half — see V-7.** The check fails open for any
sid with no stored aggregate, which on the live regtest lockbox is **99.5 % of key slots**, and the
unbound set still grows because an empty `user_public_key` is treated as absent and the shipped wasm
and Kotlin bindings cannot send the field at all.

Until (1)–(4) are done and measured, nothing may present the SE's index as authority on parenthood.


### 5.5 Operator liquidity — ZERO, because a close pays out of `F` itself

The protocol requires **no operator capital at any point**. The round that demanded it is deleted, and
with it the successor root that had to be funded before holders could move. There is no window to
carry because there is nothing to carry it for.

**REQ-80 (the zero claim MUST always carry its mechanism). GUARDED.** This design may be described as requiring
no operator liquidity. The statement MUST be accompanied by the mechanism that makes it true — a close
pays every unreleased leaf out of `F` itself, and renewal moves no value — because "no liquidity"
asserted without a mechanism is a marketing claim rather than a property. It MUST NOT be extended to
the Lightning legs (§8), which consume ordinary channel liquidity in both directions and always did.

**REQ-69 (no per-payment liquidity).** An ordinary payment MUST NOT consume operator capital. A payment
that did would make the requirement flow-proportional, which is the axis this design is built to avoid.

**An SSP MAY offer to buy leaves as a SERVICE.** That is a business choice, demand-driven, and the
protocol works without it: a holder who finds no buyer closes or exits on their own. Nothing in this
document may depend on it.


## 6. Off-chain split & combine

### 6.0 Paying ANY amount — the tail, and the dust slot we were not spending

> **OWNER REQUIREMENT, 2026-08-20: a user MUST be able to pay any amount.** A design that quantises
> payments is a FAIL regardless of its other merits. This section is the answer, and it rests on a
> Bitcoin policy fact this document had not used.

#### 6.0.1 The physical law

A transaction may carry **at most ONE** output below the dust threshold — Bitcoin Core's
`MAX_DUST_OUTPUTS_PER_TX = 1`. A transaction that uses that slot must pay **zero fee**, and the dust
must be spent by the package child. The thresholds: a P2TR output is dust below **330 sat**; a P2A
anchor is dust below **240**.

**Spark does not evade this. It collides with it and does not check.** Verified against their source:
denominations are powers of two from 1 sat, the default wallet actively converges to one leaf per
denomination (so 1-sat leaves are manufactured routinely), a leaf is a REAL output at its exact value
— their own fixture decodes to a v3 transaction with an **8-sat P2TR output** — and there is no dust
check anywhere on their tree path. Every Spark transaction carries a **zero-value** P2A anchor, and
that anchor is itself dust, so the one permitted slot is already spent. Any payload below 330 makes the
transaction carry two dust outputs and `IsStandardTx` rejects it. Because a branch transaction carries
every child as a sibling output, **one sub-dust child kills the whole branch** — their optimiser will
cheerfully request `[1, 1, 2, 4]`, four jointly dead outputs in one transaction.

So Spark bought arbitrary amounts by minting outputs it never intends to broadcast, recycled through
SSP swaps rather than through exit. Their published 16,348-sat floor is **economic, not physical** — it
appears in their documentation and in **zero lines of their code**.

#### 6.0.2 What we have that they do not

**Our anchor is FUNDED at `P2A_VALUE = 240`, exactly its own standardness threshold — so it is NOT
dust, and our dust slot has never been spent.** Every tier we build carries zero dust outputs. That is
one free sub-dust output per transaction, available to us and not to them.

#### 6.0.3 Four leaf shapes, chosen by value

No denomination grid is introduced: splits stay exact and Σ-conserving, so every amount at or above the
dust limit is already expressible today.

| value `v` | shape | cost |
|---|---|---|
| `v ≥ min_child_value` (1560 at 3.0 sat/vB) | today's two-rung ladder, self-funding | unchanged |
| `945 ≤ v < 1560` | ONE rung — the spine-tip shape, which already exists | one renewal instead of two |
| `DUST_LIMIT ≤ v < 945` | depth-0 stub, no ladder; zero-fee v3 with a zero-value anchor, bumped in package | floor is exactly 330 |
| `1 ≤ v < DUST_LIMIT` | **a TAIL** — carried as the split's single permitted dust output | see below |

A tail forces two things on its split transaction and on nothing else: the anchor must be the FUNDED
240 kind, so the tail owns the dust slot; and the split's fee must be zero, so it enters the mempool
only as a package whose child spends the anchor, the tail, and the broadcaster's own fee input.

**THE CHOICE IS BUILT; THREE OF THE FOUR SHAPES ARE NOT REACHABLE YET, AND THAT GAP IS MEASURED
RATHER THAN DESCRIBED.** `LeafShape::for_value` implements the table above, deriving both upper
boundaries from `min_child_value` and `min_spine_tip_value` — the same functions the admission guards
and the builders call, never restated as literals, so the shape a payment is admitted at and the
ladder then built cannot be two different answers. `req83_leaf_shapes` proves the property REQ-83
actually needs, which is not that the arms are correct one by one but that **the four bands tile
`[1, ∞) `with no gap and no overlap**: it sweeps every value up past the top boundary at seven fee
rates from 0.1 to 1 000 sat/vB, because the defect being guarded against is a one-satoshi hole at a
boundary and sampling is exactly what misses one. A hole is not a cosmetic defect — a hole is an
amount a user cannot pay.

The bands also cannot invert at any rate, which is what keeps all four arms reachable: the gaps
between them are `DUST_LIMIT`, `P2A_VALUE` and `committed_fee + P2A_VALUE`, all strictly positive.
That is pinned too, because the argument depends on the shape of two functions defined elsewhere.

**What a selector does NOT establish is that the payment lane can reach it** — a selector with no
caller reads exactly like a working feature, which is this repository's most repeated failure
(`sdk74`'s retry that never ran; a fork extractor that validated perfectly and credited zero). So
`the_payment_lane_today_admits_only_the_laddered_band` measures the live guard instead:

| band | reachable through `transfer()` today |
|---|---|
| `Laddered` | **yes** — the piece floor sits exactly at its boundary |
| `SpineTip` | no |
| `Stub` | no |
| `Tail` | no |

The piece floor IS the `Laddered` boundary, so every amount in `[1, 1560)` is refused before a
builder is ever consulted. That test is written to FAIL when a lower band is wired up, so closing one
cannot pass silently either.

#### 6.0.4 The release fragment, and why a tail cannot take a sibling hostage

This is the part Spark does not have, and it is what makes sub-dust leaves safe here rather than merely
possible.

At split time the tail's owner and the SE co-sign a spend of the tail outpoint with
`SIGHASH_NONE | SIGHASH_ANYONECANPAY` (0x82, valid for a taproot key-path spend), and that signature —
the **release fragment** — is published to every sibling in the conveyed bundle. Because it commits to
no outputs, any party who ever needs the split on chain can attach it alongside their own fee input,
satisfy the ephemeral-dust rule, and keep the tail's satoshis as fee credit.

**Consequence: a tail can never block, hold hostage, or price a sibling's exit.** In Spark a branch
containing a sub-dust child is dead for everyone in it; here the sibling sweeps the tail and proceeds.

**The security analysis, because a signature committing to no outputs deserves one.**

* **What it authorises.** `ANYONECANPAY` commits to *this* input — its outpoint and its amount — so the
  fragment cannot be replayed against any other outpoint, including another tail under the same key.
  `SIGHASH_NONE` lets the spender choose every output. So the fragment is an unconditional licence to
  spend **one specific outpoint worth at most 329 sat**, and nothing else.
* **Blast radius.** Bounded by the tail's own value, which the tail's owner surrendered deliberately as
  the price of riding for free. No other output, coin or tier is reachable with it.
* **Why sweeping tails is not a business.** At most ONE tail per transaction, value in `[1, 329]`. Any
  thief must first put the split on chain, which costs roughly 168 vB — about 504 sat at 3.0 sat/vB, and
  more at any realistic rate. **The maximum prize is strictly less than the minimum cost**, on every
  fee schedule, so the attack never pays. The one-tail-per-transaction cap is what makes this argument
  hold, and it MUST be bound in the verifier rather than left as a convention.

#### 6.0.5 Tails are a SATS-ONLY mechanism — a coloured tail would be a burn switch

**The blast-radius argument in §6.0.4 holds only because a tail's worth is its satoshis. For a coloured
leaf that is false, and the difference is not a detail — it is the whole safety case.**

An RGB allocation is bound to an OUTPOINT, and the amount of the asset is carried in the consignment,
not in the output's value. A 329-sat tail can therefore hold an unbounded quantity of an asset. The
release fragment authorises **anyone** to spend that outpoint to **any** outputs, and spending a sealed
outpoint without carrying its allocation forward **destroys the allocation**. So for a coloured tail:

* the prize is not ≤329 sat, it is the entire allocation;
* "maximum prize below minimum cost" collapses — sweeping becomes arbitrarily profitable;
* and the fragment stops being a courtesy to siblings and becomes a **burn switch anyone may pull**.

**REQ-86 (no coloured tails). BUILT AND TESTED.** A tail MUST NOT carry an RGB allocation. The
verifier refuses a coloured payload under `DUST_LIMIT` (`refuse_coloured_tail`, called on every tier
in the coloured verification loop; `req86_no_coloured_tails`, three cases including that the offending
vout is NAMED rather than merely detected).

**It is deliberately redundant today, and that is the requirement rather than an oversight.**
`refuse_dust_payloads` already forbids EVERY sub-dust payload, so this guard cannot fire yet and the
prohibition currently holds vacuously. The moment REQ-83 admits tails for sats, that blanket stops
covering the coloured case — and a prohibition that existed only as a side effect of another rule
would vanish with it, silently, without anyone editing a line. Naming it now is what keeps it when
the ground moves.

**And RGB does not need tails, which is why this costs nothing.** For a coloured leaf the amount a user
pays is the ASSET amount, and that is already arbitrary on any carrier — the satoshis are a vessel, not
the payment. A coloured leaf therefore keeps a carrier at or above `DUST_LIMIT` and expresses any asset
amount natively, with no grid, no tail and no fragment. The dust floor bounds the VESSEL, never the
value being sent.

**The same asymmetry applies to the swap.** A value-neutral swap needs a counterparty holding
interchangeable value; satoshis are fungible and asset allocations are not, so an SSP can only swap
coloured leaves of the SAME contract from its own inventory of that contract. **The swap is therefore a
sats-side mechanism**, and coloured payments rely on exact-fit selection and whole-leaf retransfer —
both of which are built, `cosign_colored_child_retransfer` included. Where no coloured subset fits, the
fallback is a split, exactly as today.

**REQ-83 (any amount, and the tail that makes it possible).** A SATS payment of any amount at or above 1 sat
MUST be expressible; a COLOURED payment of any asset amount MUST be expressible on a carrier at or
above `DUST_LIMIT` (§6.0.5). Amounts below `DUST_LIMIT` MUST ride as a tail: at most one per transaction, value
in `[1, DUST_LIMIT)`, in a transaction carrying the funded 240 anchor at zero fee.

**REQ-84 (a tail MUST carry its release fragment).** No tail may be created without its
`SIGHASH_NONE|ANYONECANPAY` release fragment being conveyed to every sibling of that split. A tail
without one is a hostage: it makes the split unbroadcastable and strands every sibling, which is
precisely Spark's failure and MUST NOT be reproduced. The verifier MUST refuse a bundle whose split
carries a tail with no fragment.

**REQ-85 (the one-tail cap is load-bearing, not tidiness).** At most one sub-dust output per
transaction MUST be enforced in the verifier. It is what keeps the maximum sweepable prize below the
minimum cost of broadcasting, and it is the whole of the economic argument in §6.0.4.

**REQ-83 and REQ-85: THE RULE IS BUILT AND TESTED; THE ADMISSION IS NOT.** `tail_verdict` decides
what a well-formed tail is — exactly one sub-dust output, value in `[1, DUST_LIMIT)`, in a
transaction carrying the FUNDED 240 anchor — and `req83_85_tail_rule` pins it in six cases, including
that the boundary is exclusive (a payload AT the floor is ordinary, not a tail), that 1 sat is a
legitimate tail, that the anchor and the opret are never counted as tails, and that TWO tails are
refused because `329 + 200 > 504` — the moment sweeping starts to pay, which is the whole of REQ-85.

**The live verifier still refuses every sub-dust payload**, because §6.0's relay claims are unproven.
Building the rule first means that when admission is switched on, what gets admitted is already
specified and tested rather than invented under pressure.

**And a correction worth keeping: tails belong to the PAYMENT lane, not to tier verification.** A
first attempt wired the rule into `refuse_dust_payloads`, which verifies TIERS, and eight
dust-poisoning attack tests failed — correctly. On a tier a sub-dust output is an ATTACK, and
reporting it as "a well-formed tail, admission pending" tells an attacker their shape is right and
softens a security refusal into a feature-flag notice. The same bytes mean opposite things in the two
lanes.

**THE RELAY CLAIMS ARE NOW PROVEN — measured against Bitcoin Core 30.2, not read from its source.**
`scripts/tail_relay_probe.py` asks the node and records what it says:

| shape | verdict |
|---|---|
| v3 TRUC, FUNDED 240 anchor, 0 fee | refused — **`min relay fee not met`** |
| v2 legacy, funded 240 anchor, 0 fee | refused — `min relay fee not met` |
| v3 TRUC, **ZERO-value** anchor (Spark's shape) | refused — **`dust`** |
| PACKAGE `[0-fee tail parent, paying child]` | **`package_msg: success`** |

Read the first and third rows together, because the difference between them is the entire design.
**Our shape is refused for the FEE, not for DUST** — the funded 240 anchor sits at its own
standardness threshold, so it is not dust and the transaction's one permitted dust output is left
free for the tail. Spark's zero-value anchor IS that one permitted dust output, so a sub-dust payload
makes a second and the transaction is refused as `dust`. That is why a sub-dust child kills a whole
branch there, and it is now measured rather than asserted.

And the fourth row is the claim §6.0 actually rests on: a 0-fee parent carrying a tail **relays** when
its child pays for it.

**REQ-84: THE FRAGMENT IS BUILT, AND ITS SAFETY ARGUMENT IS A TEST.** `release_fragment_sighash`
and `verify_release_fragment` produce and check the `SIGHASH_NONE | ANYONECANPAY` signature, and
`req84_release_fragment` pins the three properties the design rests on:

* **`SIGHASH_NONE` lets any sibling choose their own outputs** — the fragment is published at split
  time to parties who do not yet know what their sweep looks like, so a fragment that committed to
  outputs would be useless to them, and a tail nobody can sweep is exactly the hostage REQ-84 forbids;
* **it cannot be replayed against another outpoint** — `ANYONECANPAY` still commits to this input's
  outpoint, so the licence covers one outpoint and nothing else, not another tail under the same key,
  not a sibling vout of the same transaction;
* **nor against a restated amount** — the input's value is committed too.

Together with REQ-85's one-tail cap that is the whole safety case: an unconditional licence to spend
**one** outpoint worth at most `DUST_LIMIT − 1`, against roughly 504 sat to broadcast the split.

**What is NOT built is the bundle-level enforcement** — "the verifier MUST refuse a bundle whose
split carries a tail with no fragment" needs the conveyed bundle to carry fragments, and cannot be
exercised while tails are not admitted. That is the remaining half, and it lands with the admission.

**Still open, stated precisely.** The three load-bearing CLAIMS of this section are settled: that a
funded 240 anchor leaves the dust slot genuinely free and that a `[tail, funded anchor]` split relays
as a zero-fee package are both measured by `scripts/tail_relay_probe.py`, and the release fragment's
behaviour is pinned by `req84_release_fragment`. What remains is not evidence but PLUMBING, and it is
two things:

1. **the builders for the three lower leaf shapes** — the selector chooses them, no builder emits
   them, and the table above says so;
2. **the bundle-level half of REQ-84** — "the verifier MUST refuse a bundle whose split carries a
   tail with no fragment" needs the conveyed bundle to carry fragments, and cannot be exercised
   while no lane emits a tail. It lands with the admission, not before.



### 6.1 In-ladder split (laddered coins)
A non-exact payment out of a laddered coin is an **in-ladder split**. `transfer()` routes on
`ParentShape`, which has THREE arms — `Root → in_ladder_pay`, `Child → child_in_ladder_pay`,
`SpineTip → spine_batch_pay`. There is no fourth: `Unladdered → split_coin` is DELETED (§0.3), so a
coin carrying none of the three is REFUSED by `parent_shape` rather than routed to a plain split.
`parent_shape` probes the spine tip FIRST. That ordering has a consequence worth stating: `in_ladder_pay` gives its change leg
`ChangeLeg::LastIsTip`, so after the first partial payment the sender's change IS a spine tip, and the
SECOND and every later payment out of it take the `SpineTip` arm rather than this one. `SP` is a SPINE
state tier spending `X_m.out[0]` at `SPINE_CSV = 0` — a DESCENDANT of the trigger, never a rival for
`F`, and strictly below the `S_0` it replaces on that output — carrying one resting output per child
plus the P2A anchor; each child then hosts its own extension + state tiers (`establish_child`). The
parent is terminalized before the co-sign and its superseded state disclosed for the receiver's
census (REQ-38).

**REQ-39 (in-ladder split)** A laddered coin MUST NOT be split as plain BTC: a prior owner's
retained no-timelock trigger could spend `F` and void a split of it while the ladder still paid the
splitter the whole coin. The split MUST descend from the trigger, value MUST be conserved
exactly — `Σ children == tier_out_total(X_m.out[0], n) = X_m.out[0] − committed_fee_for_outputs(n)
− 240`, where `committed_fee_for_outputs` adds 43 vB per extra child so the tier still relays
standalone — and every child MUST clear the §5.1 executor floor before the parent's budget is
consumed (ERR-16). Verified by `sdk58` (accept + 12 adversarial cases REJECT: aggregates,
hidden-state, Model-A payee, parent terminality, child-superseded race, count-padding, value-spoof),
`sdk59` (end-to-end split payment), `sdk04` (the terminalized parent refuses a second spend at both
the wallet and the SE).

**REQ-47 (split depth) The build side MUST NOT mint a child the receive side would refuse.**
A conveyed child is admitted by `check_exit_headroom_with_margin` — the exit walk must fit inside the
epoch the payee inherits WITH `exit_slack_margin = max(required/4, required/tiers)` of head-room, not
merely by the bare latency rule `exit_wait_blocks <= epoch`. Both sides MUST evaluate the SAME rule.

The caps that follow are **derived, not chosen**: depth **8** on mainnet (19 transactions to walk),
depth **54** on regtest (111). They are stated here because the failure they prevent is silent and
expensive: a builder using the bare rule against a payee using the margin rule mints depths that are
unadoptable at every tip — and since a parent is terminalized before its child is conveyed, each such
child is a stranded piece with a terminalized parent behind it. Held together by
`the_build_side_never_admits_what_the_receive_side_refuses`.

**REQ-48 (the payee's clock)** The window a split is measured against MUST be derived from the
PARENT's own conveyed backup chain, never from a freshly-read epoch. A builder measuring a fresh
epoch while the payee measures `epoch_expiry − tip` admits splits the payee cannot use. The parent's
flat backups travel with the bundle for exactly this reason — **a local lookup and a conveyed fact
are not interchangeable**: a wallet holding a conveyed CHILD has never held the root, so looking the
backups up by `(wallet, parent_sid)` finds nothing (`sdk17` catches this immediately).

### 6.2 Branch split & combine (the legacy coloured lane)
A branch split builds one SE-co-signed, un-broadcast tx spending the coin's FUNDING output `F` into
`{piece sub-coin, change sub-coin}` (minus a fee reserve), records both as sub-coins with their own
backup ladders, the shared exit branch, and ancestor records; it sets the parent's spend budget to 1.

**The PLAIN-BTC form of this is DELETED** — `split_coin` and every route into it (§0.3). It spent `F`
directly, which is the same outpoint a prior owner's retained no-timelock trigger spends [B1], and a
laddered coin now carves its piece out of the ladder instead (§6.1). What remains compiled in is the
**COLOURED** form, `mercuryrustlib::rgb::create_colored_split_tx` / `create_colored_combine_tx`, and
it exists for one reason: a carrier for which no coloured ladder can be built at all. On a wallet
with `colored_ladder` on, `refuse_legacy_colored_split_lane` refuses this whole route unless
`migration_hatch_verdict` can prove, of EVERY carrier the spend would touch, that it holds no ladder
of any kind AND cannot be coloured now — i.e. that no trigger exists or can be built to rival the
spend. Three conditions, all necessary, none of them widenable by an empty list: an unnamed carrier
proves nothing and is refused. A carrier that is merely not laddered YET is refused too, and waits
for its ladder.

The receiver side of this lane is NOT legacy and does not retire with the producer: a receiver
adopting a conveyed sub-coin writes its `branch-<statechain_id>` rows, every exit path reads them,
and a sub-coin with no `branch-` row cannot be exited at all (§2.3, §9.2).

**REQ-18** Before co-signing a split/combine, the SDK MUST set the parent(s)' `spend_budget` to 1
(exactly one more co-signature). After the split, each parent MUST be terminal.
**INV-10** `piece_sats + fee_reserve < parent_sats`; `change_sats = parent_sats − piece_sats −
fee_reserve`. `fee_reserve = clamp(parent_sats/100, 300, 2000)`.
**INV-11** A split tx has exactly one input (the parent) and one output per split entry plus, for
colored splits, one OP_RETURN; `output_vouts.len() == splits.len()`.
**Combine.** N coins → M outputs in one SE-co-signed (per-input) tx; each input matched to its coin
by outpoint; per-input MuSig2 over all prevouts.

### 6.3 First-class split children
A RECEIVED in-ladder split child is a **first-class coin**, not an exit-only claim
([CHILDREN.md](CHILDREN.md)).

**REQ-40 (child handover)** Conveying a child MUST include the standard SE key-handover material,
and the receiver's claim MUST COMPLETE that handover (`/transfer/receiver`) after the census passes:
the SE rotates its share so `A_child` is INVARIANT (every pre-signed child tier stays valid) and the
sender is permanently locked out (auth rotated). The child is deliberately left NON-terminal — its
safety is the census (REQ-38) against any pre-conveyance rival plus the pending-transfer lock
(REQ-36) against a post-conveyance one. These MUST hold together: a non-terminal child conveyed
WITHOUT a completed handover and held past the lock's expiry could be out-raced by the still-owner
sender. The one exception is a Lightning-latched piece, which stays terminal (INV-30).
**REQ-41 (onward payment)** A first-class child MUST be payable onward off-chain, either WHOLE
(`child_retransfer` — co-sign a fresh state over `ext_child.out[0]` at a strictly lower CSV paying
the new recipient, disclosing the state it replaces) or SPLIT (`child_in_ladder_pay` — the child's
state is replaced by a split state paying two grandchildren, giving a depth-2 ancestor chain). Each
hop costs exactly ONE co-signature and discloses exactly ONE superseded state, which the next
receiver's N-hop census counts and proves out-raced. Verified by `sdk60` (alice → bob → carol, the
funding outpoint unspent throughout, carol exits to her own key) and `sdk17` (multi-hop with a
partial second hop). A cooperative `withdraw` of a child is not possible — its funding `SP.out[j]`
is un-broadcast, so there is no confirmed outpoint to spend — and MUST be routed to the unilateral
exit instead (§9.2).

---

## 7. Tokens (RGB)

Assets are RGB contracts (NIA fixed-supply, IFA inflatable). A carrier is **laddered like any other
coin** — CTES-R colours every tier, so the allocation rides the ladder rather than a shape of its own.
The legacy coloured branch lane (§6.2) is what a carrier falls back to when no coloured ladder can be
built for it at all, and what an un-pinned network still runs on (§0.4 V-6); it is no longer the
default path for tokens.

**INV-29 (terminal freeze)** An RGB **carrier** is never laddered WITH A PLAIN LADDER: a plain T/X/S
tier spend is sats-only and would destroy the allocation, so carriers are excluded from PLAIN ladder
establishment (REQ-37). **The exclusion is from the PLAIN ladder, not from laddering** — the word to
avoid is still "structurally". `claim()`'s decision site is `match (config.colored_ladder,
allocation)`, and with `colored_ladder = true` a single-allocation carrier reaches
`build_colored_ladder_auto` + `cosign_colored_ladder` and IS laddered, coloured. Both `SdkConfig`
constructors now READ `TesrParams::attestation_identity_const` instead of shipping a literal, so that
branch is the shipped behaviour on every network with a provisioned enclave — regtest today, mainnet
the moment an identity is pinned there. A carrier still falls back to the flat signed-once backup
shape when the coloured lane cannot be taken FOR THAT COIN (its allocation is not booked yet, its
outpoint holds more than one allocation, its RGB state could not be read this pass); those are
retried on the next `claim()`, and while they last that carrier may only reach the legacy lane
through §6.2's migration hatch. Carriers are also excluded from plain re-anchor (REQ-32), from plain
withdraw/unilateral exit, and from watch bundles that carry a sats-sweeping backup (REQ-34).
Correspondingly, a colored tx only ever spends outputs of TERMINALIZED structure (terminalization
precedes the colored co-sign, and the SE refuses renewal on a terminal node), so no ancestor of an
RGB anchor is ever re-signed and **no superseded colored witness exists anywhere in the system** —
consignments carry un-broadcast witness txs, which is the model rgb-lib already supports.
(PROTOCOL.md §5.10.) Verified by `sdk52` (in one wallet the plain coin carries a ladder, the carrier
carries none, and an off-chain RGB transfer still settles) and `sdk32`.

> **Citation caveat — both RGB-carrier E2Es pin the OLD default, and §0.2(3) applies.** `sdk52`
> asserts "the RGB carrier must NOT be laddered" and `sdk74` asserts
> `!SdkConfig::regtest(..).colored_ladder`; both build their wallets from `SdkConfig::regtest`, which
> now READS regtest's pin and answers TRUE. Those two assertions therefore contradict the shipped
> constructor as written, so neither may be cited for the CURRENT behaviour until it is rewritten and
> RUN — a rewritten assertion is a new assertion. What they still verify unambiguously is the
> PLAIN-ladder half of INV-29, which no default change touches.

**Issuance.** `issue_token`/`issue_inflatable_token`: issue in the RGB engine, then fund + register
a statechain coin as the carrier in one colored on-chain tx.
**REQ-19** IFA issuance MUST create one colorable UTXO per allocation (fungible + each
inflation-right) before issuing.
**INV-12** After issuance the carrier holds the full fungible `supply`; IFA inflation-right stays
free in the engine.

**Mint (IFA).** `mint_tokens`: on-chain inflate in the engine, then bind the newly-minted allocation
to a fresh statechain coin.
**REQ-20** `mint_tokens` MUST isolate the newly-minted allocation (pre-inflate snapshot) so binding
never consumes already-bound supply.
**Burn.** `burn_tokens` burns engine-held free balance (on-chain). Statechain-bound supply must be
exited first.

**Transfer.** `transfer_tokens`/`batch_transfer_tokens` carve the recipient piece(s) + change out of
a carrier; the consignment rides `BackupTx.rgb_consignment` as a `ConsignmentEnvelope{c, a, s}`.
WHICH carve depends on whether the carrier holds a coloured ladder, and the two are different
TRANSACTIONS rather than one transaction under a different setting: with a coloured ladder on the
carrier the piece is a coloured IN-LADDER split (`colored_in_ladder_pay`) descending
from the carrier's trigger, and the legacy colored split — which spends the carrier's funding output
`F` directly, i.e. rivals that same trigger — is refused by `refuse_legacy_colored_split_lane`
(§6.2). Multi-carrier payment divides on the same fact. The legacy lane COMBINEs several carriers of
one asset (`colored_combine_transfer`) into ONE SE-co-signed colored combine tx (N input carriers →
recipient piece + change), conserving the allocation across all combined inputs. The coloured lane
cannot build that transaction at all — each input's `F` is already spent by that carrier's own `T`,
and there is no multi-parent coloured tier (`SP` spends exactly one `X_m`) — so it pays one
in-ladder split PER carrier (`colored_multi_carrier_transfer`) and the recipient books the legs as
separate allocations summing to the amount. INV-13's conservation holds either way, per transaction
on the legacy lane and per leg on the coloured one; ATOMICITY does not, and the difference is stated
rather than hidden: a leg that fails after earlier legs were conveyed SHORT-PAYS the recipient
instead of failing whole (L-14).
**REQ-21 (G2)** The receiver MUST book the amount the CONSIGNMENT assigns to its own witness
outpoint (`accept_offchain_amount`), treating the envelope amount `a` only as a cross-checked hint;
a mismatch MUST reject the transfer (ERR-8).
**REQ-22** The receiver MUST book under the consignment's cryptographically-verified `contract_id`,
not a sender-claimed id.
**INV-13** Token conservation: for a (batch) transfer, `Σ recipient amounts + change =
Σ allocations of the combined input carriers` (a single-carrier transfer is the N=1 case).
**N/A** RGB has no issuer freeze (no consensus enforcement point); documented, not faked.

---

## 8. Lightning swaps (SSP)

Both directions work on the laddered lane, via a HODL-invoice latch
([LIGHTNING.md](LIGHTNING.md)).

> **[M-6] On the Lightning lane, coordinator liveness between the pre-pay census and the completed
> claim is a payment-SAFETY dependency, not a liveness one.** This is a stronger statement than the
> trust model makes anywhere else.
>
> The lane runs census → pay → claim. The census reads a conveyed message; the payment is an
> IRREVERSIBLE Lightning leg; the claim comes after. A coordinator that serves a valid message to the
> census and then withholds, alters, or refuses at claim time leaves the payer out the full invoice
> amount — **acting alone**, with no sender and no key. Everywhere else in this document a
> coordinator acting alone can only deny; here it can take.
>
> The shape is not specific to the mailbox — refusing `/transfer/receiver` does the same — so it is
> stated as a property of the lane rather than of the transport. The failure text exists and has
> fired on the live stack ("paid the Lightning invoice … but claimed 0 transfers"), which is what
> makes the window observed rather than theoretical.

Each direction has an EXACT lane (the wallet ALREADY HOLDS a coin of the exact size — the whole coin
is latch-transferred) and a NON-EXACT lane (the coin is split IN-LADDER and the latched PIECE is
conveyed, §6.1). **The exact lane no longer MINTS its coin.** `ensure_exact_coin` used to fall back
to plain-splitting the smallest un-laddered coin, and that fallback died with the route it used
(§0.3): the plain split spent `F`, the outpoint a prior owner's retained trigger also spends [B1].
It now searches and refuses. That is not a capability lost but REQ-42's precondition met — the
non-exact lane carves its piece as a DESCENDANT of the trigger, which is exactly what makes it safe
where the minting fallback was not.

### 8.1 Pay (Mercury → Lightning)
`pay_lightning_invoice(ssp, invoice)`: find the exact coin, `create_external_hash_latch` bound to
the invoice's payment hash, hand the coin to the SSP; the SSP pays the BOLT11; the LN preimage
`unlock_by_preimage`s the coin and is returned to the payer as proof.

**REQ-23** The SSP MUST verify the latch hash equals the invoice payment hash before paying, and
MUST run its pre-payment value gate — `verify_bundle` / `verify_conveyed_child` over the conveyed
ladder — BEFORE `send_payment`, pricing against the value the ladder cryptographically commits to
(`sdk37`, `sdk63`).
**REQ-42 (one-call pay routes both lanes)** `pay_lightning_invoice` MUST NOT depend on obtaining an
exact coin: when the wallet holds no coin of exactly that size it MUST fall back to the non-exact
in-ladder lane (`pay_lightning_invoice_inladder`), the same way the receive side does. Without that
fallback the one-call API refuses every laddered coin — i.e. every coin — and is unusable. The
requirement got STRICTER, not looser, when the minting fallback was deleted (§8): the exact lane can
now only find a coin, never make one, so the fallback is the whole answer for every amount the
wallet does not already hold. `sdk63` (exact), `sdk65` (non-exact).
**INV-14 (atomicity)** The SSP can claim the coin **iff** it holds the preimage, which exists **iff**
the invoice was paid. No payment ⟹ latch expires ⟹ payer keeps the coin. The returned preimage
MUST satisfy `sha256(preimage) == invoice_hash`.
**REQ-43 (failed pay is recoverable)** A pay that fails after the coin was latched MUST leave the
value fully recoverable. Non-exact: `pay_lightning_invoice_inladder` MUST ROLL BACK — the optimistic
booking is wrong while `SP` is un-broadcast, so the parent is restored as exitable and the piece +
optimistic change are dropped, returning the WHOLE parent (`sdk66`). Exact: the orphan `S'` co-sign
inflates `sig_count`, so `reclaim_lightning_payment` MUST restore the coin locally as exitable
(value intact via the ladder; onward re-transfer is census-bricked until a `refresh`) rather than
attempt a self-transfer that would fail `verify_bundle` (`sdk68`).

### 8.2 Receive (Lightning → Mercury)
`create_lightning_invoice(ssp, amount)`: the SSP latch-transfers a coin to the user under an
SE-minted preimage and issues a HODL invoice on that hash; on payment the SSP confirms the latch
(releasing the coin) then retrieves the preimage and claims the HTLC. When the SSP holds no coin of
the exact size it fronts an in-ladder split PIECE instead; `settle_receive` is unchanged (it
operates on the piece's statechain id).

**INV-15 (atomicity)** The SE reveals the preimage only after the latch is unlocked (coin released),
so the SSP can take the HTLC money **only after** the user's coin is claimable. No payment ⟹ latch
expires ⟹ SSP keeps its coin. A wallet with zero on-chain presence can receive. No operator trust is
needed in this direction: the SSP owns the coin throughout its risk window. `sdk64` (exact),
`sdk67` (non-exact), `sdk19`/`sdk24`/`sdk25` (unpaid, cancelled, delayed-claim).
**INV-30 (latched-piece terminality)** A latched in-ladder piece is deliberately left unclaimed
until a preimage lands — precisely the situation the TEMPORARY pending-transfer lock (REQ-36) does
not cover, since it expires with the batch window and the receiver cannot complete the handover
until the latch releases. So for the latched lane, and ONLY there, the piece child is terminalized
at the SE before conveyance (while the sender still holds its auth key), permanently closing the
post-expiry rival window. Plain in-ladder payments rely on the pending lock plus the receiver's
prompt handover instead (REQ-40).

---

## 9. Exit

### 9.1 Cooperative (normal)
`withdraw(address, coins?)`: the SE co-signs a fresh direct spend to L1. For sub-coins the branch
is materialized first (branch txs are locktime-free). One on-chain tx per coin, no wait. A token
carrier MUST be excluded from the withdraw-everything default and hard-error if named (an
RGB-unaware sweep destroys the allocation, INV-29). A split child has no confirmed outpoint to
spend, so it is routed to the unilateral exit instead and booked `WITHDRAWING` (§2.1, §9.2).

### 9.2 Unilateral (SE gone)
`unilateral_exit(coins?)` dispatches on the coin's shape. **Four arms, probed in this order** — the spine tip is a shape of its
own and omitting it is not a simplification: a tip that falls through to the flat fallback gets its
`branch-` rows and its latest absolute-locktime backup broadcast, which is RGB-unaware and destroys a
carrier's allocation.
- **Laddered** — walk the tier chain: broadcast the trigger, then each extension/state as its
  relative-CSV matures (`exit_pass`). No absolute-locktime backup is broadcast. Idempotent and
  incremental: call once per block until `complete`. `sdk50` (SDK surface), `sdk40` PART 1
  (consensus: each tier is REJECTED before its CSV is met, accepted after).
- **Split child** — the same walk over the full pre-co-signed chain
  `T → X_m → SP → ext_child → state_child` (`exit_child_pass`), whose final state already pays this
  wallet's own key. This is also where a cooperative `withdraw` of a child is routed (§6.3).
- **Spine tip** (the sender's own change leg from an in-ladder split) — `exit_spine_tip_pass` /
  `_with_bump`, walking the one-rung cap over `SP.out[K]` via `next_spine_tip_exit_tier`.
- **Flat (no ladder)** — broadcast the exit branch (instant, locktime-free) then the coin's latest
  pre-signed backup, subject to its absolute locktime. This arm does NOT retire with the plain-split
  ROUTE: it is what exits a branch-carrying sub-coin, and on a network with no pinned enclave
  identity it is what exits every coin (§0.4 V-6).

**REQ-24** `unilateral_exit` MUST require no SE interaction.
**REQ-25** A tier or backup whose timelock is unreached MUST be reported as
`ExitStatus{complete:false, wait_blocks>0}`, not an error; callable again after the wait.
**REQ-44** `unilateral_exit` MUST refuse a coin that is not `CONFIRMED` even when named explicitly —
exiting a parent already consumed by a split would kill the tx funding the receiver's child —
and MUST refuse a token carrier (an RGB-unaware spend destroys the allocation, INV-29).
**INV-16** After the chain confirms, funds are at the owner's address; RGB allocations settle
on-chain.

### 9.3 Cost
`estimate_exit_cost(coin)` → `{branch_txs, branch_vbytes, backup_vbytes, total_vbytes, wait_blocks,
exit_deadline_block}`.
**INV-17** `total_vbytes = branch_vbytes + backup_vbytes` (measured from the actual pre-signed txs);
`fee_sats_at(rate) = ceil(total_vbytes · rate)`; `wait_blocks = max(0, backup_locktime − tip)`.
**Scope (stated honestly).** `estimate_exit_cost` measures the FLAT material only — the
stored branch plus the latest absolute-locktime backup — and it is also what feeds the calendar
deadline used by REQ-33. It does NOT account for a laddered coin's tier chain, whose cost and
wait are structural instead: 3 pre-signed tiers = 375 vB (3 × `TIER_VBYTES` 125, plus up to 3 P2A
fee children in a spike) and a sequential `E_m + Δ_k` CSV wait; each split level adds 2 tiers
(293 vB — an `SP` with two payload outputs, plus an extension) and ONE extension CSV, because the
`SP` itself is a spine tier at CSV 0 and waits only for its parent to confirm
(`config::tesr_exit_vbytes` / `tesr_exit_wait_blocks`, PROTOCOL.md §5.9).
`exit_deadline_block` is `None` for a laddered coin: it reports the height at which an ANCESTOR
could race an off-chain sub-coin, and a laddered root has no such ancestor. It is **not** a claim
that the coin has no calendar at all — the retained flat chain's locktime is a real deadline
(INV-27, `sdk86`) and no client surfaces it.

### 9.4 Refresh (cooperative on-chain re-anchor)
`refresh(id, fee_rate?)` / `refresh_sponsored(id, sponsor, fee_rate?)`: one SE-co-signed single-input
spend of the coin's current 2-of-2 outpoint into a FRESH deposit aggregate (a new `statechain_id`,
same owner; a sub-coin's exit branch is materialized first).

Refresh is **not a deadline reset for the EXIT** — a laddered coin's exit is the CSV tier
chain, which never matures while idle (INV-27). It does reset the coin's flat calendar, by
minting a fresh chain at `tip + initlock`, which is what makes it the answer for a coin that has
spent most of its hop budget. It is the **re-anchor primitive**: the escape hatch that moves a
coin out of its current ladder/branch and permanently kills every exit right rooted at the old
outpoint. For a coin that carries no ladder at all it is still the way to escape the backup-ladder
floor without going to L1 (§2.4).

**REQ-31** `refresh` MUST spend the current outpoint into a fresh aggregate, which then gets a fresh
full ladder of its own (REQ-37); because the old outpoint is now spent, EVERY exit right rooted at
it — every previous owner's backup and every old tier — is permanently invalidated. It is
COOPERATIVE (it needs the SE); if the SE is gone the owner exits unilaterally (§9.2) instead. The
fee is drawn from the coin (single-input, blind SE), so the user-pays variant yields `amount − fee`.
`refresh_sponsored` reimburses that fee OFF-CHAIN from a funded sponsor; because the rebate is a
non-exact payment out of the sponsor's own (laddered) coin it is minted by an in-ladder split, so
the rebate MUST be sized to `max(fee + dust, min_child_value)` — **1 560 sat** at the shipped 3.0.
Sizing it below that floor makes every sponsored refresh fail AFTER the user has already paid the
on-chain fee. The operator absorbs the difference; the user ends ≥ whole.
`sdk30` (a)/(c), `sdk38` (a broke sponsor loses boundedly).

**REQ-32 (auto-refresh)** When `SdkConfig::auto_refresh` is set (default), the SDK MUST re-anchor a
coin nearing its BACKUP-ladder floor before it is spent, transparently: `auto_refresh_due(margin)`
re-anchors every confirmed, non-carrier coin whose headroom (`locktime − tip`) is ≤
`auto_refresh_margin_blocks`, and `transfer`/`transfer_many` MUST run it (and await the fresh coins'
confirmation) before selecting coins. Token CARRIERS are excluded (a plain re-anchor would destroy
the allocation, INV-29).

> **`background_auto_refresh` has NO production reader.** The flag appears only in `SdkConfig`'s two
> constructors, in doc comments, and in tests; nothing on a runtime path reads it. Background
> re-anchoring therefore runs REGARDLESS of it, because `start_background` → `maintenance_plan`
> schedules `DeadlineSafety` unconditionally and `deadline_safety_due`'s first route is
> `auto_refresh_due`. The design intent behind the flag — an idle wallet never silently shrinks —
> stands as an intent; what implements it today is the margin (`auto_refresh_margin_blocks`), not the
> flag. Either wire the flag or delete it; a config field that nothing reads is a claim the code does
> not make.

> **Coverage note.** This requirement has NO live E2E: the pass itself is not exercised end-to-end
> today. The underlying re-anchor is covered by `sdk30`, and the property REQ-32 exists to guarantee
> (a coin never becoming un-spendable by aging) is STRUCTURAL for laddered coins rather than
> maintained by this pass: idle coins never age (INV-27) and renewal is off-chain and unbounded
> (`sdk43`). REQ-32 remains normative for the FLAT backup chain — which a laddered coin retains and
> which still ages (INV-27, `sdk86`), so this is not a requirement the retired shape took with it.

### 9.5 Watchtower (automatic deadline protection)
Two passes — but NOT one per shape, and the calendar pass is not for ladderless coins only.

> `start_background` does not branch on any flag: it iterates `maintenance_plan`, which returns
> `[MaintenancePass::DeadlineSafety]` unconditionally (its `SdkConfig` parameter is unread), and runs
> `deadline_safety_due` every tick. That pass is CALENDAR-driven over WHOLE LADDERED COINS as well as
> ladderless ones — a laddered coin retains its absolute flat-backup calendar (§0.3, G12, §14.3), so
> it is very much the subject of a deadline pass. The event-driven `defend_ladders()` alarm is the
> SECOND pass, not the laddered shape's only one.

**Calendar pass (coins with an exit branch).** `auto_exit_due(margin)` protects any owned coin with a branch that
is within `margin` blocks of its deposit-anchored exit-race deadline (§9.3), before an ancestor can
broadcast a stale backup. The background watcher MUST run it each poll when `SdkConfig::auto_exit`
is set (default), with `auto_exit_margin_blocks` — **derived, never chosen**:
`k_max·interval + tesr_exit_txs(d)·144`, i.e. **860** blocks on regtest (`14·10 + 5·144`) and
**2 120** on mainnet (`14·100 + 5·144`), because the exit walk lands `3 + 2d` transactions ONE AFTER
ANOTHER and each must confirm before the next tier's relative lock starts counting
(`config::auto_exit_margin_blocks_for`). A single confirmation window for a whole walk is not enough:
on mainnet the `k·interval` gap alone is 1 400 blocks. The walk's own `Σ csv` is deliberately NOT
folded in here — `auto_exit_due` takes that head start per coin, off the coin's own chain. A laddered
ROOT coin has no exit branch and therefore no deposit-anchored exit-race deadline, so it is not THIS
pass's subject — but it is not deadline-free either: its retained flat calendar belongs to
`auto_refresh_due` (REQ-32), the route `deadline_safety_due` runs first.

**Alarm pass (laddered).** `defend_ladders()` is event-driven, not calendar-driven: it is a no-op
while the coin sits un-broadcast (nothing ages), and reacts when someone ELSE spends the coin's
funding `F` — a hostile trigger by a prior owner or a griefer — by broadcasting the owner's own
tiers as each relative-CSV matures. Because the adopted current state carries the strictly lowest
CSV (INV-28), it matures first and the funds land at the OWNER's key. `sdk51`.
**REQ-33** For a **plain** sub-coin the watchtower MUST force a unilateral exit (§9.2). For a
**received token carrier** — which the plain exit refuses — it MUST instead MATERIALIZE the coin by
broadcasting ONLY its exit branch (settling the RGB allocation on-chain and spending the shared
root), NEVER the sats-sweeping backup; it emits `TokenCarrierMaterialized`. An issued/flat carrier
has no exit branch (no ancestor, no clawback risk) and MUST be skipped. This gives a received token
the same automatic clawback protection plain coins already have. `sdk34`.

**REQ-34 (keyless watch delegation)** A watch bundle MUST emit, per off-chain coin, only pre-signed
exit material and public metadata — the exit branch/tier chain, the timelock schedule, and (plain
coins only) the latest backup tx — and MUST contain NO key material; a token carrier's entry MUST
omit the backup tx entirely (structurally denying an RGB-destroying sweep, INV-29). A tower MUST be
able to protect the bundled coins with only an electrum connection (no wallet, DB, SE, or keys),
tolerate idempotent re-broadcasts (so N independent towers compose without conflicting), and surface
genuine rejections. Every tier pays the owner, so a malicious or buggy tower can only settle funds
to the owner early or do nothing. The full trust analysis is [TRUST-MODEL.md](TRUST-MODEL.md) §5.
Verified by `sdk45` (a keyless tower loaded from the persisted bundle alone drives an offline
owner's exit against a griefer's trigger; the bundle asserted to carry zero key material; a SECOND
independent tower over the same bundle is harmlessly idempotent), `sdk51` (the in-wallet pass),
`sdk52` (carriers structurally excluded), plus `unit::watchtower::tests`.

---

## 10. Invalidation & security invariants

> Invalidation has TWO mechanisms — and with one coin shape they are not one per shape: a laddered
> coin carries BOTH, because it retains its flat backup chain (INV-27).
> - **Tier replacement**: relative-CSV replacement — a lower-CSV tier out-races and orphans the one it
>   supersedes (INV-28), disclosed to the receiver and checked by the census (REQ-38). The
>   normative treatment is [PROTOCOL.md](PROTOCOL.md) §5.5/§5.7/§5.11. There is no ladder
>   formula and no re-anchor rent on this shape (INV-27), so the deposit-anchored deadline
>   arithmetic does not govern its EXIT. The retained flat chain still carries an absolute
>   calendar, and the `k·interval` gap still applies to that chain — `sdk86` measures the
>   per-hop cost directly.
> - **The flat chain**: the absolute-locktime decrementing ladder (§2.4, INV-5), whose floor is
>   escaped by exit, materialization or re-anchor. It is the ONLY mechanism on a coin that carries no
>   ladder at all (§0.4 V-6).

**INV-18 (no old state)** Split/combine spend into NEW outpoints; a child cannot confirm before its
parent (its input is the parent's output), so there is no old-vs-new race within a tree. On the
laddered shape the split state `SP` additionally DESCENDS from the trigger rather than racing it, so
a prior owner's retained no-timelock trigger can only start the clock on the current owner's own
chain, never void the split. Verified by `sdk58`/`sdk59`.
**INV-19 (fork prevention)** The SE refuses a second spend of any node (single-use / spend budget),
so a node cannot be forked into two conflicting children. Verified by `sdk04` (a terminalized
in-ladder parent is refused a second split at the SE, and the refusal is pinned to terminality
rather than to an incidental plumbing error), `rgb04` (single-use).
**INV-20 (terminal ancestors)** A sub-coin's receiver only accepts it if every structural ancestor
is terminal at the SE (REQ-17) — a malicious sender cannot double-spend a parent afterwards. The
receiver derives the required ancestor count from the branch itself: it requires at least one named
terminal ancestor **per structural INPUT the branch consumes** (`n_parents ≥ Σ inputs`, `≥ 1` —
`required_terminal_ancestors` accumulates `tx.input.len()` over the branch); a per-HOP count
(`branch_len`) would require only ONE terminal ancestor for an N-input COMBINE. So a sender cannot
hide a non-terminal, double-spendable ancestor by shipping an empty or short `terminal_parents` list
(ERR-7). Verified by `unit::terminal_parents_tests` (the count binding, including the
empty/short-list cases) and, on the laddered shape, by `sdk58`'s parent-terminality attack (a child
whose parent is not terminal is REJECTED). Honest-accept paths: `sdk29`/`sdk31`/`sdk39` (token
transfers over real branches). *Coverage gap:* no live branch-lane E2E rejects a non-terminal
ancestor end to end; the guard is in code (`verify_terminal_parents`) but only the unit test and the
laddered-lane equivalent exercise it.
**INV-21 (bounded lifetime)** With `epoch_deadline` set, the SE stops co-signing new state past the
deadline; unilateral exit still works forever (needs no SE), so funds are never swept.
**INV-22 (UTXO granularity)** Exact amounts are native (1-sat resolution) via off-chain split —
strictly finer than fixed-denomination leaves. The resolution is unchanged by TES-R; only the
minimum viable PIECE differs by LANE — not by shape, which there is now only one of —
`min_child_value` on the in-ladder lane against `min_split_output` on the coloured branch one (§5.1),
because a child funds two exit tiers instead of one backup.
**INV-23 (nonce single-use)** The SE binds each server nonce to exactly ONE challenge: `sign/second`
sets the challenge atomically only if it was NULL (or identical — idempotent retry) and otherwise
refuses (ERR-12). A second finalize over one nonce with a different message is therefore impossible,
which is what makes the blind-MuSig2 scheme safe against an owner who controls the raw signing
requests — without it, two partial signatures over one secnonce would leak the SE's key share and
yield two co-signed conflicting spends while `count_finalized_signatures` (and hence single-use /
budget / epoch enforcement) counted only one. The lockbox consumes the secnonce atomically
(`load_and_consume_secnonce`, `lockbox/src/db_manager.cpp`, called from `server.cpp`); the SGX
enclave lane carries the same consume (`enclave/App/database/db_manager.cpp`,
`statechain/sign.cpp`); the coordinator-side challenge binding
(`server/src/endpoints/sign.rs`) is a third, independent stop. Verified by `sdk12` Part C.
**INV-24 (budget monotonic)** `set_spend_budget` may only TIGHTEN a coin's `sig_budget`
(`new = min(existing, count+remaining)`); it can never raise it, so an already-terminal node cannot
be re-opened for a second conflicting spend. This is why a first-class split child is handed over
with a KEY ROTATION and a releasable pending lock (REQ-36/REQ-40) instead of a budget re-open: any
re-open would resurrect exactly the fork class this clamp prevents. Verified by `sdk04` (a terminal
node stays terminal and refuses a second spend) and `unit::invalidation_model::terminal_predicate_matrix`.
**INV-25 (branch value conservation)** The receiver's `validate_branch` rejects any exit branch whose
txs create value (`Σ outputs > Σ inputs` at any hop): `tx.verify` checks scripts but not the fee
rule, so without this a sender could hand over a coin whose branch is script-valid yet un-broadcastable
(the receiver could never exit on-chain while the sender keeps the funds). The guard is in code
(`transfer_receiver::validate_branch`) and honest branches are accepted by every token
E2E (`sdk29`/`sdk31`/`sdk39`). *Coverage gap:* no live E2E feeds a value-INFLATING branch, so the
reject side of this invariant is currently unexercised. The laddered shape's analogue (a value-spoofed
tier) IS covered, by `sdk54`/`sdk58`.
**INV-26 (received amount = spendable only)** A transfer's received token amount counts only
`Fungible` assignments, never `InflationRight` (the right to mint). Booking an inflation right as
spendable balance would let a right-holder inflate a receiver's balance out of nothing
(conserves INV-12/INV-13). Verified by `sdk09`.

---

## 11. Error semantics

- **ERR-1** single-use second spend → HTTP 410 `single-use coin already spent`.
- **ERR-2** past epoch deadline → HTTP 4xx epoch refusal.
- **ERR-3** spend budget exhausted → HTTP 410 `spend budget exhausted`.
- **ERR-4** preimage requested while latch locked → HTTP 404 `not available ... still locked`.
- **ERR-5** wrong preimage on `unlock/preimage` → HTTP 403.
- **ERR-6** deposit token requires payment → `SdkError::TokenPaymentRequired{token_id,
  deposit_address, fee_sats}`.
- **ERR-7** non-terminal ancestor → receiver validation error `structural parent ... is NOT
  terminal`, transfer not booked.
- **ERR-8** consignment/envelope amount mismatch → receiver rejects `consignment assigns X ...
  envelope claimed Y`.
- **ERR-9** `InsufficientBalance{requested, available}` on over-balance transfer.
- **ERR-10** double-withdraw / spend of a non-CONFIRMED coin → refused with the coin's status.
- **ERR-12** second `sign/second` reusing a server nonce over a different message → HTTP 409
  `server nonce already finalized with a different challenge`.
- **ERR-13** derived-token refusals: bad, replayed, or non-owner `auth_sig` → HTTP 401; `count`
  outside `1..=cap` → HTTP 400 `count must be between`; lifetime allowance exceeded → HTTP 429
  `lifetime derived tokens`; issuance disabled (`cap = 0`) → HTTP 403.
- **ERR-14** co-sign or re-address attempted while a transfer of the coin is open (REQ-36) → HTTP 409
  `coin has an open transfer ...` / `coin already has an open transfer to a different recipient`.
  A same-recipient retry is idempotent, not an error. The lock releases on `key_updated`, or when
  the transfer's OPEN WINDOW closes — and that window has **three** arms, of which `batch_timeout`
  governs only the last: an ordinary NON-batch transfer (the default lane) is open for a hardcoded
  **one hour** from `updated_at` that `batch_timeout` cannot move; a Lightning-latch batch is open
  until `MAX(lightning_latch.expires_at)`; a batch with no latch rows falls back to
  `batch_time + batch_timeout`. Quoting `batch_timeout` alone describes the rarest of the three.
- **ERR-15** census mismatch (REQ-38) → receiver validation error from `verify_bundle` /
  `verify_child_bundle`, transfer not booked (`num_sigs`/tier-count mismatch, an unlinked or
  unsigned superseded tier, a superseded CSV that ties or wins, or a ladder not exiting to the
  receiver's own key).
- **ERR-16** in-ladder split below the admission floor (§5.1) → refused BEFORE the parent's budget is
  consumed: `in-ladder split refused — the piece falls short` / `… the change falls short` / `… both
  legs fall short`, naming each leg's own floor. The two legs are floored independently
  (`SplitFloors { piece, change }`): a piece always funds two rungs, the change funds whatever
  `change_leg_role()` says THAT LANE's builder gives it — ONE rung
  (`min_spine_tip_value` = **945** sat plain / `colored_spine_tip_floor` = **1 074** coloured, at the
  shipped 3.0; the coloured tier is 168 vB against the plain 125) on the plain-root, spine-batch AND
  coloured lanes, and two rungs only on the plain-CHILD lane, where the change is carved as a
  `Piece`.

---

## 12. Traceability

Every requirement/invariant over BUILT behaviour is verified by at least one test; pure-logic items
have unit tests, protocol items have E2E tests (regtest). The rows that read NONE are the designed,
unbuilt sections, and they are marked as such in place.

| Item | Test |
|---|---|
| REQ-1, REQ-3 | design (2-of-2 keys); exercised by every co-sign flow |
| REQ-2, REQ-24, REQ-25, REQ-44, INV-16 | `sdk50` (SDK unilateral exit walks T→X→S to the owner's key), `sdk40` PART 1 (consensus: each tier rejected before its CSV, accepted after), `sdk58` (child chain exits to the receiver) |
| INV-17 (FLAT exit cost: branch + backup) | `unit::types::tests::exit_cost_math`, `unit::invalidation_model::exit_cost_scaling_model`, `sdk39` (depth-2 token exit) |
| REQ-4, REQ-14, ERR-6, INV-7 | `sdk01` deposit; `unit::types::tests::error_semantics` |
| REQ-5, ERR-1 | `rgb04` (single-use refusal) |
| REQ-6, ERR-2, INV-21 | `rgb07` (epoch deadline) |
| REQ-7, REQ-13, REQ-18, ERR-3, INV-19 | `sdk04` (terminalized in-ladder parent refuses a second spend, at the wallet and at the SE), `unit::types::terminal_predicate`, `unit::invalidation_model::terminal_predicate_matrix` |
| REQ-8, REQ-9, REQ-15, REQ-16, INV-5, INV-8 | `sdk01`, `sdk04`, `sdk41` (receiver gains control, sender locked out), `sdk55` (backup chain cannot be padded or inverted), upstream `tb01/tb05/tm01/ta02/ta03` |
| REQ-36, ERR-14 (pending-transfer lock) | `sdk49`/`sdk41`/`sdk01` + `sdk58`/`sdk59` (green with the lock live — i.e. the sender pre-sign re-ordering is correct and no honest flow is blocked); `sdk60` (a child conveyed under the lock is claimed and re-transferred); `tb05` drives the refusal itself — it conveys a coin, leaves the transfer open and unclaimed, then calls `transfer_sender::execute` again for the same statechain id to a DIFFERENT recipient, asserting the second call errors with "coin has an open transfer" |
| REQ-37 (ladder establishment) | `sdk48` (auto-established, seed-derived payee, idempotent), `sdk52` (carrier excluded from the PLAIN ladder — its "carrier carries none" assertion pins the OLD `colored_ladder` default and is not evidence for the current one; see INV-29's citation caveat) |
| REQ-45 (replay refused by name) | **Weaker than the rule.** What exists: the refusal's own predicate (which excludes `IN_TRANSFER` and `TRANSFERRED` by construction) and the live suite passing with the guard in place. **Two real gaps:** no test drives a deliberately DUPLICATING coordinator, and no test exercises a statechain SELF-TRANSFER against the guard — which is the case a careless predicate breaks. `rgb10` PART 2 is an RGB-LAYER self-split (one wallet mints two of its own witness seals and splits a root coin between them), NOT a statechain self-transfer, so it is not the negative control for this predicate |
| REQ-46 (balance counts coins, not rows) | `sdk01`, `sdk16`, `sdk32`, `sdk59` (balances unchanged under the dedupe — it removes a silent failure mode without moving any honest number) |
| REQ-47 (split depth cap), REQ-48 (the payee's clock) | `the_build_side_never_admits_what_the_receive_side_refuses` (the two gates evaluate one rule), `sdk17` (a conveyed child's parent backups come from the BUNDLE — a local lookup finds nothing), `d44_floor_probe` (the floors the spec publishes are the ones the code computes) |
| REQ-38, ERR-15 (census) | `sdk46` (count formula vs the real SE), `sdk47` (ladder carried across a transfer), `sdk54`/`sdk55` (padding/spoof REJECT), `sdk58` (**12** child-bundle attacks REJECT), `sdk56` (retry does not advance the count) |
| INV-27 (idle coins never age — CSV side; the flat calendar does), INV-28 (lower CSV wins) | **`sdk86`** (both clocks on a RECEIVED coin over 2 hops: chain byte-identical + `F` unspent, while `L` loses 300 blocks to mining and `interval` per hop), `sdk30` (a) (CSV half only, k=0 deposit), `sdk40` PART 2/PART 3, `sdk41`, `sdk51` |
| Off-chain renewal + rollover (§2.6) | `sdk42` (renew → persist → reload), `sdk43` (rollover to a fresh level, then exit the deep chain), `sdk44` (the whole cadence driven from the canonical `TesrParams` schedule via `establish_auto`/`renew_auto`/`rollover_auto`) |
| REQ-10, ERR-4 | `sdk19` (never paid → preimage withheld, receiver cannot claim), `sdk25` (a receiver who delays past the latch window loses the ability to claim), `sdk64`/`sdk67` (the release path) |
| REQ-11, REQ-12, ERR-5, REQ-23, INV-14 | `sdk63` (exact pay + SSP pre-pay census), `sdk65` (non-exact pay via a latched in-ladder piece), `unit::ssp::swap_tests::preimage_matches_hash` |
| REQ-42 (one-call pay routes both lanes) | `sdk63` (exact), `sdk65` (non-exact fallback) |
| REQ-43 (failed pay recoverable) | `sdk66` (non-exact rollback: whole parent recovered), `sdk68` (exact reclaim: coin restored as exitable) |
| INV-15, INV-30 | `sdk64` (exact receive), `sdk67` (non-exact receive via a latched piece), `sdk24` (payer paid, SSP aborts), `sdk25` (delayed-claim attacker fails) |
| REQ-15, INV-9 | `sdk01`; `unit::select` (exact/split/insufficient) |
| REQ-17, INV-20, ERR-7 | `unit::terminal_parents_tests` (count binding); `sdk58` (parent-terminality attack REJECT); `sdk29`/`sdk31`/`sdk39` (honest branches accepted). **Gap:** no branch-lane non-terminal-ancestor REJECT E2E |
| REQ-18, INV-10, INV-11 | `sdk29`/`sdk31` (colored splits/combines); `unit::split_math` |
| REQ-39, ERR-16 (in-ladder split) | `sdk58` (accept + **12** REJECTs), `sdk59` (end-to-end split payment), `sdk12` Part B (value flow), `sdk30` (c) (the `min_child_value` floor in a sponsored rebate) |
| REQ-40, REQ-41 (first-class children) | `sdk60` (alice→bob→carol off-chain, `F` unspent throughout), `sdk17` (multi-hop, partial second hop), `sdk04` (a spent parent is refused) |
| INV-18, INV-19 | `sdk58`/`sdk59` (SP descends from the trigger), `rgb03`/`rgb06` (off-chain DAG), `rgb04` |
| REQ-19, REQ-20, INV-12, INV-13 | `sdk09` (IFA issue + mint + batch) |
| REQ-21/INV-13 (multi-carrier combine) | `sdk31` (token combine) |
| REQ-21, REQ-22, ERR-8 | `sdk02`, `sdk09`; `unit::envelope` |
| INV-29 (terminal freeze / carrier ⊥ PLAIN ladder) | `sdk52` (plain coin laddered, carrier not, RGB transfer still settles), `sdk32` (tokens over time), `sdk39`. **Read the row title exactly**: the relation is carrier ⊥ PLAIN ladder, not carrier ⊥ ladder — a carrier IS laddered where the coloured builder can run. `sdk52`'s "carrier carries none" assertion pins the OLD `colored_ladder` default; see INV-29's citation caveat |
| ERR-9 | `sdk04` (`unit::select` insufficient) |
| ERR-10 | `sdk04` (double-withdraw / split-parent refusal) |
| INV-22 | `sdk01`/`sdk09` (exact-amount splits) |
| REQ-26 | `sdk11`; `unit::identity_tests::sign_validate_roundtrip` |
| REQ-27 | `sdk11` (multi-recipient), `sdk69` (a retained trigger is broadcast against a multi-recipient split and both recipients still exit) |
| REQ-28, ERR-11 | `sdk11`; `unit::invoice::tests` (roundtrip, reject) |
| REQ-29, REQ-30 | `sdk11` (query API + fee quote) |
| REQ-31 (refresh / re-anchor) | `sdk30` (a) (idle coin unchanged, then re-anchored) / (c) (sponsored rebate sized to `min_child_value`), `sdk38` (broke sponsor, bounded loss) |
| REQ-32 (auto-refresh in transfer) | **no live test** — see the coverage note in §9.4. Re-anchor itself: `sdk30`; unbounded off-chain renewal: `sdk43` |
| REQ-33 (watchtower carrier materialize) | `sdk34` (received-carrier auto-materialize, clawback defeated) |
| REQ-34 (keyless watch delegation) | `sdk45` (keyless bundle carries zero key material, a 2nd independent tower is idempotent, an offline owner is defended against a hostile trigger), `sdk51` (in-wallet pass), `sdk52` (carriers excluded); `unit::watchtower::tests` |
| REQ-35, ERR-13 (derived slots) | `sdk36` (poisoned-pool split/refresh, onboarding still charges, direct mint, caps, garbage/replayed/non-owner auth); `mercurylib unit::deposit::derived_token_tests` |
| INV-20 (ancestor-count binding), ERR-7 | `unit::terminal_parents_tests`, `sdk58` (laddered-lane equivalent) |
| INV-23, ERR-12 | `sdk12` Part C (nonce-reuse refused) |
| INV-24 | `sdk04` (terminal node stays terminal), `unit::invalidation_model::terminal_predicate_matrix` |
| INV-25 | honest branches accepted: `sdk29`/`sdk31`/`sdk39`. **Gap:** no live value-inflating-branch REJECT; the laddered analogue is `sdk54`/`sdk58` |
| INV-26 | `sdk09` (IFA received amount = fungible only) |
| Concurrency / chaos | `chaos22` (N users act in parallel) |
| REQ-49…REQ-52 (§5.3, the sweep) | **PARTIAL — the decision, not the absorption.** 8 unit tests over the pure predicate: the surplus is constant in face and vanishes at the 21.3 sat/vB indifference point; the fairness floor refuses ONE satoshi below walk-out value; all four REQ-50 limits pinned at their boundaries in both directions; a NaN market rate is REFUSED rather than admitted (the polarity that makes an unparseable rate fail closed); an absurd running exposure cannot overflow into looking empty; only the fee refusal is transient; and REQ-51's deadline path settles at an infinite fee rate, because an expensive settlement beats a voided leaf. The marginal vsize is DERIVED from the transaction module's input model and cross-checked against `sweep_tx_vsize`, so the two cannot drift. **No absorption path exists and nothing calls any of it** — see the §5.3 banner |
| REQ-57 (§5.4, witness binding) | `sdk92` (live: **4 bound / 4 co-signatures** on a laddered coin; one-satoshi lie refused BY THE SESSION COMPARE — and the test fails if the same bytes with the correct value are refused by that compare rather than by REQ-68's aggregate check, so neither gate can stand in for the other; **and the same request with the disclosure deleted is refused `400`**, which is what makes binding a property of the SE rather than a convention among clients), `sdk71` (**14/14, 0 refusals** across laddering + conveyance + claim), `sdk78` (the **coloured multi-input** path: a four-input combine, every input bound), `lockbox/tests/test_tx_sighash.cpp` (4/4), `lockbox/tests/test_session_rebuild.cpp` (3/3), each with negative controls. Measured across the live suite: **118 bound, 0 unbound, 0 index-miss.** **Open:** nothing in REQ-57 itself — the residual is REQ-65's aggregate question |
| REQ-56 (§5.4, the predicate + registry) | **PARTIAL.** The decision procedure is built and pinned: `lockbox/tests/test_registry.cpp` (23 checks — fork, released sibling, shared exit key, one-satoshi shortfall, INV-Q). The storage is built and pinned against a live Postgres: `lockbox/tests/test_registry_db.cpp` (26 checks — idempotent establish, monotone release, single-use nonce by PRIMARY KEY, freeze as a ratchet). **NOT built:** nothing populates it in production, and no `collapse_grant` route exists |
| REQ-68 (§5.4, why parenthood is not yet SE-authored) | **MEASURED, and it is a blocker.** `/get_public_key` takes only a `statechain_id`, so the SE never learns the client's key and cannot derive the coin's aggregate — see REQ-68 for the two candidate closures and the privacy cost of the sound one. Until it is decided, `se_signed_tx` is an audit trail only |
| Release (§5.4, `/release`) | **BUILT.** `lockbox/tests/test_release_route.cpp` — 10 checks against live Postgres and real BIP-340, three of them forgeries run BEFORE the honest path: no latch → refused (fails closed), someone else's key → refused, a signature over a DIFFERENT sid → refused (the tag binds it), replayed nonce → refused, fresh nonce → accepted, `released` monotone |
| REQ-61 (§5.4, the owner latch) | **ARMING BUILT, ENFORCEMENT NOT.** The key is read structurally from the unique P2TR output and stored write-once (`ON CONFLICT DO NOTHING`); a second arming with a different key is a no-op. Measured live: 4 bound co-signatures → exactly 1 `LATCH_ARMED`. **Nothing yet refuses a co-signature for want of a BIP-340 by that key** — and REQ-61(b) must be settled first, since the payer co-signs the payee's tiers before the payee holds anything |
| REQ-68 (§5.4, the coin binding) | **BUILT and MEASURED, coverage OPEN.** `sdk92` half (b) — a self-consistent disclosure built from keys unrelated to the coin, submitted under the coin's own sid, refused `403 AGGREGATE_MISMATCH` while the coin's own tiers are served in the same run; `lockbox/tests/test_aggregate_derive.cpp` (13 checks, incl. an adversary that cannot match a victim's aggregate); the SE↔client differential in `ci-guards/tests/emit_aggregate_vectors.rs`. Transfer-safety measured separately: the aggregate is invariant under `/keyupdate`, and a drifted `t2` is refused live. **Open: the check FAILS OPEN for 99.5 % of key slots — see V-7** |
| REQ-55, REQ-58…REQ-60, REQ-62…REQ-67 (§5.4, the rest) | **NONE — design, not built.** No frontier population, no `collapse_grant`, no freeze at grant time. See the status banner in §5.4 |
| REQ-69, REQ-80 (§5.5, operator liquidity is ZERO) | **DESIGN.** The requirement is now that no operator capital exists on any path; there is nothing to measure until a close has been run. REQ-70…REQ-75 were DELETED with the round. |

**Suite sizes.** Workspace unit + guard tests: **812**, 0 failures (`cargo test --workspace --tests`).
The E2E suite over regtest + lockbox + RLN is **85** tests.

## 13. Query, utility & invoice API

Client-side conveniences (no new SE state); mirror Spark's query/signing/invoice surface.

**REQ-26** `sign_message_with_identity_key(msg)` MUST produce a BIP340 Schnorr signature over
`sha256(msg)` under a STABLE identity key (derived at `m/1000h/0h/0h`, unchanged as coins come and
go); `validate_message_with_identity_key(msg, sig, pubkey)` MUST verify it and reject a tampered
message.
**REQ-27** `transfer_many(recipients)` MUST pay each recipient its exact amount from one off-chain
split (N pieces + change), under the same acceptance rules as a single transfer of that shape: the
parent terminalized before the co-sign (REQ-18), and the receiver's proof of ancestor terminality
taken from the lane the piece actually travels — the attested census on the in-ladder routes, which
are now the only routes this call has, and `terminal_parents` on a branch-carrying conveyance
(REQ-17).

`transfer_many` MUST dispatch on the parent's shape exactly as single-recipient `transfer()` does: a
laddered ROOT coin through a MULTI-CHILD in-ladder split (one `SP` over `X_m.out[0]` carving N
recipient children plus change), a received CHILD through the child-level equivalent, and a SPINE TIP
through the next spine batch. There is no fourth route: `ManyRoute::PlainSplit` and the plain N+1
branch split behind it are DELETED (§0.3), so every remaining route is in-ladder and the match has no
tail. A plain split of a laddered parent is the shape REQ-39 forbids — the split tx and the coin's
trigger both spend `F` — and it is now un-buildable rather than merely forbidden.
Building it on a RECEIVED laddered coin would leave its previous owner holding a broadcastable
no-timelock `T` that can void the split after the pieces are handed over. `sdk69` proves the required
shape by executing that attack: the retained trigger is broadcast and spends `F`, and both recipients
still exit unilaterally for their exact amounts, because `SP` descends from the trigger instead of
racing it. `sdk11` asserts the route as well as the amounts.

**REQ-28** `create_sats_invoice`/`create_tokens_invoice` MUST encode {address, amount, asset?,
memo?, expiry?} into a `utexoinv1…` string that round-trips through `decode_utexo_invoice`;
`fulfill_utexo_invoice` MUST reject an expired invoice (ERR-11) and otherwise pay the embedded
amount/asset to the embedded address.
**REQ-29** `list_coins`/`get_transfers`/`get_transfer` MUST reflect the wallet's current coins and
activity; `get_withdrawal_fee_quote` MUST return a positive fee at the electrum-estimated rate.
**REQ-30** `get_token_l1_address` returns the RGB engine funding address; `query_token_transactions`
returns the contract's transfer history.

- **ERR-11** `fulfill_utexo_invoice` on an expired invoice → `invoice expired at …`.

## 14. Named limitations

**A design is sound when its failure modes are enumerated and survivable, not when it has none.** A
specification that claims no drawbacks loses its authority at the first counter-example, so this
section is written to be the one a reviewer judges the document by.

Each entry says what it threatens, what it does NOT threaten, and what would close it. Nothing here
is a surprise found elsewhere in the document: §0.4 lists the divergences between this document and
the build, §0.5 the three things the design does not claim, and §1.2's goal table carries each goal's
scope limit inline.

### 14.1 Irreducible — these do not have a fix, and a future version will not close them

| # | Limitation | Why it cannot be closed |
|---|---|---|
| **L-1** | **The statechain trust unit** (X-7): the SE together with a past owner holding a retained pre-rotation share can fresh-co-sign an immediate spend | a fresh signature needs no backup, so no timelock reaches it; and erasure cannot be proven — any proof attests one instance of the data. What the ladder changes is the NOTICE **on the ladder's own path** — a thief who walks the tiers needs a public on-chain trigger and ≥144 blocks rather than a mempool race. **It does not FORCE that path**: `F` is built key-path-only (`Address::p2tr(.., None, ..)`, no merkle root), so nothing at consensus requires a spend of `F` to be a trigger, and the trust unit — which holds the full key — can sign a direct spend with no tier, no timelock and no alarm. A coin received before the compromise and left untouched is unconditionally safe |
| **L-2** | **Sub-economic finality**: a piece whose value is below the cost of defending it is forfeit to the party who split it | **NOT an unconditional option — state the counter, or the row is wrong.** The splitter holds the parent's lowest flat backup, one 112-vB transaction spending `F` that pays them the whole coin and voids the entire subtree at zero marginal cost per extra piece. But the piece holder holds `T` (it travels in `ChildTesrBundle.parent`), `T` also spends `F`, and `T` carries NO timelock — `TRIGGER_SEQUENCE` disables the relative lock and the builder sets no absolute one. So `T` is valid the instant `F` confirms while the backup is valid only at `L_k`: the payee can PRE-EMPT for the whole window, not merely race at the end, and one confirmed `T` kills every flat backup permanently. **A payee who acts keeps their money, always.** What is irreducible is that acting costs the walk (`3 + 2d` transactions, `293d + 375` vB), so below break-even the defence costs more than the piece — and only THERE is the splitter's backup free. The residual is an economic viability bound on small pieces, not a theft option over large ones |
| **L-3** | **No operator-side value rule is possible**: the SE cannot refuse to co-sign a piece below a viability floor | it is blind (G9) — it signs 32-byte hashes and cannot tell a tier from a backup or a 1 500-sat coin from a whole bitcoin. Every value defence is therefore receiver-side. "The SE enforces a floor" is a WRONG proposal and is recorded here so it is not proposed |
| **L-4** | **No in-protocol payment atomicity for a plain transfer** | a transfer is a one-way handover. Delivery-versus-payment needs the Lightning latch (§8) or an invoice |
| **L-5** | **Perpetual watching** is the price of zero idle rent | this is the trade the architecture exists to make: 0 vB of idle rent (G12) in exchange for a REACTIVE duty. Nothing ages while un-broadcast, but once a hostile trigger is public the defence is a race the owner or a tower must enter. On the mainnet schedule no theft transaction can become valid until `e_floor + d_floor` = **288** blocks after that public trigger, and nothing ever expires to the operator |

### 14.2 Open, with a known fix and a named owner

| # | Limitation | Threatens | What closes it |
|---|---|---|---|
| **L-7** | **The sid ↔ aggregate binding is an unauthenticated coordinator column** (CO-1, §0.4 V-4) | G2/G11 for a coin whose acceptance path consults it. A NULL leaves any coin with no ladder at all — since the plain-split lane was retired, that is now a coin the planner REFUSES rather than one it routes cheaply; a wrong value combines with the rogue-key decomposition (the SENDER picks `user_public_key`) to make an attacker-chosen output pass | attest the binding as the count is. Note the fix that does NOT work: `validate_tx0_output_pubkey` cannot serve, for the rogue-key reason above |
| **L-8** | **The attested counter is a plaintext row in an operator-run database** (CO-3 / SM-5, ONE defect — do not price it twice) | the census's right-hand side. The attestation authenticates the WIRE, not the STORAGE: production runs the lockbox container with no sealed monotonic state, so one `UPDATE … sig_count = sig_count − 1` absorbs a hidden rival and the receiver holds a VALID signature affirming the wrong number | **A naive hash chain does NOT close it.** An append-only chain `h_n = H(h_{n−1} ‖ sid ‖ n)` published in the attestation is a PURE FUNCTION of `(sid, n)` given a fixed `h_0` — an operator who rolls back to `k` and re-advances regenerates the IDENTICAL `h_k` and `h_{k+1}`, so no honest party can ever hold a contradicting head and the chain detects nothing. It works only if each link commits to per-round data the owner WITNESSED — the round's session bytes, already the primary key of the lockbox's partial-signature cache — so that re-advancing over different traffic produces a head a prior receipt contradicts. The second remedy, a two-store cross-check of the coordinator's `finalized` against the attested count, is sound BUT ORDER-SENSITIVE: read the coordinator FIRST and the attestation SECOND and refuse iff `attested < finalized`; the reverse order refuses an honest coin whenever a co-signature lands between the two calls |
| **L-9** | **One lost co-sign reply strands a coin's off-chain life** (SM-1) | NOT G4 or G8 — nothing is confiscated and the value is recoverable unilaterally. What is lost is the coin's cooperative life: the census counts sighashes but accepts only signed transactions as disclosure, so the coin stays exactly one slot short (it does not compound) and descendants inherit the refusal | persist `{sid, unsigned tx, msg, session, own partial}` and make the idempotent re-serve normative at EVERY gate in front of the SE, not only at the lockbox; plus a census self-check at wallet open that reports DEGRADED rather than idle |
| **L-10** | **A flat backup may legally carry an RGB transition, and nothing binds its assignment** (RGB-1) | G5 wherever a coin still travels FLAT | the coloured lane removes the flat backup's role entirely — this tracks §0.4 V-1, which is now DONE wherever an attestation identity is pinned and still open on every network awaiting an enclave (V-6). **What is not yet checked here is whether a COLOURED-laddered carrier still retains a flat backup chain of its own**; until that is measured, treat this row as narrowed rather than closed |
| **L-11** | **The coloured lane's economics are denominated in carrier sats while the loss is denominated in ASSET value** (RGB-2), and a plain flat backup over a carrier is a BURN rather than a transfer | G5 | admission floors that price the asset, not the carrier. Today the mitigation is structural: the automatic passes never re-anchor a carrier, they SEVER it |
| **L-12** | **The child lane has no unknown-version reject arm, and the sender picks the floor** (A-12) | G10 and, through it, payment finality: a conveyance at a version carrying neither the key handover nor the transfer signature lets a payer keep the child auth key while the payee books the payment | a two-sided version check on the child lane and a floor the RECEIVER sets. The FFI performs the downgrade itself today, hard-coding the pre-ladder fields in both directions |
| **L-13** | **Split-tree per-epoch materialisation is an uncharged on-chain rent** (VE-1) | the footprint economics of §7, not a safety goal | price it, or bound the tree shape that can be minted |
| **L-14** | **Batch atomicity**: `transfer_many` / `batch_transfer_tokens` hand off pieces independently | no all-or-nothing across recipients. A dropped hand-off leaves that piece reclaimable by the sender — the split parent is terminal, so there is no double-spend | an atomic multi-piece hand-off. This is the only remaining `transfer_many` caveat; its laddered-parent routing is correct (REQ-27) |
| **L-15** | **Unilateral-exit fees** — a FLAT exit (branch + absolute-locktime backup) broadcasts fixed-fee transactions with no bump | G4 under a fee spike | the package path is BUILT and live-verified but needs a funded UTXO, a signer and a Core RPC endpoint, so a keyless tower cannot use it. **The child lane's bump variant DOES exist** — `exit_child_pass_with_bump` and `exit_spine_tip_pass_with_bump`, both wired into `unilateral_exit`. The gap is the WATCH half: `watch_child_pass_seen` and `watch_spine_tip_pass_seen` have no bump variant, so a tower defending a child tier is stuck at the rate it was signed at |
| **L-16** | **Blind-SE ancestor binding** — the SE stores no per-sid funding outpoint, so a receiver cannot bind `terminal_parents` ids to specific outpoints | the FLAT (branch-carrying) lane's defence against SUBSTITUTION of terminal decoys (omission is defeated by the count check) | superseded on the laddered shape by the census, which binds to the attested counter rather than to named ids. Tracks §0.4 V-1, so it is narrowed to the networks still awaiting an enclave (V-6) rather than closed outright |
| **L-17** | **Amount width** — coin sats are booked as `u32`; a single coin above ~42.9 BTC would truncate | nothing at the intended per-coin sizes; it is not guarded | widen the type |
| **L-18** | **Mint concurrency** — `mint_tokens` isolates the fresh allocation by a before/after snapshot and deliberately does not hold the wallet lock across its on-chain wait | a concurrent same-asset receive into the SAME wallet during a mint could be misattributed | issuers must not mint and receive the same asset concurrently |

### 14.3 Measured limits — true of the design, not defects in it

These are consequences of arithmetic. Under §0.1 a measurement overrules a design statement, so they
are stated as limits rather than as things to fix.

* **Split depth is capped at 8 on mainnet (19 transactions)**, 54 on regtest (111). Deeper
  children are unadoptable at every tip, and the build side refuses to mint what the receive side
  would refuse.
* **Hop capacity is 100 decrements, of which 99 are USABLE.** Hop 100 lands the locktime
  exactly on the co-sign anchor, and the receiver refuses `lock_time <= tip`.
* **K = 1 bounds the payees of one coloured PAYMENT, not the payments of one carrier.** The
  sender's change lands on a spine tip, and that tip is payable again.
* **The P2A anchor slot is an auction, not a race.** An under-paying squat is refused; an
  over-paying one RAISES the tier's effective feerate at the attacker's expense. TRUC contention is
  a price, not a denial of service.
* **There is NO operator capital requirement (§5.5).** This bullet asserted a standing float of
  `μ · W / epoch_days · TVL` — ≈ 9 % of TVL — for as long as the discharge round existed. The round
  is deleted, and the float went with it: it traced to exactly one ordering requirement, that a
  funded successor root be CONFIRMED before holders could migrate onto it. With no successor root
  there is no window to carry. A tree now CLOSES when its root owner decides, paying every unreleased
  leaf out of `F` itself, so the closer fronts nothing. **The figure is kept here, struck through in
  words rather than deleted, because a summary that silently drops a number readers may have quoted
  is worse than one that says it was withdrawn.**
* **THE DESIGN SELLS PAYMENT VELOCITY — and it LOSES to a batched on-chain payout until ~74 % of
  leaves are swept.** Full derivation in
  [PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md) §0.2.

  **The comparison must be against a BATCHED payout** — one-to-many on chain is ONE transaction with
  `N+1` outputs, 4 412 vB for N = 100 (~44 vB per recipient), not `N` transactions.

  **And the sweep is not free: acquiring a leaf IS an onward hop**, so sweep fraction `s` and hop
  count `h` are coupled (`h ≥ s`). One coin to 100 recipients, all settling:

  | sweep fraction | Utexo | all on-chain | |
  |---:|---:|---:|---|
  | **0.00** | 29 800 vB | **4 411 vB** | **ON-CHAIN wins 6.8×** |
  | 0.70 | 16 396 | 15 191 | on-chain |
  | **0.74** | 15 627 | 15 807 | crossover |
  | 1.00 | 10 628 | 19 811 | Utexo **1.87×** |

  **The design rule:** a piece received and immediately cashed out should never have been an
  off-chain split — a batched on-chain payment is strictly cheaper. And the sweep does not optimise a
  winning position, it CREATES one: below ~74 % coverage the split lane loses on block space.

  Note what is NOT affected: the per-leaf VALUE recovery (§5.3, ~1 057 sat at 3 sat/vB) is a satoshi
  quantity independent of K and of this aggregate. The sweep's commercial case stands even where its
  block-space case does not.

* **PER-PAYMENT COST BY LANE — and the LEAF lane is the normal one.** Payments are arbitrary amounts,
  so every non-exact payment is an in-ladder split and the recipient receives a CHILD. A root holder
  is the DEPOSITOR, or the rare payee of an exact-amount transfer. **After the first payment,
  everyone downstream holds a leaf.**

  | who | block space per payment | against ~154 vB on chain |
  |---|---:|---|
  | **leaf, spent onward off-chain** | **0** | this is the product |
  | **leaf, swept by an SSP** (§5.3) | 58 vB | 0.38× |
  | **leaf, WALKED out — the shipped default** | 250 vB | **1.62× WORSE** |
  | root holder — the depositor ONLY | ~589 vB/yr ⇒ 5.9 at 100 payments/yr | 26× |

  **The root row MUST NOT be quoted as the typical case.** It is the most flattering number in the
  model and it describes a population that barely exists once payments start flowing. The honest
  headline is the row above it: **on the shipped default, settling a payment costs MORE block space
  than making it on chain.** §5.3's sweep and §5.4's round are what change that — which is why the
  sweep is not an optimisation but a precondition for the median user's economics.

* **THE ON-CHAIN CADENCE IS REAL, AND IT IS THE FLAT CALENDAR — not the tier chain.**
  This is the number to quote when someone asks what the design saves, because "zero rent" is true of
  the tiers and false of the coin. On the mainnet profile (`initlock = 10 000`, `interval = 100`):

  | | |
  |---|---|
  | a coin's calendar runway from a fresh anchor | **10 000 blocks ≈ 69 days** |
  | what each whole-coin hop costs of it | **100 blocks** — so 100 decrements, **99 usable** |
  | what one re-anchor costs | **one 112-vB transaction** |
  | so one on-chain transaction buys | **min(99 off-chain payments, 69 days)**, whichever binds first |
  | amortised | **≈ 1.13 vB per payment**, against ~110 vB for a plain on-chain payment — **≈ 97×** |
  | idle rent | **≈ 589 vB per coin-year**, against ≈ 5 840 without the tier chain — **≈ 10×**, not ∞ |

  A SPLIT PIECE does not get its own 10 000 blocks: it inherits what remains of the parent's, which
  is why its exit deadline is a height the splitter owns (L-2, §14.1). And each hop spends calendar
  whether or not time passes, so a coin transferred 99 times must be re-anchored no matter how young
  it is.

* **The floors are rate-evaluations, not constants.** At the shipped `committed_fee_rate = 3.0`:
  plain rung 615, coloured rung 744, `min_child_value` 1 560, `min_spine_tip_value` 945, plain root
  floor 2 175, coloured ROOT 2 562, coloured CHILD 1 818. Quoting one without its rate is
  quoting a rate.

---

Unit tests live in `clients/libs/rust-sdk/src/*` (`#[cfg(test)]`); E2E dispatch via
`SDK_E2E`/`RGB_E2E` in `clients/tests/rust`; upstream Mercury suite runs by default.
