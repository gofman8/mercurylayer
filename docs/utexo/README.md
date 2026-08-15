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
descendant of the trigger — admission floor `min_child_value` = 1310 sat at 2 sat/vB); **first-class
received children** (the claim completes the SE key handover, so a received piece pays onward
off-chain, whole or split); **Lightning both directions** on the ladder via a HODL-invoice latch.

> **Status** — one protocol (TES-R). **Zero tests pin the pre-TES-R design.** Live suite
> `SDK_E2E=1..68` (not contiguous — some numbers were retired) plus the `chaos22` fuzzer, alongside
> `RGB_E2E=1..14` and the complete upstream Mercury suite.

## Start here

- **Building on the SDK** → [build/getting-started.md](build/getting-started.md) →
  [build/wallet-sdk.md](build/wallet-sdk.md) → [build/api-reference.md](build/api-reference.md) →
  [build/testing-guide.md](build/testing-guide.md); tokens in
  [build/issuer-sdk.md](build/issuer-sdk.md).
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
- [PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md) — the cost model: what a payment
  costs off-chain and on, the leaf lane (never the whole-coin lane, [D82]), and §0.8's discharge-round
  footprint that SPEC §5.4 cites. *Normative — SPEC defers to it for every per-payment figure.*
- [DECISIONS.md](DECISIONS.md) — every design decision with its evidence, its retractions and the
  reasoning that survived them. *Normative — the authority every other document cites.*
- [TRUST-MODEL.md](TRUST-MODEL.md) — party-by-party matrix: what is verified (with file and test) vs
  trusted, and the boundaries no protocol change removes. *Normative; auditors start here.*
  ([D60] This entry said only *Auditors.* and was therefore outside the line-citation census, which
  derives its set from these labels — while `SPEC.md` and `PROTOCOL.md` both defer to it for what is
  trusted, and D54's residual bound is stated nowhere else but its B11 row. A document the
  specification defers to is normative whatever the reader-audience note says.)