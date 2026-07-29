# Utexo on Mercury + RGB — documentation

Utexo brings full Spark (buildonspark) feature parity to Mercury Layer with a **single statechain
entity** (blind-MuSig2 2-of-2, no FROST multi-operator) and **RGB** as the token standard. Users
deposit BTC or RGB assets onto statechain coins and then transact off-chain, instantly, at no
per-payment on-chain cost — any amount, tokens, and Lightning in both directions — while every coin
stays unilaterally exitable to L1 without anyone's permission.

**The coin shape: TES-R.** Every plain deposit is **laddered** — `claim()` pre-signs a
*Trigger / Extension / State* chain over the on-chain funding output `F`:

```
F (on-chain funding, 2-of-2)
└─ T    TRIGGER    no timelock — signed once at deposit
   └─ X_m  EXTENSION  RELATIVE CSV E_m — renewal replaces it horizontally, off-chain
      └─ S_k  STATE     RELATIVE CSV Δ_k — decrements by δ on every transfer
```

All three tiers are v3/TRUC with a P2A anchor, and all three are **un-broadcast**. BIP-68 relative
timelocks only start counting once the parent confirms, and `T` has no timelock — so **nothing
matures until someone broadcasts `T`**. An **idle coin never ages**: no calendar deadline, no
expiry, **0 vB of idle rent**. A transfer co-signs a fresh state one δ *lower* than the one it
replaces (replace-by-lower-timelock), so the new owner's exit always matures first and the
superseded state is disclosed and counted by the receiver's census. Renewal and rollover are
off-chain and unbounded; `refresh` is the on-chain **re-anchor** primitive, not a deadline reset.
The unilateral exit **walks the pre-signed chain tier by tier**, waiting out each relative timelock.

**One protocol, two coin shapes — both current.** There is no protocol version flag
(`deposit_protocol_version` and `UTEXO_PROTOCOL_DEFAULT` are deleted); `claim()` ladders every fresh
confirmed **root** coin unconditionally. But not every coin is laddered, **by design**:

- **Laddered** — every plain BTC deposit.
- **Un-laddered** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation — terminal freeze), and a **split sub-coin whose funding is un-broadcast**
  cannot root a trigger. These keep the signed-once backup with **decrementing absolute
  nLockTimes** and move by backup-chain handover. This shape is load-bearing for RGB — current, not
  legacy.

Also shipped: **any amount** via the in-ladder split (a state tier spending `X_m.out[0]`, a
descendant of the trigger — admission floor `min_child_value` = 1306 sat at 2 sat/vB); **first-class
received children** (the claim completes the SE key handover, so a received piece pays onward
off-chain, whole or split); **Lightning both directions** on the ladder via a HODL-invoice latch.

> **Status** — one protocol (TES-R). **Zero tests pin a legacy lane.** Live suite
> `SDK_E2E=1..68` (not contiguous — some numbers were retired) plus the `chaos22` fuzzer, alongside
> `RGB_E2E=1..14` and the complete upstream Mercury suite.

## Start here

- **Evaluating the protocol** → [learn/tldr.md](learn/tldr.md) →
  [learn/core-concepts.md](learn/core-concepts.md) → [learn/invalidation.md](learn/invalidation.md)
  → [PROTOCOL.md](PROTOCOL.md) → [PARITY.md](PARITY.md) +
  [ARK-SPARK-PARITY.md](ARK-SPARK-PARITY.md).
- **Building on the SDK** → [build/getting-started.md](build/getting-started.md) →
  [build/wallet-sdk.md](build/wallet-sdk.md) → [build/api-reference.md](build/api-reference.md) →
  [build/testing-guide.md](build/testing-guide.md); tokens in
  [build/issuer-sdk.md](build/issuer-sdk.md).
- **Auditing security** → [TRUST-MODEL.md](TRUST-MODEL.md) → [PROTOCOL.md](PROTOCOL.md) §5 (races,
  de-trigger, exit costs) → [CHILDREN.md](CHILDREN.md) + [LIGHTNING.md](LIGHTNING.md) →
  [SPEC.md](SPEC.md) / [INVALIDATION-SPEC.md](INVALIDATION-SPEC.md) /
  [GRANULARITY-SPEC.md](GRANULARITY-SPEC.md) → [REVIEW.md](REVIEW.md) +
  [AUDIT-2026-07.md](AUDIT-2026-07.md) → [build/testing-guide.md](build/testing-guide.md).

## Learn — conceptual prose, no code required

- [tldr.md](learn/tldr.md) — the whole system on one page. *Anyone.*
- [core-concepts.md](learn/core-concepts.md) — the tour: SE, the TES-R ladder, the two coin shapes,
  in-ladder splits, first-class children, verification at claim, exits. *Start of the learn path.*
- [transfers.md](learn/transfers.md) — what a transfer is: key handover, replace-by-lower-timelock,
  the amount-maker, and what a receiver checks on each shape. *Users and integrators.*
- [exits.md](learn/exits.md) — deposits, cooperative exit, the tier-by-tier unilateral walk, watching
  instead of deadlines, renewal vs re-anchor. *Users and integrators.*
- [tokens.md](learn/tokens.md) — RGB issuance and lifecycle, why the carrier is never laddered, why
  there is no freeze. *Token issuers and holders.*
- [lightning.md](learn/lightning.md) — the HODL-invoice latch, both directions, exact and non-exact.
  *Integrators and LSPs.*
- [trust-model.md](learn/trust-model.md) — short version: what you trust, what is verified instead,
  what timeliness actually requires. *Anyone; auditors start at [TRUST-MODEL.md](TRUST-MODEL.md).*
- [invalidation.md](learn/invalidation.md) — how old state is killed, compared with Spark, Ark
  (Second), SuperScalar and vanilla Mercury; exit-cost tables. *Protocol evaluators.*
- [invalidation-deep-dive.md](learn/invalidation-deep-dive.md) — long-form: the timelock machinery,
  lifecycle walkthroughs, failure scenarios, UX duties, FAQ. *Developers and researchers.*
- [granularity-deep-dive.md](learn/granularity-deep-dive.md) — long-form partial amounts: paying 0.1
  out of 1, the in-ladder split, the floors, token packaging. *Developers and researchers.*

## Build — practical, with code

- [getting-started.md](build/getting-started.md) — the regtest stack, a wallet in 30 lines, and what
  your coin actually is. *First stop for developers.*
- [wallet-sdk.md](build/wallet-sdk.md) — every wallet operation with code: deposit, the three
  transfer routes, splits, exits, watchtowers, Lightning, config knobs. *Wallet developers.*
- [issuer-sdk.md](build/issuer-sdk.md) — launch, mint/burn (IFA) and distribute an RGB asset; carrier
  mechanics and holder guidance. *Token issuers.*
- [api-reference.md](build/api-reference.md) — `UtexoWallet` method-by-method, with signatures,
  events, errors and types. *Reference while coding.*
- [testing-guide.md](build/testing-guide.md) — how to run every suite, what each covers, retired
  numbers, the adversarial coverage map and chaos. *Contributors and auditors.*

## Normative specs & design

- [PROTOCOL.md](PROTOCOL.md) — **the protocol.** TES-R tiers and parameters, renewal/rollover, the
  in-ladder split, races, cooperative de-trigger, exit costs. *Normative.*
- [CHILDREN.md](CHILDREN.md) — first-class received split children: the key-handover design, why the
  reopen approach was rejected, the per-hop census. *Normative.*
- [LIGHTNING.md](LIGHTNING.md) — the adopted HODL-invoice latch: both directions, non-exact amounts,
  failure and rollback, and the one case that stays terminalized. *Normative.*
- [SPEC.md](SPEC.md) — system specification (REQ / INV / ERR) across SE, client libs, SDK and SSP,
  with per-item traceability to tests. *Normative.*
- [TRUST-MODEL.md](TRUST-MODEL.md) — party-by-party matrix: what is verified (with file and test) vs
  trusted, and the boundaries no protocol change removes. *Auditors.*
- [INVALIDATION-SPEC.md](INVALIDATION-SPEC.md) — normative `IVL-*` for old-state invalidation and the
  un-laddered shape's absolute-locktime machinery and duties. *Normative.*
- [GRANULARITY-SPEC.md](GRANULARITY-SPEC.md) — normative `GRN-*` for partial amounts: split bounds,
  floors, raw token units, error semantics. *Normative.*
- [PARITY.md](PARITY.md) — Spark ↔ Mercury+RGB feature matrix, every row citing the live test.
  *Evaluators.*
- [ARK-SPARK-PARITY.md](ARK-SPARK-PARITY.md) — our off-chain transfer mapped against Ark out-of-round
  (Arkade) and Spark, including the honest single-SE trust difference. *Evaluators.*
- [RGB-TEST-PARITY.md](RGB-TEST-PARITY.md) — how the upstream RGB suites map onto RGB running over a
  statechain coin. *RGB integrators.*

## Reviews & status

- [REVIEW.md](REVIEW.md) — two adversarial security reviews: SE co-signing crypto and value rules,
  then a whole-protocol production-readiness pass. *Auditors.*
- [AUDIT-2026-07.md](AUDIT-2026-07.md) — the July 2026 mainnet audit: findings, ratings and
  remediation status, kept as written with a status refresh for the unified protocol. *Auditors.*
- [PLAN.md](PLAN.md) — the build plan and key design decisions; done, with the shipped end state
  stated up front. *Contributors.*
- [PROGRESS.md](PROGRESS.md) — the running work log; the status block is current, the log beneath it
  is historical. *Contributors.*

## Research

Condensed study notes and pricing models. *Researchers and designers.*

- [protocol-notes.md](research/protocol-notes.md) — Spark's core protocol distilled, each bullet
  mapped to what our shipped protocol does instead.
- [sdk-notes.md](research/sdk-notes.md) — Spark's SDK surface and docs sitemap, with our shipped
  equivalents and where the two surfaces genuinely diverge.
- [invalidation-economics.md](research/invalidation-economics.md) — what it costs to enter, hold,
  transact and leave, across feerate, tree depth, coin size and time.
- [granularity-economics.md](research/granularity-economics.md) — the price of exact amounts:
  unpayable-amount map, carrier depletion, token breakevens, colored exit at depth.
- [RGB-BANK-VALIDATION.md](research/RGB-BANK-VALIDATION.md) — (Russian) adversarial validation of the
  batched shared-UTXO "RGB bank" concept against the pinned rgb-consensus line.

## History

`history/` is a **historical record, kept for the reasoning — not a description of current design.**
Both files open with a status block saying what actually shipped; read [PROTOCOL.md](PROTOCOL.md)
for the live protocol.

- [MIGRATION.md](history/MIGRATION.md) — the plan and adversarial reasoning behind removing the old
  lane and making TES-R the only path. **Executed.**
- [SPLIT-FINDINGS.md](history/SPLIT-FINDINGS.md) — the split-transfer review audit trail: the B1
  theft vector, the census holes, the child-bundle FATALs. **All closed or superseded.**
