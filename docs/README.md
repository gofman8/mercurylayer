# Mercury Utexo — documentation

Mercury Utexo is a Bitcoin L2 with Spark-class UX on a **single statechain entity** (SE). The SE
co-signs a 2-of-2 with the owner under **blind MuSig2**: it receives a session commitment, never the
transaction, the values or the destinations, and it holds no Bitcoin library and no trustworthy
chain access. Users deposit BTC or RGB assets onto statechain coins and then transact off-chain and
instantly — any amount, tokens, and Lightning in both directions — while every coin stays
unilaterally exitable to L1 without anyone's permission. A payment broadcasts nothing and spends
**0 vB on chain** at the moment it is made; the cost of eventually settling it on L1 is real, and
[PARTIAL-PAYMENT-ECONOMICS.md](utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md) is the one place to take a
per-payment figure from — it prices the leaf lane, states the side on which this design loses, and
marks which of its numbers rest on a path that is design rather than built.

**The coin shape.** Every plain deposit is **laddered** at `claim()` — funding `F` (on-chain, 2-of-2)
→ `T` TRIGGER (no timelock) → `X_m` EXTENSION (relative CSV) → `S_k` STATE (relative CSV). All three
tiers are v3/TRUC with a P2A anchor and all three stay **un-broadcast**; relative timelocks only
start counting once the parent confirms and `T` carries none, so **an idle coin never ages**. A
transfer co-signs a state one delta *lower* than the one it replaces, so the new owner's exit matures
first. Payments are arbitrary amounts, so an ordinary payment is an **in-ladder split** and the
recipient receives a first-class **child**: the key handover completes, the sender is locked out, and
children re-transfer whole or split again. A separate **flat backup chain** over `F` carries
decrementing absolute locktimes whose lowest is the coin's epoch expiry
(`epoch_deadline_from_flat_backups`, `clients/libs/rust/src/tesr.rs`, which refuses to report a
deadline at all rather than default one); broadcasting `T` spends `F` and kills every flat backup
permanently. RGB assets ride as **carriers**: `SdkConfig::colored_ladder` ships `false`, so a carrier
takes the flat signed-once backup shape rather than a coloured ladder. Lightning runs through a
HODL-invoice latch.

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
as such in place, with a status banner at the head of the document that carries them. The largest is
the **discharge round** (SPEC.md §5.4, priced in PARTIAL-PAYMENT-ECONOMICS.md §4): its enforcement
point in the SE is empty, so nothing on that path is exercised. Also design rather than built: the
sweep / SSP absorption the discharge round rests on, the `key_updated` tightening of `get_preimage`
and the `lock_expiry` clock reconciliation on the Lightning lane, and the restoration half of the
cooperative de-trigger. Read a document's banner before quoting a figure out of it.
