# Mercury Utexo — documentation

Mercury Utexo is a Bitcoin L2 with Spark-class UX on a **single statechain entity** (SE). The SE
co-signs a 2-of-2 with the owner under **blind MuSig2**: it receives a session commitment, never the
transaction, the values or the destinations, and it holds no Bitcoin library and no trustworthy
chain access. Users deposit BTC or RGB assets onto statechain coins and then transact off-chain and
instantly — any amount, tokens, and Lightning in both directions — while every **laddered** coin
stays unilaterally exitable to L1 without anyone's permission. That qualifier is load-bearing since
2026-09-06: a coin's ladder is now its ONLY exit material, so a coin that has none has no unilateral
exit either (see "What the 2026-09-06 rule changed" below). A payment broadcasts nothing and spends
**0 vB on chain** at the moment it is made; the cost of eventually settling it on L1 is real, and
[PARTIAL-PAYMENT-ECONOMICS.md](utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md) is the one place to take a
per-payment figure from — it prices the leaf lane, states the side on which this design loses, and
marks which of its numbers rest on a path that is design rather than built.

**The coin shape.** Every plain deposit is **laddered at first sight of its funding transaction** —
while `F` is still `IN_MEMPOOL`, not at confirmation — as funding `F` (2-of-2) → `T` TRIGGER (no
timelock) → `X_m` EXTENSION (relative CSV) → `S_k` STATE (relative CSV). All three tiers are v3/TRUC
with a P2A anchor and all three stay **un-broadcast**; relative timelocks only start counting once the
parent confirms and `T` carries none, so **an idle coin never ages**. The enclave signature count
after a deposit is **3** (`T`, `X_0`, `S_0`). A transfer co-signs a state one delta *lower* than the
one it replaces, so the new owner's exit matures first. Payments are arbitrary amounts, so an ordinary
payment is an **in-ladder split** and the recipient receives a first-class **child**: the key handover
completes, the sender is locked out, and children re-transfer whole or split again. RGB assets ride as
**carriers**: `SdkConfig::colored_ladder` is not a policy bool but READS the network's compiled-in
enclave attestation pin, so colouring is ON where an identity is pinned (regtest today) and OFF where
none is. Lightning runs through a HODL-invoice latch.

**What the 2026-09-06 rule changed, and it cuts both ways.** The **flat backup chain** over `F` — a
retained chain of decrementing *absolute* locktimes — is **DELETED**. A coin's ladder is now its only
exit material, a transfer conveys an empty `backup_transactions` vector, and `coin.locktime` is `None`
for life.

- **What that buys.** Nothing on a coin matures on its own, so **a coin can no longer be taken from
  its owner on a fixed date by a prior owner.** There is no epoch, no absolute deadline, no calendar
  a receiver must read and no height at which a past owner's retained transaction ripens into a
  capture. A laddered coin has exactly one clock and it is *reactive*: the obligation begins only if
  someone broadcasts the trigger, and a keyless third-party tower can carry it. What a past owner
  still holds is the shared, un-timelocked trigger and superseded states — pre-emptable, loud when
  broadcast, and beaten per tier by the honest owner's lower CSV.
- **What it costs.** The flat backup used to give every coin a unilateral exit that needed **no
  attestation at all**, and nothing replaced it. `TesrParams::attestation_identity_const` returns
  `None` for mainnet and for every public testnet (testnet, testnet3, testnet4, signet); only regtest
  carries a compiled-in pin. The SDK `claim()` establish pass — the lane every wallet user is on —
  binds against the coordinator's aggregate through the attested `get_statechain_info`, so with
  neither a pin nor a configured identity it records `LadderSkipReason::AttestationIdentityUnpinned`
  and ladders **nothing**. The deposit is still **booked**, and then has **no exit material**: it
  cannot be conveyed and it cannot be unilaterally exited. **Cooperative withdrawal is the only route
  out**, which reintroduces exactly the SE-liveness dependence the ladder exists to remove. (The
  `LadderAtSight::Plain` lane, `mercuryrustlib::update_coins`, is the exception — `tesr::establish_auto`
  and `cosign_tier` never consult the pin, so that lane ladders unpinned.) No enclave is provisioned
  on any of those networks, so this is a **not-yet-deployable state, not a live regression**; pinning
  an identity is the only remedy that restores the SE-free exit.

## [`utexo/spec/`](utexo/spec/README.md) — normative

The authoritative description of the system. Where any other page disagrees with these, these win.

- [SPEC.md](utexo/spec/SPEC.md) — REQ / INV / ERR across SE, client libs, SDK and SSP, with per-item
  traceability to tests and per-section build status.
- [PROTOCOL.md](utexo/spec/PROTOCOL.md) — the tiers and parameters, renewal and rollover, the
  in-ladder split, races, cooperative de-trigger, exit costs.
- [CHILDREN.md](utexo/spec/CHILDREN.md) — first-class split children: key handover, and the per-hop
  census a receiver runs.
- [LIGHTNING.md](utexo/spec/LIGHTNING.md) — the HODL-invoice latch both directions, non-exact
  amounts, failure and rollback.
- [TRUST-MODEL.md](utexo/spec/TRUST-MODEL.md) — party-by-party: what is verified, what is trusted,
  and the boundaries no protocol change removes. Auditors start here.
- [PARTIAL-PAYMENT-ECONOMICS.md](utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md) — the cost model, priced
  on the leaf lane.

## [`utexo/learn/`](utexo/learn/) — conceptual

Explainers for readers who have not read the code: [tldr](utexo/learn/tldr.md),
[core-concepts](utexo/learn/core-concepts.md), [transfers](utexo/learn/transfers.md),
[exits](utexo/learn/exits.md), [tokens](utexo/learn/tokens.md),
[lightning](utexo/learn/lightning.md), [trust-model](utexo/learn/trust-model.md),
[invalidation](utexo/learn/invalidation.md) and its
[deep dive](utexo/learn/invalidation-deep-dive.md), and the
[granularity deep dive](utexo/learn/granularity-deep-dive.md) on sending partial amounts.

## [`utexo/build/`](utexo/build/) — practical

Building on the SDK (`mercury-utexo-sdk`, `clients/libs/rust-sdk`, everything on `UtexoWallet`):
[getting-started](utexo/build/getting-started.md), the
[wallet SDK guide](utexo/build/wallet-sdk.md), the [issuer SDK guide](utexo/build/issuer-sdk.md),
the [API reference](utexo/build/api-reference.md), and the
[testing guide](utexo/build/testing-guide.md) for running the `SDK_E2E` and `RGB_E2E` suites against
a local regtest stack. [`openapi.yaml`](openapi.yaml) is the SE server's HTTP API.

## Conventions

Code is cited by **symbol and file**, never by line number — the guard
`ci-guards/tests/deny_line_number_citations_in_normative_docs.rs` fails the build on a line-number
citation in a normative document. Parts of the specification are **design, not built** and are marked
as such in place, with a status banner at the head of the document that carries them.

*The verdict that stood here — that the **discharge round**'s "enforcement point in the SE is empty,
so nothing on that path is exercised" — is **SUPERSEDED and was false as written**.* The round as a
round (the R0–R9 sequence, round eligibility, the operator-liquidity derivation) was **deleted from
the design**; what replaced it is an owner-triggered **close**, and that is built on both sides — the
lockbox serves a `/collapse_grant` route enforcing REQ-56, `witness::Disclosure` and `prevout_value`
occur throughout `lockbox/src/`, and the coordinator carries `server/src/endpoints/collapse.rs`. Take
the verdict from SPEC.md §5.4 and §5.4.7, not from this index. What is still design rather than built:
the close's *economics* (float, migration window, pricing), the sweep / SSP absorption
(`SdkConfig::sweep_at_claim` ships `false` and `claim()` hard-errors if it is on), the `key_updated`
tightening of `get_preimage` and the `lock_expiry` clock reconciliation on the Lightning lane, and the
restoration half of the cooperative de-trigger.

**Test evidence.** `cargo test` is green for `mercuryrustlib` (387), `mercury-utexo-sdk` (152) and
`ci-guards` (32 suites, no failures), and the E2E crate compiles. The E2E suite was **re-derived** for
the 2026-09-06 rule and most of it has **not been re-run**; commit `dd03ab2` records a subset passing
on a live regtest stack. Ten flows were **deleted** — `rgb01`, `rgb02`, `rgb03`, `rgb05`, `rgb06`,
`rgb08`, `rgb09`, `rgb10`, `sdk73`, `sdk78` — so `RGB_E2E` ids 1, 2, 3, 5, 6, 8, 9, 10 and `SDK_E2E`
73 and 78 no longer dispatch anything and may not be cited. Read a document's banner before quoting a
figure or a flow out of it.
