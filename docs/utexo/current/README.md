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

**One protocol, two coin shapes — both current.** There is no protocol version flag — the
`deposit_protocol_version` field and the `UTEXO_PROTOCOL_DEFAULT` escape hatch are deleted, and
`claim()` ladders every fresh confirmed **root** coin unconditionally. But not every coin is laddered, **by
design**:

- **Laddered** — every plain BTC deposit.
- **Un-laddered** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation — terminal freeze), and a **split sub-coin whose funding is un-broadcast**
  cannot root a trigger. These keep the signed-once backup with **decrementing absolute
  nLockTimes** and move by backup-chain handover. This shape is load-bearing for RGB.

Also shipped: **any amount** via the in-ladder split (a state tier spending `X_m.out[0]`, a
descendant of the trigger — admission floor `min_child_value` = 1310 sat at 2 sat/vB); **first-class
received children** (the claim completes the SE key handover, so a received piece pays onward
off-chain, whole or split); **Lightning both directions** on the ladder via a HODL-invoice latch.

## Status — what is running, and what is not

The live suite is 74 `SDK_E2E` cases (numbers run to 91 and are **not** contiguous — unused numbers
exist) plus the `chaos22` fuzzer, alongside `RGB_E2E` cases to `rgb16` and the complete upstream
Mercury suite. **Zero tests pin any design other than TES-R.**

Parts of the specification are **DESIGN, not built**, and each is marked as such in place. The
three largest:

- **The discharge round** (SPEC.md §5.4) is a source-scan design with **nothing plant-and-run**. Its
  enforcement point is empty: `disclosure` and `prevout_value` occur 83× in the client and **0× in
  `lockbox/`**, so the SE would presently co-sign a collapse that pays out nobody.
- **De-trigger restoration.** The cooperative de-trigger is wired and proven — the owner answers a
  griefer's confirmed `T` with a spend at zero CSV wait and the retained tiers die
  (`UtexoWallet::detrigger_to_owner`, `SDK_E2E=89`). What does **not** exist is the restoration half:
  there is no fresh `F′` and no rebuilt `T′/X′_0/S′_0`, so returning off-chain afterwards is a fresh
  deposit. The coloured (168-vB `opret`) variant and the mass-grief prioritization policy are not
  test-covered.
- **The Lightning completion bind** (LIGHTNING.md) — additionally requiring `key_updated = true`
  before `get_preimage` releases — is designed and not built. It must never be applied globally: it
  deadlocks the `sender-settles-first` lane.

Measured figures in these documents are regtest measurements unless the citing section says
otherwise, and each behavioural claim carries the symbol or the test that establishes it. Evidence
is cited by **symbol and file**, never by line number.

## The documents

- [SPEC.md](SPEC.md) — system specification (REQ / INV / ERR) across SE, client libs, SDK and SSP,
  with per-item traceability to tests, and per-section build status. *Normative.* *For implementers
  and auditors.*
- [PROTOCOL.md](PROTOCOL.md) — **the protocol.** TES-R tiers and parameters, renewal and rollover,
  the in-ladder split, races, cooperative de-trigger, exit costs. *Normative.* *For protocol
  engineers.*
- [CHILDREN.md](CHILDREN.md) — first-class received split children: the key-handover design and the
  per-hop census a receiver runs on each hop. *Normative.* *For wallet and SDK developers.*
- [LIGHTNING.md](LIGHTNING.md) — the HODL-invoice latch: both directions, non-exact amounts, failure
  and rollback, and the one case that stays terminalized. *Normative.* *For integrators and LSPs.*
- [TRUST-MODEL.md](TRUST-MODEL.md) — party-by-party matrix: what is verified (with file and test) vs
  trusted, and the boundaries no protocol change removes; its B11 row states residual bounds given
  nowhere else. *Normative — SPEC.md and PROTOCOL.md both defer to it for what is trusted.* *For
  auditors; start here.*
- [PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md) — the cost model: what a payment costs
  off-chain and on, priced on the **leaf** lane rather than the whole-coin lane, and the §0.8
  discharge-round footprint that SPEC.md §5.4 cites. *Normative — SPEC.md defers to it for every
  per-payment figure.* *For protocol evaluators and anyone quoting a cost.*
- [README.md](README.md) — this index: what the system is, what ships, what does not, and which
  document answers which question. *For everyone, first.*
