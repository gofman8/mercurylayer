# Mercury Utexo — Protocol Specification

**Status: normative draft, 2026-08-14.** Requirements are labelled **REQ-n**, invariants **INV-n**,
error semantics **ERR-n**; keywords MUST/SHOULD/MAY per RFC 2119. Every labelled statement maps to a
verifying test in [§12 Traceability](#12-traceability) or is marked UNPROVEN in place.

---

## 0. How to read this document

### 0.1 Authority order — the DESIGN is normative, and the CODE follows

Set by the owner, 2026-08-13, and it inverts the rule this document was started under
("specify what the code does and delete the aspirational prose").

**Where this specification and the implementation disagree, the implementation is what changes.** A
divergence is therefore a defect with an owner, not a licence to weaken a sentence here — and not
something a reader may discover by accident, so every one that is known is listed in §0.4.

Two things follow, and they are the reason the rule is worth stating rather than assuming:

* A section MAY specify behaviour that is not yet built, PROVIDED §0.4 records that it is not. What a
  section MUST NOT do is describe an unbuilt thing in the present tense.
* A measurement MAY NOT be overruled by a design statement. Where the design says one thing and a
  measurement says the design is not achievable — the depth cap of [D53], the payment granularity of
  [D44], the coloured lane's economics — **the measurement wins and the design changes.** Design
  authority is over choices, not over arithmetic.

### 0.2 What a normative statement in here rests on

This corpus has repeatedly caught itself publishing claims that were true of a plan, of an older
commit, or of a document rather than of the system. Three rules, each of which exists because it was
broken:

1. **Every claim names its evidence, and evidence means a test that RUNS.** A test that asserts a
   description passes while the construction underneath it is wrong ([D42]); a source scan may assert
   presence, absence, ordering and window shape but never reachability, binding or behaviour
   ([D64]) — behaviour is proven by planting the historical defect and running the real checker.
2. **A measurement carries its date and its target.** "756 tests" named no target set and was
   therefore neither wrong nor checkable. Test counts in this document say what was counted.
3. **A rewritten assertion is a NEW assertion and must be run before it is cited** ([D70]).

### 0.3 Scope, and the shapes a coin can have

In scope: the SE (Mercury coordinator + lockbox), the client libraries (`mercurylib`,
`mercuryrustlib`, `mercury-rgb`), the wallet SDK (`mercury-utexo-sdk`), the SSP service
(`mercury-ssp`), and their Bitcoin/RGB/Lightning interactions. Companion normative documents:
[PROTOCOL.md](PROTOCOL.md) (tiers, renewal, terminal-freeze), [CHILDREN.md](CHILDREN.md) (first-class
split children), [LIGHTNING.md](LIGHTNING.md) (the Lightning latch), [TRUST-MODEL.md](TRUST-MODEL.md)
(the trust unit and the named residuals).

**There is ONE protocol.** `claim()` establishes a TES-R exit ladder (§2.6) for every fresh confirmed
**root** coin, unconditionally **once an enclave attestation identity is pinned — see §0.4 V-6, which
is not a footnote: with the shipped defaults NO ladder is established on ANY network**; the
`deposit_protocol_version` field and the `UTEXO_PROTOCOL_DEFAULT`
escape hatch that could opt a deposit back into the pre-TES-R shape are DELETED.

**The target is ONE COIN SHAPE.** The shipped build has two, and the second is transitional:

- **Laddered** — every plain deposit. Its exit is the relative-CSV tier chain (§9.2): 0 vB of idle
  rent, and it never matures while idle (INV-27). It is NOT deadline-free — the flat backup chain is
  retained, so the coin keeps an absolute-locktime calendar which each whole-coin hop shortens by
  `interval` [D36]. INV-27 states the exact scope.
- **Un-laddered** — an RGB **carrier** whose ladder has not been coloured (a plain tier spend would
  destroy the allocation — terminal-freeze, INV-29), and a split sub-coin whose funding is
  un-broadcast and therefore cannot root a trigger [B0]. These keep the signed-once backup chain
  (§2.4) and transfer by backup-chain + branch handover.

The mechanism that removes the second shape is **CTES-R** — colour every TES-R tier, so a carrier is
laddered like any other coin and terminal-freeze retires with it. Its gate passed against the live
stack ([CTESR-GATE.md](CTESR-GATE.md)) and it is BUILT; what it is not is DEFAULT (§0.4, row 1).
Material scoped to the un-laddered shape — absolute deadlines, backup-chain handover, terminal-parent
proofs, the carrier-depletion arithmetic — is expected to be **deleted**, not migrated.

### 0.4 Divergence register — where the code does not yet meet this document

Each row is a defect in the CODE by §0.1. A row is removed only when the divergence is closed, never
when the sentence is softened. Nothing here is a hidden caveat: each is also stated where it bites.

| # | This document specifies | The shipped build does | Consequence, and what closes it |
|---|---|---|---|
| **V-1** | one coin shape: every coin, carrier or not, carries a coloured ladder | `SdkConfig::colored_ladder` ships **false** [D30], so a carrier stays un-laddered by default | The un-laddered lane and everything scoped to it stay load-bearing. What gates the flip is not safety but measured economics — [PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md): one coloured partial payment per carrier, and a long unilateral exit for the child it produces |
| **V-2** | every client verifies a conveyed ladder | the nodejs and web clients **refuse** any transfer that DECLARES a ladder, and fall through to the un-laddered `num_sigs == backups.length` check against a coordinator-supplied `interval` | Those clients are the UN-DEFENDED population, not an exempt one — the refusal keys on three SENDER-supplied fields, so it is a refusal of declared ladders, not a structural one. Closed by porting `verify_bundle` to wasm/JS and Kotlin. Background: [COLORED-FORWARDING.md](COLORED-FORWARDING.md) |
| **V-4** | the `statechain_id ↔ aggregate` binding is attested, like the count | it is coordinator-supplied and unattested (P1) | A coordinator serving NULL downgrades any coin to the un-laddered lane; serving a wrong value is not detectable in-protocol. [D69] closed the COUNT's half of this ([TRUST-MODEL.md](TRUST-MODEL.md) B11); P1 is the half that remains |
| **V-6** | every fresh confirmed root coin is laddered by `claim()` | **no network ships a pinned enclave attestation identity**, so on the shipped defaults EVERY laddering claim refuses and the coin stays flat | `TesrParams::attestation_identity_const` returns `None` for bitcoin/mainnet, testnet/testnet3/testnet4/signet AND regtest; `SdkConfig::regtest` and `SdkConfig::mainnet` both ship `attestation_identity: None`; the only other source is the `UTEXO_ATTESTATION_IDENTITY` environment variable, which an embedder has not set. The pass then records `LadderSkipReason::AttestationIdentityUnpinned` and continues — correctly, since verifying an attestation against a coordinator-served key proves nothing ([D69]) — but the effect is that "unconditionally" is conditional on configuration the product does not supply. Closed by compiling in a pin per network at release, or by an operator setting one |
| **V-5** | the two closed forms of [D40.3] are evaluated and published | UNEVALUATED | An external dependency, not unfinished work: both are queries over DEPLOYED coins and regtest has none that mean anything |

### 0.5 What this document does NOT claim

* **No in-protocol payment atomicity for a plain transfer.** A transfer is a one-way handover — a
  gift, not an escrow (TRUST-MODEL B8). Delivery-versus-payment needs the Lightning latch (§8) or an
  invoice.
* **No defence against the statechain trust unit itself** (SE + a past owner with a retained
  pre-rotation share, TRUST-MODEL B1). A fresh co-signature needs no backup, so no timelock reaches
  it. What the ladder changes is the notice period, not the possibility.
* **No claim that a sub-economic piece is final.** An ancestor's lowest backup rung voids an entire
  tree for the cost of one transaction, and does so whether or not its holder means to
  ([SUBECONOMIC-FINALITY.md](SUBECONOMIC-FINALITY.md)). **Read L-2 with it:** a piece holder holds the
  parent's `T`, which spends the same `F` and carries no timelock, so a payee who acts can PRE-EMPT
  the backup across the whole window and always keeps their money. The option is free only BELOW
  break-even, where the walk costs more than the piece is worth — which is what "sub-economic" means.

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
is no longer taken on trust: it arrives under the enclave's `utexo/sig_count/v2` signature, verified
against the **PINNED enclave attestation identity** ([D69] — not the chain-anchored per-coin key this
clause used to name, because a deep in-ladder-split ancestor has no chain anchor by design), over a
nonce the receiver itself chose (§3.3, REQ-38). Plus a liveness duty on the owner (or a delegated tower) that
differs by coin shape — an **un-laddered**
coin must be exited before its backup locktime floor / epoch deadline; a **laddered** coin has no
deadline at all, but its defender must react within the CSV edge once someone publicly broadcasts
the trigger (§9.5, INV-28). No custody rests on the SE in either case, and nothing ever expires to
the operator.


### 1.1 Adversary model

Eleven adversaries are modelled. The two columns that matter are what each one CONTROLS and what it
provably cannot do; a full treatment with the mechanism behind each entry is in
[`spec-work/SPEC-FOUNDATION.md`](spec-work/SPEC-FOUNDATION.md) §1.

| # | Adversary | Provably cannot | Can still do — and this is the residual |
|---|---|---|---|
| **X-1** | Prior owner of THIS coin | produce any new co-signature under `A` (the handover rotates the SE share and re-points the auth key); win the CSV race from behind; hide a co-signed rival from the census | broadcast the no-timelock trigger `T` purely to grief, choosing the moment (§9.5) |
| **X-2** | Prior owner of an ANCESTOR / the splitter | co-sign anything further over the parent (the budget ratchet makes it terminal); mint a rival to a child already handed over | **void a sub-economic piece with ONE 112-vB backup, at zero marginal cost per extra piece, and no operator can stop it** — the transactions are already signed. See §0.5 |
| **X-3** | The paying sender at conveyance | substitute a decoy funding output or sid; declare a soft CSV; drop an ancestor segment; pad the flat backups or the superseded set; skim value out of the tier chain; widen the schedule ([D27], `cap_schedule`) | convey at a version that carries no key handover (A-12). **NOT the forged `fee_rate`** — that was this row's headline residual and [D72] closed it on BOTH synchronous verifiers, ahead of every value law; see §14.4 |
| **X-4** | Receiver / payee | claim twice, claim after cancellation, or reverse a claim (the enclave keyupdate is irreversible and the counter monotonic) | nothing the model defends against — note the asymmetry in §0.5: a transfer is a gift, not an escrow |
| **X-5** | The blind SE enclave alone | steal (it never holds a full key); see ANY value; forge exit material after the fact; un-terminate a node; produce a second partial from a replayed session | refuse to co-sign — which is a freeze, not a seizure: the unilateral tree is pre-signed and SE-independent |
| **X-6** | The coordinator alone | under-report `num_sigs` or the budget — closed by the attested count, and by [D69] the verifying key is pinned rather than served | serve a wrong or NULL `aggregate_xonly` (§0.4 V-4); serve a wrong `x1_pub`, which BRICKS the coin for everyone; drop the pending-transfer lock; withhold or reorder the mailbox |
| **X-7** | **SE + a past owner with a retained pre-rotation share** | — | fresh-co-sign an immediate spend. **No timelock reaches this**: a fresh signature needs no backup. This is the statechain trust unit (TRUST-MODEL B1) and it is irreducible, not a defect |
| **X-8** | Watchtower (delegated) | move funds — a keyless tower holds no key and broadcasts only the owner's own pre-signed material | fail to act, which costs a race; and a KEYLESS tower cannot fee-bump at all ([D31]) |
| **X-9** | Miner / mempool adversary / pinner | make a tier invalid | keep it unconfirmed. The P2A anchor slot is an AUCTION, not a race ([D45]): an under-paying squat is refused and an over-paying one raises the tier's effective feerate at the attacker's expense |
| **X-10** | RGB counterparty (consignment sender, issuer, proxy) | forge a consignment the client validates — client-side validation is the only authority, and no proxy, SE or issuer is trusted for token rules | withhold a consignment (a liveness failure, not a theft) |
| **X-11** | Lightning SSP | take custody — the swap is atomic (§8) and the pre-payment gate binds recipient and amount | see every value in the swap |

**One thing the model does NOT have: an adversarial test that plays the COORDINATOR.** Every
adversarial test in this repo plays a malicious sender. X-6's "can still do" column is therefore
argued, not exercised, except where [D69] and the terminality tests now cover it.

### 1.2 Security goals

Twelve properties. Each states its own scope limit, because a goal stated without its limit is the
form of claim this corpus keeps having to retract.

| id | kind | property | scope limit — read this with the property |
|---|---|---|---|
| **G1** | safety | **VALUE CONSERVATION.** On every acceptance path, Σ(payload outputs) equals the tier total derived from the PARSED value of the output the tier spends; the residual is exactly one P2A anchor plus at most one zero-value opret; the chain is anchored hop by hop back to the funding value read FROM CHAIN | the yardstick must be receiver-derived, and since [D72] it IS: `verify_bundle_ex` binds `bundle.fee_rate` to `TesrParams::for_network(..).committed_fee_rate` before any value law, and the child lane inherits it because `verify_child_bundle` re-verifies its embedded parent through the same function. The residual is the child lane's inverted DIRECTION — there INFLATION is the attack, because the amount comes from an un-broadcast `SP.out[j]` |
| **G2** | safety | **NO UNDISCLOSED SPENDING PATH.** No co-signature under the coin's aggregate exists that the receiver was not shown. Three conjoined obligations, never the equation alone: (a) exact equality against an ATTESTED count; (b) a per-item battery on every superseded entry; (c) slot uniqueness over the union of live and disclosed tiers | bounds spending PATHS, not VALUE — G1 carries the value claim. Treating it as arithmetic has bitten twice: junk padding, and a genuine tier disclosed twice inflating the expected total for free |
| **G3** | safety | **OLD STATE DIES.** Every superseded state is disclosed and provably out-raced: the live state carries a strictly-lower CSV over the same outpoint, and every pre-renewal state hangs on a parent that can never confirm | **race-conditional, not axiomatic.** The live rival must also be RELAYABLE ([D26]) — co-signing a rival at a rate that cannot relay loses a race the verifier believes it wins |
| **G4** | liveness | **EXIT AVAILABILITY.** The current owner can always reach L1 with pre-signed material alone — no counterparty, no SE call, no key held by anyone else — in `3 + 2d` transactions at depth `d` | bounded four ways, all measured: FEE (above the committed 3.0 sat/vB a tier needs a CPFP child, and a keyless tower cannot build one — [D31]); VALUE (below `V_min` the walk costs more than the piece); DEPTH (mainnet cap **8**, 19 transactions — [D53]); COLOUR (a coloured coin's re-anchor is a manual call nothing schedules) |
| **G5** | safety | **ALLOCATION INTEGRITY (RGB).** The allocation the receiver validated is the one that settles. RGB transitions anchor only in signed-once transactions or coloured tiers; a PLAIN tier spend of a carrier destroys the allocation | rests entirely on a carrier never being laddered plainly. That is why the automatic passes exclude carriers, and why the sever route exists ([D46]) |
| **G6** | safety | **FINALITY.** A completed claim is irreversible: the keyupdate cannot be undone, the counter is monotonic, and claim/cancel are mutually exclusive | finality is at CLAIM, not at conveyance. A conveyed-but-unclaimed transfer is reversible for the lock window — and is already presented to an SSP as if received |
| **G7** | safety | **NON-CUSTODY UNDER OPERATOR COMPROMISE.** A hacked operator may take FUTURE deposits and FUTURE transitions; pre-hack state left untouched is safe. Every spend needs both shares, and the hacked SE holds only the post-rotation one | the surviving path is X-7. This is the requirement that disqualified the shared-root factory architecture, where one confirmation confiscates every coin under the root |
| **G8** | safety | **NO CONFISCATION BY DESIGN.** Nothing expires, nothing sweeps to the operator, no output pays the operator by timeout. Missed liveness costs a race, never a forfeiture | under pressure at exactly one place: the sub-economic leaf is not confiscated by the OPERATOR, but it is forfeit to the party who split it |
| **G9** | safety | **SE BLINDNESS.** The SE signs 32-byte sighashes; a coloured sighash is byte-indistinguishable from a plain one; consignments stay P2P | covers CONTENT, not traffic — the SE still learns sids, auth keys, counts, flags, timing and the caller's endpoint. Blindness is also why every operator-side value fix is impossible: "the SE refuses to co-sign a piece below a floor" is a WRONG proposal and must not be re-proposed |
| **G10** | safety | **AUTHORIZATION INTEGRITY.** Only the current owner authorizes an irreversible operation, single-use and endpoint-bound | the single-use nonce is deployed on **FOUR** endpoints — `withdraw/complete`, `deposit/get_derived_token`, `statechain/spend_budget` and `transfer/cancel` — and every OTHER mutating endpoint takes a static, replayable signature |
| **G11** | safety | **ADMISSION SOUNDNESS.** A receiver never admits a coin whose exit provably cannot complete, and every term of an admission test is receiver-derived — a serde field is not admissible | enforced in TIME and in STRUCTURE; the uncovered dimension is VALUE. `min_child_value` is not a floor that ignores economics — it IS `V_min` evaluated at the shipped rate (1 560 sat at 3.0 sat/vB), correct at that rate and no other |
| **G12** | liveness | **ZERO IDLE COST ON THE CSV SIDE.** All tiers are un-broadcast, `T` carries no timelock, and CSV does not tick until the parent confirms — so the TIER CHAIN adds 0 vB of rent and no deadline, however long the coin or the DAG sits idle | **this is NOT "the coin never touches the chain", and reading it that way is the [T-1] error.** A laddered coin RETAINS its flat backup chain, whose locktimes are ABSOLUTE and do age — so the flat calendar, not the CSV hop budget, is what sets the real maintenance cadence ([T-2]). See §14.3's cadence entry for the measured numbers. A leaf is worse: its clock is the parent's lowest backup rung, a height belonging to the splitter |

### 1.3 Assumptions

Every goal above holds only under these. A specification that states goals without stating these
claims more than it can deliver.

| id | assumption | if false |
|---|---|---|
| **A-1** | **FEE MARKET** — the market rate stays at or below the committed **3.0 sat/vB** long enough for each tier to relay and confirm inside its head start; where it does not, someone can attach a ~153-vB v3 child to the tier's 240-sat P2A anchor and submit a 1P1C package | the CSV edge degrades into a fee race a fixed-fee tier cannot win. The remedy is BUILT and live-verified but not universal: it needs a funded UTXO, a signer and a Core RPC endpoint (electrum has no `submitpackage`), so a keyless tower has no move ([D31]) and the child lane has no bump variant |
| **A-2** | **CHAIN LIVENESS AND RELAY POLICY** — blocks are produced; v3/TRUC, P2A, 1P1C and sibling eviction are honoured; no reorg deeper than `confirmation_target` | v3/P2A/1P1C are relay POLICY, not consensus: a policy change can make a pre-signed tree un-relayable. TRUC's one-ancestor rule already bites — a second rescue funded from the first's unconfirmed change is refused at any price |
| **A-3** | **ENCLAVE BEHAVIOUR** — old shares are destroyed at every transfer, the secnonce is one-shot, the budget ratchet only lowers, and attestations come from the enclave's pinned identity ([D69]) | this is X-7 / TRUST-MODEL B1 and it cannot be verified: any proof of erasure attests one instance of the data. There is no client-facing attestation of the enclave itself, and production runs the plain-C++ lockbox container, not SGX |
| **A-4** | **COORDINATOR HONESTY** for the facts only it holds — the sid ↔ aggregate binding, `x1_pub`, mailbox behaviour, the pending-transfer lock, both sign gates, batch atomicity | nothing in the protocol detects a violation of most of these (§0.4 V-4). The one item that IS closed is the count and budget |
| **A-5** | **RGB VALIDITY** — client-side consignment validation is sound and complete | R8 collapses and a forged consignment books a wrong asset or amount; the receiver has no other authority |
| **A-6** | **OWNER OR DELEGATE AVAILABILITY** — someone is awake within the CSV window once a trigger is broadcast, and inside the margin before an un-laddered coin's absolute deadline | delegable and redundant, but NOT removable: timelock security is defined by acting before maturity. The pre-TES-R unconditional no-watch window is gone — a received coin is watched from receipt, forever |
| **A-7** | **THE USER'S CHAIN VIEW** — the indexer honestly reports tip, spentness, confirmations and fee rates, and delivers broadcasts | it can blind and delay but not steal. Worse in practice: the dev defaults are plaintext, and an on-path attacker is strictly stronger than a lying indexer |
| **A-8** | **PARAMETER PROVENANCE** — the CSV schedule and the flat-ladder parameters are COMPILED IN per network, never coordinator-served | the coordinator would define the defence. The tempting fix — derive the interval from the conveyed chain — is CIRCULAR and accepts exactly the padding it exists to stop. Cost of the fix, stated plainly: a config typo is now a fleet-wide outage rather than a quiet weakening |
| **A-9** | **LOCAL STATE DURABILITY** — the owner retains `wallet.db` and, for token wallets, the whole RGB data directory | loss of local state is loss of funds, and the mnemonic alone is NOT a backup. The SE is blind and cannot re-serve exit material — that is the privacy design, not an oversight |
| **A-10** | **SINGLE LIVE INSTANCE PER WALLET** | two processes on one wallet can broadcast stale state against each other. The lock is in-process only and the blind SE cannot arbitrate. Bundle restore is disaster recovery, not device sync |
| **A-11** | **THE RETAINED CHECKS SUBSUME the unverified blinded-MuSig commitments on the laddered lane** | UNANALYSED — commission the analysis. What keeps the two lanes' residuals from composing is that the legacy arm still runs the full legacy verifier |
| **A-12** | **CLIENT CONFORMANCE** — every client that can receive a coin either verifies the ladder or refuses it BY NAME, and a decoder rejects a version it does not implement | **[D16] the unknown-version reject arm EXISTS and is exact-set** — `ADMISSIBLE_PROTOCOL_VERSIONS = [0, 2, 4]` and `admissible_shape` refuse anything outside it BY NAME ("its numeric ordering carries no meaning, so an unknown value cannot be 'at least' anything"), at the top of `validate_encrypted_message` (the claim path) and of `prepay_flat_census`. What remains is narrower and real: the CHILD lane reaches neither call — `prepay_child_census` has no shape check and gates with `<`, so every value in `[4, u32::MAX]` clears it, and `validate_encrypted_message`'s child block returns before that function's check. Inert today (v99 selects the arms v4 does) and exactly the ordinal reading [D16] forbids. The FLOOR is the worse half: the sender picks it, and a low-version conveyance carries neither the key handover nor the transfer signature |

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
status polling MUST accept that combination (treating it as an error made every later poll fail for
the life of the coin; defect found and fixed during this migration).

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
100 decrements of capacity either way, of which **99 are usable** — hop 100 lands on the co-sign anchor and the receiver's `lock_time <= tip` rule refuses it [D62]). The coordinator's own copy is a cross-check only: `info_config`
REFUSES the call outright if the two disagree. Taking `interval` from the coordinator would let the
coordinator define the defence, and deriving it from the conveyed chain is circular — a padded chain
of uniform `interval/2` hops validates against itself, which is the padding INV-5 exists to stop.

For an **un-laddered** coin this chain IS the exit material, and it is a finite budget (`initlock`
blocks, spent by each transfer and by wall-clock time). When it nears the floor the coin must be
moved to L1 by exit (§9), materialized if it is a carrier (REQ-33), or COOPERATIVELY re-anchored
on-chain via `refresh` (§9.4, REQ-31).

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
(`committed_fee(rate)` over `TIER_VBYTES = 125` — the MEASURED signed vsize; the earlier 124
modelled a 64-byte `SIGHASH_DEFAULT` witness, and TES-R signs `SIGHASH_ALL`, so every tier carries
the explicit 65th byte [D4]) so it relays standalone, and a 240-sat P2A anchor for live-rate
fee-bumping.

> **A multi-child split state pays MORE than that constant.** `TIER_VBYTES` prices a one-payload
> tier; an `SP` carrying `n` children is charged `committed_fee_for_outputs(n, rate)` over
> `TIER_VBYTES + (n − 1)·P2TR_OUT_VBYTES` — 43 vB per extra child — and `build_split_state_from`
> refuses unless `Σ children` equals `tier_out_total` computed on exactly that. Quoting
> `committed_fee(rate)` for an `SP` understates its fee by `(n − 1)·43·rate`.

**INV-27 (idle coins never age — ON THE CSV SIDE)** No tier is on-chain, and a BIP-112 relative
lock does not tick until its parent confirms, so no tier anywhere matures until someone broadcasts
`T`. An idle laddered coin — and an idle split DAG — therefore costs **0 vB of rent** and its exit
chain is unchanged by the passage of time.

**This is a statement about the tiers, not about the coin** [D36]. A laddered coin also retains its
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
renewal, and the code does not try (INV-6). What actually stands beside consensus is the receiver's
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

### 3.0 CENSUS COMPLETENESS, and the shape obligation that discharges it [D47]

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

**[D59] The table below names THREE of those eight properties as enforced on an acceptance path.
That count is WRONG — it under-counts by at least four.** The direction is safe (it claims less
enforcement than exists), but the harm is not: this section reads as an exact audit and tells a
maintainer to re-check only three rows. Also enforced, read in the code bodies rather than in their
doc comments:

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
conveyed-child verifier — not on orphan helpers. The table's three rows are correct about themselves;
what is wrong is the word "exactly" and the claim that the other five are mere conventions.

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

⚠️ **The consequence for the warning below.** It used to read "a change to any shape rule above MUST
be re-checked against premise 4", which reads as though all eight were load-bearing. Two of them —
tier nVersion 3 and tier `nLockTime` 0 — could be relaxed today with nothing in the tree failing.
Re-check against premise 4 means re-check against the THREE rows marked enforced; changing a builder
convention is a change to what this document DESCRIBES, and it will not be caught by a test.

**Those rules therefore carry a CENSUS obligation and not only a relay/race one.** This is the whole
point of stating it here: every one of them already exists for a transport reason, and the failure
mode is that a future change relaxes one for a perfectly good relay-side reason, the two categories
stop being distinguishable, and the census silently begins counting one thing as another. A change
to any shape rule above MUST be re-checked against premise 4.

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
  single-use endpoint-bound owner challenge (`"<nonce>:<sig>"`, audit [15]); one consumed nonce
  authorizes the whole `count` batch. Never routed to the token server; works on any network.
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

  > **[D69] The verifying key is pinned, not chain-anchored, and the difference is the whole point.**
  > This clause used to read "verified against the chain-anchored `enclave_public_key` the receiver
  > already bound to `tx0`". True for a coin that IS on chain — and a depth-≥2 in-ladder-split
  > ancestor's funding output is **deliberately un-broadcast**, so for those ancestors there was
  > nothing to bind to and the verifying key arrived in the same response as the signature. The
  > enclave now signs every attestation with one long-term identity
  > (`utexo/attestation-identity/v1`, published at `GET /attestation_identity`) and the client pins
  > it, so the check is independent of the coordinator's word AND of whether the coin is on chain —
  > it holds at every split depth. Resolution is **pin → config → refuse**: a compiled-in pin is not
  > overridable, and "neither" is a refusal, never a fallback to the served key. A missing attestation, a mismatched key, or a
  `has_sig_budget` the enclave cannot state is REFUSED, not defaulted (`get_statechain_info`,
  `verify_sig_count_attestation`; there is no phased rollout, D23). Without it a coordinator that
  under-reported `num_sigs` by `k` would hide `k` co-signed rival states while the exact-equality
  census still balanced.

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

> **The order matters and the earlier text had it backwards** ("waits `confirmation_target`, creates
> the first backup tx"). The backup exists from the mempool sighting, NOT from confirmation, so a
> deposit that never confirms still leaves a signed backup — and the ladder, which does wait for
> `CONFIRMED`, is the only part gated on confirmations.

**REQ-14** A deposit slot MUST consume a deposit token; if payment is required the SDK MUST surface
`SdkError::TokenPaymentRequired` rather than silently proceeding (ERR-6).
**REQ-37 (ladder establishment)** `claim()` MUST establish a TES-R ladder for every CONFIRMED,
non-duplicate, non-`single_use` ROOT coin that has none — unconditionally, and idempotently (a coin
that already carries a ladder is skipped, so repeated passes never double-sign). The exit payee MUST
be the coin's own seed-derived `backup_address`, never an out-of-wallet address. Two exclusions are
BY DESIGN, not leftovers: an RGB **carrier** (INV-29) and a coin whose funding `F` is not on-chain
([B0] — a sub-coin's trigger would have no prevout to spend, leaving it unexitable). Both exclusions
MUST fail CLOSED: if the carrier set or `F`'s on-chain status cannot be resolved, skip the pass and
retry next claim — a missed ladder is harmless, a laddered carrier or sub-coin is not. An
establishment failure leaves the coin un-laddered and still exitable by its signed-once backup.
`sdk48` (auto-established, exits to the seed-derived key, a second `claim()` does not
double-establish), `sdk52` (carrier never laddered).
**REQ-35 (derived slots)** A slot minted by an SE-co-signed flow over an existing statechain — an
off-chain split piece/change, a `transfer_many` recipient/change, a combine output, a refresh
re-anchor — is a **derived slot**: it re-houses value already inside the SE, so the SDK MUST fund
it with a FREE derived token (`deposit/get_derived_token`, vouched by the parent statechain) and
MUST NOT draw on pooled/prepaid onboarding tokens (in a token-server deployment those cost the
onboarding fee — a 2-output split must not cost 2× it). The SE MUST gate issuance on (i) the
parent's CURRENT-owner auth (single-use nonce, consumed only on a valid signature), (ii) a
per-parent LIFETIME cap (`max_derived_tokens_per_statechain`, default 64; 0 disables), and (iii)
the global outstanding-token cap (audit [26]), and MUST mark issued tokens with their parent
(`tokens.derived_from`). Fresh ON-CHAIN onboarding (a deposit address, a token-issuance carrier)
still consumes a normal token per REQ-14. Fallback: when the SE predates/disables the endpoint or
the allowance is exhausted, the SDK falls back to onboarding tokens (pre-REQ-35 behaviour).

> **CORRECTION — the exhausted case does NOT fall back, it ERRORS.** `get_derived_tokens` returns the
> fallback signal (`Ok(None)`) for exactly two statuses: `NOT_FOUND` (route absent) and `FORBIDDEN`
> (issuance disabled, `cap == 0`). The per-parent lifetime cap answers `TOO_MANY_REQUESTS`, which
> becomes an `Err`. So a wallet that exhausts its allowance fails the deposit rather than silently
> paying for an onboarding token — which is the better behaviour, and the sentence above described
> the opposite.
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
(there is one protocol): `0` = branch/backup message (un-laddered), `2` = a conveyed TES-R ladder,
`4` = a split-child bundle carrying the key handover (`3` is the superseded no-handover child
conveyance). The receiver dispatches its validation on this tag.

**REQ-15** `transfer(address, amount)` MUST move exactly `amount`: either an exact subset of coins
(§5.1) or an off-chain split minting the exact piece (§6). No dust or overpayment.
**REQ-16** The receiver MUST validate: transfer signature binds the coin to its key; tx0/branch is
valid; latest backup pays the receiver; backup locktimes decrement correctly (INV-5, enforced on
BOTH shapes); and the co-signature count reconciles — `num_sigs == backups` on the un-laddered
shape, the census (REQ-38) on the laddered one.
**REQ-17 (G1)** For a branch-carrying (sub-coin) transfer, the receiver MUST verify every
`terminal_parents` ancestor is terminal at the SE (`GET spend_budget`, `terminal==true`) and
reject otherwise (ERR-7). That endpoint is the COORDINATOR's own answer, computed from its Postgres;
this un-laddered lane still rests on it (`verify_terminal_parents`). On the laddered/child lane it
has been DEMOTED: terminality is derived from the enclave-signed `(num_sigs, sig_budget)` payload
(`budget exists ∧ num_sigs ≥ budget`, `attested_terminal`), and the coordinator's answer is kept
only as a cross-check that REFUSES on disagreement — the two stores hold the same absolute quantity,
so a mismatch means one was written behind the other's back.
**REQ-38 (census)** A receiver of a laddered coin, or of a split child (§6.3), MUST reject unless
the SE's ATTESTED co-signature count (§3.3 — an unattested count MUST be refused, since the census
rests entirely on it) equals EXACTLY the tiers it was shown:
`se_num_sigs == flat_backups + Σ conveyed tiers + Σ disclosed superseded tiers`, summed over every hop
of the conveyed ancestor chain (N-hop for a re-transferred child). Each disclosed superseded tier
MUST be parsed, linked to the ladder, signature-checked, and carry a strictly HIGHER CSV than the
tier that replaces it — a `.len()`-only count is paddable, and an unparsed `csv: None` skipped the
race check. Any hidden co-signed state shows up as a count mismatch and MUST reject (ERR-15). The
count is retry-safe: a repeated `sign/second` returns the cached partial signature and does NOT
advance `sig_count` (`sdk56`), so an in-flight retry cannot brick the equation. Verified by `sdk46`
(count formula against the real SE), `sdk54`/`sdk55` (padding/spoof attacks REJECT), `sdk58`
(11 child-bundle attacks REJECT).
**INV-8** Claiming is idempotent: repeated `claim()` passes book each transfer at most once.

### 5.1 Coin selection
`select::plan(coins, target)` returns `Exact(subset)` if a subset sums to `target`, else
`WithSplit{whole, split, split_amount}`, else `Insufficient{available}`.
**INV-9** `Exact(s)` ⟹ `Σ coins[s] = target`. `WithSplit` ⟹ `Σ whole < target ∧ split_amount =
target − Σ whole ∧ coins[split] > split_amount`. `Insufficient` ⟸ `Σ coins < target` — but the
reverse does **not** hold: since audit [29] the planner also returns `Insufficient` when the
remainder can only be minted as an unviable piece (no split candidate covers
`remainder + fee_reserve + min_split_output`, where `min_split_output = 330` (dust) `+` the sub-coin's
own backup fee at the live rate = `330 + ceil(112 · fee_rate)`; the planner also requires
`remainder ≥ min_split_output` so the minted piece can fund its own backup —
`select::plan_with_floor`, `transfer::min_split_output`).
See [GRANULARITY-SPEC.md](GRANULARITY-SPEC.md) GRN-REQ-5 (whose fee arithmetic describes the
un-laddered/colored split; the in-ladder split has its own model, §6.1).

**Executor floor (laddered parent).** `min_split_output` is a TERM in the planner's floor, not the floor itself. `plan_payment` resolves a
`ParentShape` per candidate and hands `select::plan_with_floor` the MINIMUM over those shapes of
`SplitFloors::planning()` (itself `piece.min(change)`), so for a laddered root the number the planner
actually uses is `max(min_split_output, min_spine_tip_value)` = **945** at the shipped rate — never
the bare dust default. When the chosen
parent is laddered, the in-ladder executor applies a second, strictly larger floor and the larger of
the two binds: a child funds its OWN extension + state tier before it can clear dust, so
`min_child_value = 2·(committed_fee(rate) + 240) + 330` — **1 560 sat** at the shipped
`committed_fee_rate = 3.0` ([D44]: `2·(375 + 240) + 330`), versus the old 442. It was 1 310 while the
committed rate was 2.0, and 1 306 before [D4] measured the tier at 125 vB rather than 124.
**[D56] The floor is a function of the RATE — quoting one of these numbers without its rate is
quoting a rate**, which is how the code's own derivation comment went stale in silence: every floor
test froze `rate = 2.0` as a fixture, so raising the shipped rate broke none of them. The two legs are floored INDEPENDENTLY and by LANE — `SplitFloors { piece, change }`, not one
number for both. The piece always funds two rungs, so its floor is
`max(min_split_output, min_child_value)` = **1 560** at the shipped 3.0 rate. The change's floor is
whatever `change_leg_role(lane)` says that lane's builder gives it: `SpineTip` on the plain-root,
spine-batch AND coloured lanes — one rung, `min_spine_tip_value` = **945** — and `Piece` only on the
plain-CHILD lane. Either leg falling short refuses the split UP-FRONT (ERR-16), naming that leg's own
floor. Up-front is
load-bearing: `establish_child` runs AFTER the parent's spend budget is consumed and `SP` is
co-signed, so admitting a child below the floor terminalized the parent and THEN failed, stranding
it to unilateral-exit-only. (Defect found and fixed during this migration; it also broke
`refresh_sponsored`, §9.4.)

---

### 5.2 The transfer mailbox

A conveyance travels as an ECIES ciphertext in a coordinator-held mailbox, addressed to the
receiver's auth key. The coordinator is therefore in the path of every payment, and the specification
must say exactly what that buys it. Surveyed adversary by adversary in
[`spec-work/MAILBOX-SURVEY.md`](spec-work/MAILBOX-SURVEY.md); the results:

| | class | coordinator acting ALONE can | verdict |
|---|---|---|---|
| **M-1** | withholding | delay a conveyance indefinitely | **denial only** — the read is non-destructive, so a later poll still gets the message. One escalation is worth naming: withholding past the coin's epoch expiry converts a delay into a PERMANENT failure, because admission requires the exit walk to fit inside the epoch the payee inherits |
| **M-2** | deletion | destroy the message | **denial only.** The coin stays the sender's. The LOSS arm — "the sender then re-conveys to a second payee" — is NOT coordinator-alone: the cancel that frees the coin needs a single-use, endpoint-bound signature under the SENDER's auth key. Coordinator + sender, the same adversary as L-7 |
| **M-3** | reordering | serve messages in any order | **no ordering dependence.** A message is bound to a coin by KEYS, not by mailbox position: the ciphertext is ECIES to the auth key and the transfer signature commits to `(tx0_txid, tx0_vout, new_user_pubkey)`. A misrouted message fails validation and the loop moves on, costing a wasted pass |
| **M-4** | duplication / replay | serve one ciphertext twice | **refused — and since [D71] refused BY NAME.** See below |
| **M-5** | cross-addressed injection | serve a message addressed to someone else | **fail-closed** — ECIES decryption fails under this coin's auth key. Nothing further is relied on |
| **M-6** | serve-then-renege around an irreversible leg | serve a valid message to a pre-pay census and then withhold at claim time | **real loss, Lightning lane only.** See §8 |

**REQ-45 [D71] A receiver MUST refuse a conveyance of a `statechain_id` it has already adopted, by
name.** "Already adopted" means a coin row this wallet still holds and can spend
(`IN_MEMPOOL`/`UNCONFIRMED`/`CONFIRMED`/`WITHDRAWING`). It deliberately does NOT include
`TRANSFERRED` — sending a coin away and receiving it back later is legitimate — nor `IN_TRANSFER`,
which is the sender's own row during a SELF-transfer, where the receiving slot in the same wallet
must still be able to adopt.

The reason this is a requirement and not an observation: a duplicate takes the same path as an honest
re-serve, and every check that binds the message to the coin passes. What refused it before [D71] was
`validate_tx0_output_pubkey` failing because the completed handover had rotated the SE's share — a
CONSEQUENCE of an unrelated subsystem, not a rule. The child lane had refused by name since [D3]; the
root lane is the one that never did. **A protection that holds only because something else rotates a
key is not a protection a specification can state.**

**REQ-46 [D71] A wallet's balance MUST be a function of DISTINCT statechain ids, not of rows.** A
second live row under one id is one coin counted twice. This does not make a replay safe — REQ-45
does that — it removes the SILENT failure mode: without it, a lapse upstream shows up as spendable
value that is not there, and a merchant crediting on the balance over-credits with nothing in the
log. With it, the same lapse shows up as a coin that fails to arrive.

## 6. Off-chain split & combine

### 6.1 In-ladder split (laddered coins)
A non-exact payment out of a laddered coin is an **in-ladder split**. `transfer()` routes on
`ParentShape`, which has FOUR arms — `Root → in_ladder_pay`, `Child → child_in_ladder_pay`,
`SpineTip → spine_batch_pay`, `Unladdered → split_coin` — and `parent_shape` probes the spine tip
FIRST. That ordering has a consequence worth stating: `in_ladder_pay` gives its change leg
`ChangeLeg::LastIsTip`, so after the first partial payment the sender's change IS a spine tip, and the
SECOND and every later payment out of it take the `SpineTip` arm rather than this one. `SP` is a SPINE state tier spending `X_m.out[0]` at `SPINE_CSV = 0` — a
DESCENDANT of the trigger, never a rival for `F`, and strictly below the `S_0` it replaces on that
output — carrying one resting output per child plus the P2A anchor; each child then hosts its own
extension + state tiers (`establish_child`). The parent is terminalized
before the co-sign and its superseded state disclosed for the receiver's census (REQ-38).

**REQ-39 (in-ladder split)** A laddered coin MUST NOT be split as plain BTC: a prior owner's
retained no-timelock trigger could spend `F` and void a split of it while the ladder still paid the
splitter the whole coin [B1]. The split MUST descend from the trigger, value MUST be conserved
exactly — `Σ children == tier_out_total(X_m.out[0], n) = X_m.out[0] − committed_fee_for_outputs(n)
− 240`, where `committed_fee_for_outputs` adds 43 vB per extra child so the tier still relays
standalone — and every child MUST clear the §5.1 executor floor before the parent's budget is
consumed (ERR-16). Verified by `sdk58` (accept + 11 adversarial cases REJECT: aggregates,
hidden-state, Model-A payee, parent terminality, child-superseded race, count-padding, value-spoof),
`sdk59` (end-to-end split payment), `sdk04` (the terminalized parent refuses a second spend at both
the wallet and the SE).

**REQ-47 (split depth) [D53] The build side MUST NOT mint a child the receive side would refuse.**
A conveyed child is admitted by `check_exit_headroom_with_margin` — the exit walk must fit inside the
epoch the payee inherits WITH `exit_slack_margin = max(required/4, required/tiers)` of head-room, not
merely by the bare latency rule `exit_wait_blocks <= epoch`. Both sides MUST evaluate the SAME rule.

The caps that follow are **derived, not chosen**: depth **8** on mainnet (19 transactions to walk),
depth **54** on regtest (111). They are stated here because the failure they prevent is silent and
expensive: while the builder used the bare rule and the payee used the margin rule, depths 9 and 10
were BUILT and were unadoptable at every tip — and since a parent is terminalized before its child is
conveyed, each such child was a stranded piece with a terminalized parent behind it. Held together by
`the_build_side_never_admits_what_the_receive_side_refuses`; nine tests pinned the old numbers and
were green.

**REQ-48 (the payee's clock) [D55]** The window a split is measured against MUST be derived from the
PARENT's own conveyed backup chain, never from a freshly-read epoch. The builder measuring a fresh
epoch while the payee measures `epoch_expiry − tip` is [D36]'s T-4, and it admits splits the payee
cannot use. The parent's flat backups travel with the bundle for exactly this reason — **a local
lookup and a conveyed fact are not interchangeable**: a wallet holding a conveyed CHILD has never
held the root, so looking the backups up by `(wallet, parent_sid)` finds nothing (`sdk17` catches
this immediately).

### 6.2 Branch split & combine (un-laddered coins, colored splits)
This is the shape RGB rides (§7) and the only one left for a coin that cannot be laddered.
`split_coin(id, piece)` builds one SE-co-signed, un-broadcast tx spending the coin into
`{piece sub-coin, change sub-coin}` (minus a fee reserve), records both as sub-coins with their own
backup ladders, the shared exit branch, and ancestor records; sets the parent's spend budget to 1.

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

Assets are RGB contracts (NIA fixed-supply, IFA inflatable). Allocations ride coins/sub-coins on the
UN-LADDERED shape (§6.2) — that path exists for tokens and is load-bearing, not legacy.

**INV-29 (terminal freeze)** An RGB **carrier** is never laddered WITH A PLAIN LADDER, and on the
shipped configuration is not laddered at all: a plain T/X/S tier spend is
sats-only and would destroy the allocation, so carriers are excluded from PLAIN ladder
establishment (REQ-37) — **the exclusion is conditional, not structural**: `claim()`'s decision site
is `match (config.colored_ladder, allocation)`, and with `colored_ladder = true` a single-allocation
carrier reaches `build_colored_ladder_auto` + `cosign_colored_ladder` and IS laddered, coloured.
Both `SdkConfig` constructors ship `false` ([D30]), so the shipped behaviour is as stated; the word
to avoid is "structurally". Carriers are also excluded from plain re-anchor (REQ-32), from plain
withdraw/unilateral exit, and from, from plain re-anchor (REQ-32), from plain withdraw/unilateral exit, and from
watch bundles that carry a sats-sweeping backup (REQ-34). Correspondingly, a colored tx only ever
spends outputs of TERMINALIZED structure (terminalization precedes the colored co-sign, and the SE
refuses renewal on a terminal node), so no ancestor of an RGB anchor is ever re-signed and **no
superseded colored witness exists anywhere in the system** — consignments carry un-broadcast witness
txs, which is the model rgb-lib already supports. (PROTOCOL.md §5.10.) Verified by `sdk52` (in one
wallet the plain coin carries a ladder, the carrier carries none, and an off-chain RGB transfer
still settles) and `sdk32`.

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

**Transfer.** `transfer_tokens`/`batch_transfer_tokens`: a colored off-chain split carves the
recipient piece(s) + change; the consignment rides `BackupTx.rgb_consignment` as a
`ConsignmentEnvelope{c, a, s}`. When no single carrier holds the requested amount, the transfer
automatically COMBINEs several carriers of the same asset (`colored_combine_transfer`) into one
SE-co-signed colored combine tx (N input carriers → recipient piece + change), conserving the
asset's allocation across all combined inputs.
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
> trust model makes anywhere else, and it is the one the mailbox survey adds.
>
> The lane runs census → pay → claim. The census reads a conveyed message; the payment is an
> IRREVERSIBLE Lightning leg; the claim comes after. A coordinator that serves a valid message to the
> census and then withholds, alters, or refuses at claim time leaves the payer out the full invoice
> amount — **acting alone**, with no sender and no key. Everywhere else in this document a
> coordinator acting alone can only deny; here it can take.
>
> The shape is not specific to the mailbox — refusing `/transfer/receiver` does the same — so it is
> stated as a property of the lane rather than of the transport. The failure text already exists and
> has fired on the live stack ("paid the Lightning invoice … but claimed 0 transfers"), which is what
> makes the window observed rather than theoretical.

Each direction has an EXACT lane (the wallet already holds, or can
mint, a coin of the exact size — the whole coin is latch-transferred) and a NON-EXACT lane (the coin
is split IN-LADDER and the latched PIECE is conveyed, §6.1).

### 8.1 Pay (Mercury → Lightning)
`pay_lightning_invoice(ssp, invoice)`: obtain the exact coin, `create_external_hash_latch` bound to
the invoice's payment hash, hand the coin to the SSP; the SSP pays the BOLT11; the LN preimage
`unlock_by_preimage`s the coin and is returned to the payer as proof.

**REQ-23** The SSP MUST verify the latch hash equals the invoice payment hash before paying, and
MUST run its pre-payment value gate — `verify_bundle` / `verify_conveyed_child` over the conveyed
ladder — BEFORE `send_payment`, pricing against the value the ladder cryptographically commits to
(`sdk37`, `sdk63`).
**REQ-42 (one-call pay routes both lanes)** `pay_lightning_invoice` MUST NOT depend on minting an
exact coin: when no exact coin can be minted it MUST fall back to the non-exact in-ladder lane
(`pay_lightning_invoice_inladder`), the same way the receive side does. Without that fallback the
one-call API refused every laddered coin — i.e. every coin — and was unusable (defect found and
fixed during this migration). `sdk63` (exact), `sdk65` (non-exact).
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
- **Un-laddered** — broadcast the exit branch (instant, locktime-free) then the coin's latest
  pre-signed backup, subject to its absolute locktime.

**REQ-24** `unilateral_exit` MUST require no SE interaction.
**REQ-25** A tier or backup whose timelock is unreached MUST be reported as
`ExitStatus{complete:false, wait_blocks>0}`, not an error; callable again after the wait.
**REQ-44** `unilateral_exit` MUST refuse a coin that is not `CONFIRMED` even when named explicitly —
exiting a parent already consumed by a split would kill the tx funding the receiver's child [B1] —
and MUST refuse a token carrier (an RGB-unaware spend destroys the allocation, INV-29).
**INV-16** After the chain confirms, funds are at the owner's address; RGB allocations settle
on-chain.

### 9.3 Cost
`estimate_exit_cost(coin)` → `{branch_txs, branch_vbytes, backup_vbytes, total_vbytes, wait_blocks,
exit_deadline_block}`.
**INV-17** `total_vbytes = branch_vbytes + backup_vbytes` (measured from the actual pre-signed txs);
`fee_sats_at(rate) = ceil(total_vbytes · rate)`; `wait_blocks = max(0, backup_locktime − tip)`.
**Scope (stated honestly).** `estimate_exit_cost` measures the UN-LADDERED material only — the
stored branch plus the latest absolute-locktime backup — and it is also what feeds the calendar
deadline used by REQ-33. It does NOT yet account for a laddered coin's tier chain, whose cost and
wait are structural instead: 3 pre-signed tiers = 375 vB (3 × `TIER_VBYTES` 125, plus up to 3 P2A
fee children in a spike) and a sequential `E_m + Δ_k` CSV wait; each split level adds 2 tiers
(293 vB — an `SP` with two payload outputs, plus an extension) and ONE extension CSV, because the
`SP` itself is a spine tier at CSV 0 and waits only for its parent to confirm
(`config::tesr_exit_vbytes` / `tesr_exit_wait_blocks`, PROTOCOL.md §5.9).
`exit_deadline_block` is `None` for a laddered coin: it reports the height at which an ANCESTOR
could race an off-chain sub-coin, and a laddered root has no such ancestor. It is **not** a claim
that the coin has no calendar at all — the retained flat chain's locktime is a real deadline
(INV-27, `sdk86`) and no client surfaces it [D36 T-4].

### 9.4 Refresh (cooperative on-chain re-anchor)
`refresh(id, fee_rate?)` / `refresh_sponsored(id, sponsor, fee_rate?)`: one SE-co-signed single-input
spend of the coin's current 2-of-2 outpoint into a FRESH deposit aggregate (a new `statechain_id`,
same owner; a sub-coin's exit branch is materialized first).

Refresh is **no longer a deadline reset for the EXIT** — a laddered coin's exit is the CSV tier
chain, which never matures while idle (INV-27). It does still reset the coin's flat calendar, by
minting a fresh chain at `tip + initlock`, which is what makes it the answer for a coin that has
spent most of its hop budget [D36]. It is the **re-anchor primitive**: the escape hatch that moves a
coin out of its current
ladder/branch and permanently kills every exit right rooted at the old outpoint. For an un-laddered
coin it is still the way to escape the backup-ladder floor without going to L1 (§2.4).

**REQ-31** `refresh` MUST spend the current outpoint into a fresh aggregate, which then gets a fresh
full ladder of its own (REQ-37); because the old outpoint is now spent, EVERY exit right rooted at
it — every previous owner's backup and every old tier — is permanently invalidated. It is
COOPERATIVE (it needs the SE); if the SE is gone the owner exits unilaterally (§9.2) instead. The
fee is drawn from the coin (single-input, blind SE), so the user-pays variant yields `amount − fee`.
`refresh_sponsored` reimburses that fee OFF-CHAIN from a funded sponsor; because the rebate is a
non-exact payment out of the sponsor's own (laddered) coin it is minted by an in-ladder split, so
the rebate MUST be sized to `max(fee + dust, min_child_value)` — **1 560 sat** at the shipped 3.0
([D44]; 1 310 at the superseded 2.0), not the old 442. Sizing it into that dead window made every sponsored refresh fail AFTER the user had already
paid the on-chain fee (defect found and fixed during this migration). The operator absorbs the
difference; the user ends ≥ whole. `sdk30` (a)/(c), `sdk38` (a broke sponsor loses boundedly).

**REQ-32 (auto-refresh)** When `SdkConfig::auto_refresh` is set (default), the SDK MUST re-anchor a
coin nearing its BACKUP-ladder floor before it is spent, transparently: `auto_refresh_due(margin)`
re-anchors every confirmed, non-carrier coin whose headroom (`locktime − tip`) is ≤
`auto_refresh_margin_blocks`, and `transfer`/`transfer_many` MUST run it (and await the fresh coins'
confirmation) before selecting coins. Token CARRIERS are excluded (a plain re-anchor would destroy
the allocation, INV-29). Routine BACKGROUND refreshing was specified as default-OFF via `background_auto_refresh = false`.

> **CORRECTION — that flag has NO production reader.** It appears only in `SdkConfig`'s two
> constructors, in doc comments, and in tests; nothing on a runtime path reads it. Background
> re-anchoring therefore runs REGARDLESS of it, because `start_background` → `maintenance_plan`
> schedules `DeadlineSafety` unconditionally and `deadline_safety_due`'s first route is
> `auto_refresh_due`. The design intent behind the flag — an idle wallet never silently shrinks —
> stands as an intent; what implements it today is the margin (`auto_refresh_margin_blocks`), not the
> flag. Either wire the flag or delete it; a config field that nothing reads is a claim the code does
> not make.

> **Coverage note.** This requirement's dedicated E2E (`sdk33`) was retired with the one-protocol
> migration and has NO live replacement — the pass itself is not exercised end-to-end today. The
> underlying re-anchor is covered by `sdk30`, and the property REQ-32 was created to guarantee (a
> coin never becoming un-spendable by aging) is now STRUCTURAL for laddered coins rather than
> maintained by this pass: idle coins never age (INV-27) and renewal is off-chain and unbounded
> (`sdk43`). REQ-32 remains normative for the un-laddered shape, whose backup ladder still ages.

### 9.5 Watchtower (automatic deadline protection)
Two passes — but NOT one per shape, and the calendar pass is not un-laddered-only.

> **CORRECTION.** `start_background` no longer branches on any flag: it iterates `maintenance_plan`,
> which returns `[MaintenancePass::DeadlineSafety]` unconditionally (its `SdkConfig` parameter is
> literally unread), and runs `deadline_safety_due` every tick. That pass is CALENDAR-driven over
> WHOLE LADDERED COINS as well as un-laddered ones — a laddered coin retains its absolute flat-backup
> calendar (§0.3, G12, §14.3), so it is very much the subject of a deadline pass. The event-driven
> `defend_ladders()` alarm is the SECOND pass, not the laddered shape's only one.

**Calendar pass (un-laddered).** `auto_exit_due(margin)` protects any owned coin with a branch that
is within `margin` blocks of its deposit-anchored exit-race deadline (§9.3), before an ancestor can
broadcast a stale backup. The background watcher MUST run it each poll when `SdkConfig::auto_exit`
is set (default), with `auto_exit_margin_blocks` — **derived, never chosen**:
`k_max·interval + tesr_exit_txs(d)·144`, i.e. **860** blocks on regtest (`14·10 + 5·144`) and
**2 120** on mainnet (`14·100 + 5·144`), because the exit walk lands `3 + 2d` transactions ONE AFTER
ANOTHER and each must confirm before the next tier's relative lock starts counting
(`config::auto_exit_margin_blocks_for`). The superseded literal was 288 (≈ 2 days): one
confirmation window for a whole walk, against a `k·interval` gap that on mainnet alone is 1 400
blocks. The walk's own `Σ csv` is deliberately NOT folded in here — `auto_exit_due` takes that head
start per coin, off the coin's own chain. A laddered coin has no such deadline and is not its
subject.

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

> Invalidation now has TWO mechanisms, one per coin shape.
> - **Laddered**: relative-CSV replacement — a lower-CSV tier out-races and orphans the one it
>   supersedes (INV-28), disclosed to the receiver and checked by the census (REQ-38). The
>   normative treatment is [PROTOCOL.md](PROTOCOL.md) §5.5/§5.7/§5.11. There is no ladder
>   formula and no re-anchor rent on this shape (INV-27), so the deposit-anchored deadline
>   arithmetic does not govern its EXIT. The retained flat chain still carries an absolute
>   calendar, and the audit-[17] `k·interval` gap still applies to that chain — `sdk86` measures
>   the per-hop cost directly.
> - **Un-laddered**: the absolute-locktime decrementing ladder — the mechanism specified in
>   [INVALIDATION-SPEC.md](INVALIDATION-SPEC.md) (IVL-REQ/IVL-INV/IVL-ERR numbering), which
>   remains authoritative FOR THAT SHAPE where it overlaps with the summary below.

**INV-18 (no old state)** Split/combine spend into NEW outpoints; a child cannot confirm before its
parent (its input is the parent's output), so there is no old-vs-new race within a tree. On the
laddered shape the split state `SP` additionally DESCENDS from the trigger rather than racing it,
which is what closes [B1] — a prior owner's retained no-timelock trigger can only start the clock on
the current owner's own chain, never void the split. Verified by `sdk58`/`sdk59`.
**INV-19 (fork prevention)** The SE refuses a second spend of any node (single-use / spend budget),
so a node cannot be forked into two conflicting children. Verified by `sdk04` (a terminalized
in-ladder parent is refused a second split at the SE, and the refusal is pinned to terminality
rather than to an incidental plumbing error), `rgb04` (single-use).
**INV-20 (terminal ancestors)** A sub-coin's receiver only accepts it if every structural ancestor
is terminal at the SE (REQ-17) — a malicious sender cannot double-spend a parent afterwards. The
receiver derives the required ancestor count from the branch itself: it requires at least one named
terminal ancestor **per structural INPUT the branch consumes** (`n_parents ≥ Σ inputs`, `≥ 1` —
`required_terminal_ancestors` accumulates `tx.input.len()` over the branch). The per-HOP count this
replaced (`branch_len`) is named in the code as the hole it closes: it required only ONE terminal
ancestor for an N-input COMBINE. So a sender cannot hide a
non-terminal, double-spendable ancestor by shipping an empty or short `terminal_parents` list
(ERR-7). Verified by `unit::terminal_parents_tests` (the count binding, including the
empty/short-list cases) and, on the laddered shape, by `sdk58`'s parent-terminality attack (a child
whose parent is not terminal is REJECTED). Honest-accept paths: `sdk29`/`sdk31`/`sdk39` (token
transfers over real branches). *Coverage gap:* the branch-lane E2E that rejected a non-terminal
ancestor end-to-end (`sdk10`) was retired with the migration; the guard is unchanged in code
(`verify_terminal_parents`) but only the unit test and the laddered-lane equivalent now exercise it.
**INV-21 (bounded lifetime)** With `epoch_deadline` set, the SE stops co-signing new state past the
deadline; unilateral exit still works forever (needs no SE), so funds are never swept.
**INV-22 (UTXO granularity)** Exact amounts are native (1-sat resolution) via off-chain split —
strictly finer than fixed-denomination leaves. The resolution is unchanged by TES-R; only the
minimum viable PIECE moved, from the backup-fee floor to `min_child_value` on the laddered shape
(§5.1), because a child now funds two exit tiers instead of one backup.
**INV-23 (nonce single-use)** The SE binds each server nonce to exactly ONE challenge: `sign/second`
sets the challenge atomically only if it was NULL (or identical — idempotent retry) and otherwise
refuses (ERR-12). A second finalize over one nonce with a different message is therefore impossible,
which is what makes the blind-MuSig2 scheme safe against an owner who controls the raw signing
requests — without it, two partial signatures over one secnonce would leak the SE's key share and
yield two co-signed conflicting spends while `count_finalized_signatures` (and hence single-use /
budget / epoch enforcement) counted only one. Verified by `sdk12` Part C.
**INV-24 (budget monotonic)** `set_spend_budget` may only TIGHTEN a coin's `sig_budget`
(`new = min(existing, count+remaining)`); it can never raise it, so an already-terminal node cannot
be re-opened for a second conflicting spend. This is why a first-class split child is handed over
with a KEY ROTATION and a releasable pending lock (REQ-36/REQ-40) instead of a budget re-open: any
re-open would resurrect exactly the fork class this clamp prevents. Verified by `sdk04` (a terminal
node stays terminal and refuses a second spend) and `unit::invalidation_model::terminal_predicate_matrix`.
**INV-25 (branch value conservation)** The receiver's `validate_branch` rejects any exit branch whose
txs create value (`Σ outputs > Σ inputs` at any hop): `tx.verify` checks scripts but not the fee
rule, so without this a sender could hand over a coin whose branch is script-valid yet un-broadcastable
(the receiver could never exit on-chain while the sender keeps the funds). The guard is unchanged in
code (`transfer_receiver::validate_branch`) and honest branches are still accepted by every token
E2E (`sdk29`/`sdk31`/`sdk39`). *Coverage gap:* the E2E that fed a value-INFLATING branch and asserted
the rejection (`sdk10`) was retired with the migration and has no live replacement — the reject side
of this invariant is currently unexercised. The laddered shape's analogue (a value-spoofed tier) IS
covered, by `sdk54`/`sdk58`.
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
  `change_leg_role()` says THAT LANE's builder gives it. At HEAD that is ONE rung
  (`min_spine_tip_value` = **945** sat plain / `colored_spine_tip_floor` = **1 074** coloured, at the
  shipped 3.0 — 820 / 906 at the superseded 2.0; the coloured tier is 168 vB against the plain 125) on
  the plain-root, spine-batch AND coloured lanes — CATS change 2 has landed on all three — and two
  rungs only on the plain-CHILD lane, where the change is still carved as a `Piece`.

---

## 12. Traceability

Each requirement/invariant is verified by at least one test. Pure-logic items have unit tests;
protocol items have E2E tests (regtest). See [testing-guide](build/testing-guide.md) for how to run.

| Item | Test |
|---|---|
| REQ-1, REQ-3 | design (2-of-2 keys); exercised by every co-sign flow |
| REQ-2, REQ-24, REQ-25, REQ-44, INV-16 | `sdk50` (SDK unilateral exit walks T→X→S to the owner's key), `sdk40` PART 1 (consensus: each tier rejected before its CSV, accepted after), `sdk58` (child chain exits to the receiver) |
| INV-17 (un-laddered exit cost) | `unit::types::tests::exit_cost_math`, `unit::invalidation_model::exit_cost_scaling_model`, `sdk39` (depth-2 token exit) |
| REQ-4, REQ-14, ERR-6, INV-7 | `sdk01` deposit; `unit::types::tests::error_semantics` |
| REQ-5, ERR-1 | `rgb04` (single-use refusal) |
| REQ-6, ERR-2, INV-21 | `rgb07` (epoch deadline) |
| REQ-7, REQ-13, REQ-18, ERR-3, INV-19 | `sdk04` (terminalized in-ladder parent refuses a second spend, at the wallet and at the SE), `unit::types::terminal_predicate`, `unit::invalidation_model::terminal_predicate_matrix` |
| REQ-8, REQ-9, REQ-15, REQ-16, INV-5, INV-8 | `sdk01`, `sdk04`, `sdk41` (receiver gains control, sender locked out), `sdk55` (backup chain cannot be padded or inverted), upstream `tb01/tb05/tm01/ta02/ta03` |
| REQ-36, ERR-14 (pending-transfer lock) | `sdk49`/`sdk41`/`sdk01` + `sdk58`/`sdk59` (green with the lock live — i.e. the sender pre-sign re-ordering is correct and no honest flow is blocked); `sdk60` (a child conveyed under the lock is claimed and re-transferred). **The gap this row used to claim is CLOSED:** `tb05` drives the refusal itself — it conveys a coin, leaves the transfer open and unclaimed, then calls `transfer_sender::execute` again for the same statechain id to a DIFFERENT recipient, asserting the second call errors with "coin has an open transfer" |
| REQ-37 (ladder establishment) | `sdk48` (auto-established, seed-derived payee, idempotent), `sdk52` (carrier excluded) |
| REQ-45 (replay refused by name) | **The evidence is weaker than this row first claimed, and the correction is the point.** `rgb10` PART 2 is an RGB-LAYER self-split — one wallet mints two of its own witness seals and splits a root coin between them — NOT a statechain self-transfer, so it never puts a sender row at `IN_TRANSFER` under the id a receiving slot adopts and is NOT the negative control for this predicate. What exists today: the refusal's own predicate (which excludes `IN_TRANSFER` and `TRANSFERRED` by construction) and the live suite passing with the guard in place. **Two real gaps:** no test drives a deliberately DUPLICATING coordinator, and no test exercises a statechain SELF-TRANSFER against the guard — which is the case a careless predicate breaks ([D71]) |
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
| REQ-17, INV-20, ERR-7 | `unit::terminal_parents_tests` (count binding); `sdk58` (parent-terminality attack REJECT); `sdk29`/`sdk31`/`sdk39` (honest branches accepted). **Gap:** the branch-lane non-terminal-ancestor REJECT E2E (`sdk10`) was retired with no replacement |
| REQ-18, INV-10, INV-11 | `sdk29`/`sdk31` (colored splits/combines); `unit::split_math` |
| REQ-39, ERR-16 (in-ladder split) | `sdk58` (accept + **12** REJECTs), `sdk59` (end-to-end split payment), `sdk12` Part B (value flow), `sdk30` (c) (the `min_child_value` floor in a sponsored rebate) |
| REQ-40, REQ-41 (first-class children) | `sdk60` (alice→bob→carol off-chain, `F` unspent throughout), `sdk17` (multi-hop, partial second hop), `sdk04` (a spent parent is refused) |
| INV-18, INV-19 | `sdk58`/`sdk59` (SP descends from the trigger — [B1]), `rgb03`/`rgb06` (off-chain DAG), `rgb04` |
| REQ-19, REQ-20, INV-12, INV-13 | `sdk09` (IFA issue + mint + batch) |
| REQ-21/INV-13 (multi-carrier combine) | `sdk31` (token combine) |
| REQ-21, REQ-22, ERR-8 | `sdk02`, `sdk09`; `unit::envelope` |
| INV-29 (terminal freeze / carrier ⊥ ladder) | `sdk52` (plain coin laddered, carrier not, RGB transfer still settles), `sdk32` (tokens over time), `sdk39` |
| ERR-9 | `sdk04` (`unit::select` insufficient) |
| ERR-10 | `sdk04` (double-withdraw / split-parent refusal) |
| INV-22 | `sdk01`/`sdk09` (exact-amount splits) |
| REQ-26 | `sdk11`; `unit::identity_tests::sign_validate_roundtrip` |
| REQ-27 | `sdk11` (multi-recipient) — see the divergence note in §13 |
| REQ-28, ERR-11 | `sdk11`; `unit::invoice::tests` (roundtrip, reject) |
| REQ-29, REQ-30 | `sdk11` (query API + fee quote) |
| REQ-31 (refresh / re-anchor) | `sdk30` (a) (idle coin unchanged, then re-anchored) / (c) (sponsored rebate sized to `min_child_value`), `sdk38` (broke sponsor, bounded loss) |
| REQ-32 (auto-refresh in transfer) | **no live test** — `sdk33` was retired with no replacement; see the coverage note in §9.4. Re-anchor itself: `sdk30`; unbounded off-chain renewal: `sdk43` |
| REQ-33 (watchtower carrier materialize) | `sdk34` (received-carrier auto-materialize, clawback defeated) |
| REQ-34 (keyless watch delegation) | `sdk45` (keyless bundle carries zero key material, a 2nd independent tower is idempotent, an offline owner is defended against a hostile trigger), `sdk51` (in-wallet pass), `sdk52` (carriers excluded); `unit::watchtower::tests` |
| REQ-35, ERR-13 (derived slots) | `sdk36` (poisoned-pool split/refresh, onboarding still charges, direct mint, caps, garbage/replayed/non-owner auth); `mercurylib unit::deposit::derived_token_tests` |
| INV-20 (ancestor-count binding), ERR-7 | `unit::terminal_parents_tests`, `sdk58` (laddered-lane equivalent) |
| INV-23, ERR-12 | `sdk12` Part C (nonce-reuse refused) |
| INV-24 | `sdk04` (terminal node stays terminal), `unit::invalidation_model::terminal_predicate_matrix` |
| INV-25 | honest branches accepted: `sdk29`/`sdk31`/`sdk39`. **Gap:** the value-inflating-branch REJECT (`sdk10`) was retired with no replacement; the laddered analogue is `sdk54`/`sdk58` |
| INV-26 | `sdk09` (IFA received amount = fungible only) |
| Concurrency / chaos | `chaos22` (N users act in parallel) |

## 13. Query, utility & invoice API

Client-side conveniences (no new SE state); mirror Spark's query/signing/invoice surface.

**REQ-26** `sign_message_with_identity_key(msg)` MUST produce a BIP340 Schnorr signature over
`sha256(msg)` under a STABLE identity key (derived at `m/1000h/0h/0h`, unchanged as coins come and
go); `validate_message_with_identity_key(msg, sig, pubkey)` MUST verify it and reject a tampered
message.
**REQ-27** `transfer_many(recipients)` MUST pay each recipient its exact amount from one off-chain
split (N pieces + change), with the same branch + terminal-parent guarantees as a single transfer
(REQ-17/REQ-18).

`transfer_many` MUST dispatch on the parent's shape exactly as single-recipient `transfer()` does: a
laddered ROOT coin through a MULTI-CHILD in-ladder split (one `SP` over `X_m.out[0]` carving N
recipient children plus change), a received CHILD through the child-level equivalent, and only an
un-laddered coin through the plain N+1 branch split (§6.2). A plain split of a laddered parent is the
shape REQ-39 forbids — the split tx and the coin's trigger both spend `F` — so it MUST NOT be built.

> **Was a known divergence, now FIXED.** `transfer_many` used to build the plain split directly on
> any parent, carrying neither the routing nor the `split_coin` refusal. Harmless when the parent was
> self-deposited (only the owner holds `T`), but a RECEIVED laddered coin left its previous owner
> holding a broadcastable no-timelock `T` that could void the split after the pieces were handed
> over: live [B1]. `sdk69` now proves the fixed shape by executing that attack — the retained trigger
> is broadcast and spends `F`, and both recipients still exit unilaterally for their exact amounts,
> because `SP` descends from the trigger instead of racing it. `sdk11` asserts the route as well as
> the amounts.

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
| **L-2** | **Sub-economic finality**: a piece whose value is below the cost of defending it is forfeit to the party who split it | **NOT an unconditional option — state the counter, or the row is wrong.** The splitter holds the parent's lowest flat backup, one 112-vB transaction spending `F` that pays them the whole coin and voids the entire subtree at zero marginal cost per extra piece. But the piece holder holds `T` (it travels in `ChildTesrBundle.parent`), `T` also spends `F`, and `T` carries NO timelock — `TRIGGER_SEQUENCE` disables the relative lock and the builder sets no absolute one. So `T` is valid the instant `F` confirms while the backup is valid only at `L_k`: the payee can PRE-EMPT for the whole window, not merely race at the end, and one confirmed `T` kills every flat backup permanently. **A payee who acts keeps their money, always.** What is irreducible is that acting costs the walk (`3 + 2d` transactions, `293d + 375` vB), so below break-even the defence costs more than the piece — and only THERE is the splitter's backup free. The residual is an economic viability bound on small pieces, not a theft option over large ones ([SUBECONOMIC-FINALITY.md](SUBECONOMIC-FINALITY.md)) |
| **L-3** | **No operator-side value rule is possible**: the SE cannot refuse to co-sign a piece below a viability floor | it is blind (G9) — it signs 32-byte hashes and cannot tell a tier from a backup or a 1 500-sat coin from a whole bitcoin. Every value defence is therefore receiver-side. "The SE enforces a floor" is a WRONG proposal and is recorded here so it is not re-proposed |
| **L-4** | **No in-protocol payment atomicity for a plain transfer** | a transfer is a one-way handover. Delivery-versus-payment needs the Lightning latch (§8) or an invoice |
| **L-5** | **Perpetual watching** replaces the pre-TES-R unconditional no-watch window | this is the trade the architecture exists to make: 0 vB of idle rent (G12) in exchange for a REACTIVE duty. Nothing ages while un-broadcast, but once a hostile trigger is public the defence is a race the owner or a tower must enter. On the mainnet schedule no theft transaction can become valid until `e_floor + d_floor` = **288** blocks after that public trigger, and nothing ever expires to the operator |

### 14.2 Open, with a known fix and a named owner

| # | Limitation | Threatens | What closes it |
|---|---|---|---|
| **L-7** | **The sid ↔ aggregate binding is an unauthenticated coordinator column** (CO-1, §0.4 V-4) | G2/G11 for a coin whose acceptance path consults it. A NULL downgrades any coin to the un-laddered lane; a wrong value combines with the rogue-key decomposition (the SENDER picks `user_public_key`) to make an attacker-chosen output pass | attest the binding as the count now is ([D69] closed the count's half). Note the fix that does NOT work and was rejected in writing: `validate_tx0_output_pubkey` cannot serve, for the rogue-key reason above |
| **L-8** | **The attested counter is a plaintext row in an operator-run database** (CO-3 and SM-5, ONE defect filed twice — do not price it twice) | the census's right-hand side. The attestation authenticates the WIRE, not the STORAGE: production runs the lockbox container with no sealed monotonic state, so one `UPDATE … sig_count = sig_count − 1` absorbs a hidden rival and the receiver holds a VALID signature affirming the wrong number | **[D73] The remedy this row used to prescribe DOES NOT WORK, and the correction is the useful part.** It read: an append-only chain `h_n = H(h_{n−1} ‖ sid ‖ n)` published in the attestation, so a rollback yields a head no owner's receipt matches. But that `h_n` is a PURE FUNCTION of `(sid, n)` given a fixed `h_0` — an operator who rolls back to `k` and re-advances regenerates the IDENTICAL `h_k` and `h_{k+1}`, so no honest party can ever hold a contradicting head and the chain detects nothing. It works only if each link commits to per-round data the owner WITNESSED — the round's session bytes, already the primary key of the lockbox's partial-signature cache — so that re-advancing over different traffic produces a head a prior receipt contradicts. The second remedy, a two-store cross-check of the coordinator's `finalized` against the attested count, is sound BUT ORDER-SENSITIVE: read the coordinator FIRST and the attestation SECOND and refuse iff `attested < finalized`; the reverse order refuses an honest coin whenever a co-signature lands between the two calls |
| **L-9** | **One lost co-sign reply strands a coin's off-chain life** (SM-1) | NOT G4 or G8 — nothing is confiscated and the value is recoverable unilaterally. What is lost is the coin's cooperative life: the census counts sighashes but accepts only signed transactions as disclosure, so the coin stays exactly one slot short (it does not compound) and descendants inherit the refusal | persist `{sid, unsigned tx, msg, session, own partial}` and make the idempotent re-serve normative at EVERY gate in front of the SE, not only at the lockbox; plus a census self-check at wallet open that reports DEGRADED rather than idle |
| **L-10** | **A flat backup may legally carry an RGB transition, and nothing binds its assignment** (RGB-1) | G5 on the un-laddered lane | the coloured lane removes the flat backup's role entirely — this closes with §0.4 V-1, which is another reason that row is the one to close first |
| **L-11** | **The coloured lane's economics are denominated in carrier sats while the loss is denominated in ASSET value** (RGB-2), and a plain flat backup over a carrier is a BURN rather than a transfer | G5 | admission floors that price the asset, not the carrier. Today the mitigation is structural: the automatic passes never re-anchor a carrier, they SEVER it ([D46]) |
| **L-12** | **The version selector has no unknown-version reject arm, and the sender picks the floor** (A-12) | G10 and, through it, payment finality: a conveyance at a version carrying neither the key handover nor the transfer signature lets a payer keep the child auth key while the payee books the payment | a two-sided version check and a floor the RECEIVER sets. The FFI performs the downgrade itself today, hard-coding the pre-ladder fields in both directions |
| **L-13** | **Split-tree per-epoch materialisation is an uncharged on-chain rent** (VE-1) | the footprint economics of §7, not a safety goal | price it, or bound the tree shape that can be minted |
| **L-14** | **Batch atomicity**: `transfer_many` / `batch_transfer_tokens` hand off pieces independently | no all-or-nothing across recipients. A dropped hand-off leaves that piece reclaimable by the sender — the split parent is terminal, so there is no double-spend | an atomic multi-piece hand-off. This is the only remaining `transfer_many` caveat; its laddered-parent routing is fixed (REQ-27) |
| **L-15** | **Unilateral-exit fees** — an un-laddered exit broadcasts fixed-fee transactions with no bump | G4 under a fee spike | the package path is BUILT and live-verified but needs a funded UTXO, a signer and a Core RPC endpoint, so a keyless tower cannot use it ([D31]). **The child lane's bump variant DOES exist** — `exit_child_pass_with_bump` and `exit_spine_tip_pass_with_bump`, both wired into `unilateral_exit`. The gap is the WATCH half: `watch_child_pass_seen` and `watch_spine_tip_pass_seen` have no bump variant, so a tower defending a child tier is stuck at the rate it was signed at |
| **L-16** | **Blind-SE ancestor binding** — the SE stores no per-sid funding outpoint, so a receiver cannot bind `terminal_parents` ids to specific outpoints | the un-laddered lane's defence against SUBSTITUTION of terminal decoys (omission is defeated by the count check) | superseded on the laddered shape by the census, which binds to the attested counter rather than to named ids. Closes with §0.4 V-1 |
| **L-17** | **Amount width** — coin sats are booked as `u32`; a single coin above ~42.9 BTC would truncate | nothing at the intended per-coin sizes; it is not guarded | widen the type |
| **L-18** | **Mint concurrency** — `mint_tokens` isolates the fresh allocation by a before/after snapshot and deliberately does not hold the wallet lock across its on-chain wait | a concurrent same-asset receive into the SAME wallet during a mint could be misattributed | issuers must not mint and receive the same asset concurrently |

### 14.3 Measured limits — true of the design, not defects in it

These are consequences of arithmetic. Under §0.1 a measurement overrules a design statement, so they
are stated as limits rather than as things to fix.

* **Split depth is capped at 8 on mainnet (19 transactions)**, 54 on regtest (111) — [D53]. Deeper
  children are unadoptable at every tip, and the build side refuses to mint what the receive side
  would refuse.
* **Hop capacity is 100 decrements, of which 99 are USABLE** — [D62]. Hop 100 lands the locktime
  exactly on the co-sign anchor, and the receiver refuses `lock_time <= tip`.
* **K = 1 bounds the payees of one coloured PAYMENT, not the payments of one carrier** — [D43]. The
  sender's change lands on a spine tip, and that tip is payable again.
* **The P2A anchor slot is an auction, not a race** — [D45]. An under-paying squat is refused; an
  over-paying one RAISES the tier's effective feerate at the attacker's expense. TRUC contention is
  a price, not a denial of service.
* **BLOCK SPACE IS BOUGHT WITH PAYMENT FREQUENCY, NOT WITH PAYMENT GRANULARITY — and in the split
  lane the claim can INVERT.** State it with the bound or not at all.

  | shape | on-chain cost | against |
  |---|---|---|
  | whole coin (a ROOT holder), ~99 off-chain hops | 1 deposit + 112-vB re-anchor + 1 cooperative exit ≈ **330 vB** | ~10 890 vB for 99 on-chain payments — **≈33×** |
  | split child, settled ALONE, as SHIPPED | the pre-signed walk, `3 + 2d` transactions — **668 vB** at depth 1 | ~110 vB had the payment been made on chain — **6× WORSE** |
  | split child, settled ALONE, floor available ([D77]) | spine + ONE cooperative spend of `SP.out[j]` = **4 txs** | not implemented — `withdraw` routes children to the walk before checking confirmation |
  | k siblings, settled together | spine once + one `combine_leaves` transaction: **`3 + 1`**, not `3 + 2k` | the saving returns |

  **The 1-tx cooperative exit belongs to a ROOT holder, not to a payee.** A child's funding
  `SP.out[j]` is un-broadcast, so `withdraw::execute` has no confirmed outpoint to spend and the SDK
  routes it to the unilateral walk by name. Anyone who RECEIVED a non-exact payment is in the second
  row, not the first.

  A child has **no one-transaction cooperative exit** — its funding output `SP.out[j]` is
  un-broadcast, and the root that could have been spent directly was terminalized by the split. So a
  split piece's only route on chain is materialising its ancestor spine, and there is no shortcut to
  invent. **The design converts payment frequency into block space at a good rate and payment
  granularity at a bad one.** `min_child_value` (§5.1) is what keeps the second row from being minted
  in the first place; batching (a sweep that accumulates siblings and settles them with one combine)
  is what would make it cheap. The primitive exists; the sweep does not (§14.2 L-13).

* **THE ON-CHAIN CADENCE IS REAL, AND IT IS THE FLAT CALENDAR — not the tier chain** ([T-1]/[T-2]).
  This is the number to quote when someone asks what the design saves, because "zero rent" is true of
  the tiers and false of the coin. On the mainnet profile (`initlock = 10 000`, `interval = 100`):

  | | |
  |---|---|
  | a coin's calendar runway from a fresh anchor | **10 000 blocks ≈ 69 days** |
  | what each whole-coin hop costs of it | **100 blocks** — so 100 decrements, **99 usable** ([D62]) |
  | what one re-anchor costs | **one 112-vB transaction** |
  | so one on-chain transaction buys | **min(99 off-chain payments, 69 days)**, whichever binds first |
  | amortised | **≈ 1.13 vB per payment**, against ~110 vB for a plain on-chain payment — **≈ 97×** |
  | idle rent | **≈ 589 vB per coin-year**, against ≈ 5 840 pre-TES-R — **≈ 10×**, not ∞ |

  A SPLIT PIECE does not get its own 10 000 blocks: it inherits what remains of the parent's, which
  is why its exit deadline is a height the splitter owns (L-2, §14.1). And each hop spends calendar
  whether or not time passes, so a coin transferred 99 times must be re-anchored no matter how young
  it is.

* **The floors are rate-evaluations, not constants.** At the shipped `committed_fee_rate = 3.0`:
  plain rung 615, coloured rung 744, `min_child_value` 1 560, `min_spine_tip_value` 945, plain root
  floor 2 175, coloured ROOT 2 562, coloured CHILD 1 818 ([D44]). Quoting one without its rate is
  quoting a rate.

### 14.4 Closed — recorded so they are not re-raised

| was | closed by |
|---|---|
| the sig-count TRANSPORT half (a coordinator under-reporting `num_sigs`) | [D8-CLOSE] — attested count and budget in one signature over a caller-chosen nonce |
| terminality read from an unsigned coordinator boolean | [D54] — routed through the attested budget, with a file-census guard |
| **the attestation verified against a key that has no chain anchor for deep ancestors** (TRUST-MODEL B11) | [D69] — one pinned enclave identity; resolution pin → config → REFUSE |
| T-4: the split window measured against a FRESH epoch by the builder and the REMAINING one by the payee | [D55] — derived from the parent's own conveyed backup chain |
| B.8: census head truncation | [D49] — not a defect; the census already refuses it |
| the depth cap published as 10 / 23 | [D53] — it was measured against the bare latency rule, not the rule that admits |
| **the sender-DECLARED `fee_rate`, on both synchronous verifiers** (GAP A / GAP B) | [D72] — `bundle.fee_rate` is bound to `TesrParams::for_network(..).committed_fee_rate` at the top of `verify_bundle_ex`, ahead of every value law. The child lane inherits it because `verify_child_bundle` re-verifies its embedded parent through the same function. Both tripwires fired and are now attack tests; the honest-bundle controls still pass |

> **P0 remediation status (2026-07-05 review).** The second adversarial review's six P0 blockers are
> now **FIXED on `feat/spark`** and verifiable in code: the enclave/challenge nonce-reuse crypto break
> (C1 — challenge-binding refuses reuse, `sign.rs`), the two SSP fund-loss bugs (C2/C3 — SSP
> pre-payment recipient/amount gate, `ssp.rs`), the split-locktime exit-race inversion (H5 — branch
> txs are now locktime-free, INV-4), branch-conflict masking (H1 — `reject_non_tree_branch`,
> `transfer_receiver.rs`), token-carrier destruction (H2 — carrier excluded from plain-BTC split,
> `transfer.rs`), and the mnemonic-only-backup durability gap (H3 — recovery bundle).
>
> The caveat this note used to carry — "the **SGX lockbox** must be rebuilt and redeployed for the
> enclave-side single-use secnonce to take effect" — is closed in code, and was mis-stated besides:
> the lockbox is a plain C++ service, not an SGX enclave, and it is the lane that runs. It has had
> the atomic consume since P0-1 (`load_and_consume_secnonce`, `lockbox/src/db_manager.cpp`, called
> from `server.cpp`), and `9cfe48f` applied the same consume to the SGX enclave lane
> (`enclave/App/database/db_manager.cpp`, `statechain/sign.cpp`), which had silently never had it.
> The coordinator-side challenge binding (INV-23 / ERR-12, `server/src/endpoints/sign.rs`) is a
> third, independent stop and needs no enclave rebuild at all. **That last caveat is now discharged:**
> the full E2E suite (regtest + lockbox + RLN) was re-run on 2026-08-14 — 85 tests, 84 green; the one
> failure was a stale pre-[D69] `mercury-ssp` BINARY, not a protocol defect, and it passed once
> rebuilt. Workspace unit + guard tests: **794, 0 failures** (`cargo test --workspace --tests`).
> See [REVIEW.md](REVIEW.md#second-adversarial-review-2026-07-05--full-protocol-production-readiness-pass).

Unit tests live in `clients/libs/rust-sdk/src/*` (`#[cfg(test)]`); E2E dispatch via
`SDK_E2E`/`RGB_E2E` in `clients/tests/rust`; upstream Mercury suite runs by default.
