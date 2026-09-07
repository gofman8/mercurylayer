# Utexo on Mercury + RGB — documentation

Utexo brings full Spark (buildonspark) feature parity to Mercury Layer with a **single statechain
entity** (blind-MuSig2 2-of-2, no FROST multi-operator) and **RGB** as the token standard. Users
deposit BTC or RGB assets onto statechain coins and then transact off-chain, instantly, at no
per-payment on-chain cost — any amount, tokens, and Lightning in both directions — while every coin
stays unilaterally exitable to L1 without anyone's permission.

**The coin shape: TES-R.** A coin's **only** exit material is its ladder — a pre-signed
*Trigger / Extension / State* chain over the on-chain funding output `F`, established at the
**first mempool sighting** of the funding transaction, before it confirms:

```
F (on-chain funding, 2-of-2)
└─ T    TRIGGER    no timelock — signed once, at first sight of F
   └─ X_m  EXTENSION  RELATIVE CSV E_m — renewal replaces it horizontally, off-chain
      └─ S_k  STATE     RELATIVE CSV Δ_k — decrements by δ on every transfer
```

All three tiers are v3/TRUC with a P2A anchor, and all three are **un-broadcast**. BIP-68 relative
timelocks only start counting once the parent confirms, and `T` has no timelock — so **nothing
matures until someone broadcasts `T`**. An **idle coin never ages**: no calendar deadline, no
expiry, **0 vB of idle rent**. That sentence is now unconditional (INV-27): **no coin carries an
absolute-locktime backup transaction** — none at deposit, none at any hop — so nothing on a coin
matures on its own, and no previous owner holds a matured spend of `F`. A transfer co-signs a fresh
state one δ *lower* than the one it replaces (replace-by-lower-timelock), so the new owner's exit
always matures first and the superseded state is disclosed and counted by the receiver's census
(`se_num_sigs == tiers + superseded`). Renewal and rollover are off-chain and unbounded; `refresh`
is the on-chain **re-anchor** primitive, not a deadline reset — there is no deadline. The unilateral
exit **walks the pre-signed chain tier by tier**, waiting out each relative timelock.

**One protocol, one exit material.** There is no protocol version flag — the
`deposit_protocol_version` field and the `UTEXO_PROTOCOL_DEFAULT` escape hatch are deleted — and
there is **no un-laddered lane**: the flat backup chain, its decrementing absolute nLockTimes
(INV-5, RETIRED 2026-09-06), `create_tx1`, the flat conveyance lane and its licence classifier, the
off-chain branch split/combine and the flat exit fallback all **refuse by name**. A coin without a
ladder is a coin with **no exit material**, and it cannot be conveyed. Four states put a coin in
that condition or beside it — a fault, never a lane — and they do **not** all clear themselves. Only
the first two below can be fixed by a later pass, and only in part; the last two cannot be fixed by
this codebase at all:

- a **deposit whose ladder could not be established** at first sight, on the `LadderAtSight::Plain`
  lane, is not booked — `check_deposit` rolls the coin back to `INITIALISED` and returns the error,
  so it is retried on the next pass. On the SDK's `LadderAtSight::Defer` lane the coin is booked
  first and laddered by `claim()`'s own establish pass, so a failure there leaves a BOOKED coin with
  no ladder rather than an unbooked one;
- an RGB **carrier that cannot be coloured** *this pass* (RGB state unavailable, allocation not yet
  booked) has no exit material until a later pass colours it (`LadderSkipReason::RgbCarrier`). A
  plain ladder is never built over a carrier: a plain tier spend would destroy the allocation
  (terminal freeze). A carrier **below the coloured root floor** is the exception that is not
  retried into anything — no later pass can colour it, and the legacy lane that used to pay it now
  refuses at its entry (`refuse_legacy_colored_split_lane`, both sides of the `colored_ladder` flag,
  before any SE co-sign). That piece has no ladder, no conveyance and no SE-free exit; its
  allocation is intact in the stash and its sats withdraw cooperatively. It is an **open policy
  gap**, not a lane (PROTOCOL.md §5.10 rule 4);
- a **plain ladder found over a carrier** — tokens moved onto an already-laddered outpoint — is
  recorded as `PlainLadderOverCarrier`. This one is not a missing ladder but a WRONG one, and it has
  **no remedy in the tree**: `colored_reanchor` refuses a plain-laddered coin by name and `refresh`'s
  plain re-anchor would burn the allocation. The coin is exitable **as satoshis only**, must not be
  conveyed as a carrier, and its allocation is stranded unless a counterparty co-operates off this
  path. Under laddering-at-first-sight the way to avoid it is not to move an allocation onto an
  outpoint that is already plain-laddered — which is every confirmed plain deposit;
- an **SDK deposit on a network with no pinned enclave identity** is recorded
  `AttestationIdentityUnpinned`: `claim()`'s establish pass gates on the attested
  `get_statechain_info`, which cannot verify without a pin. (`check_deposit`'s own
  `LadderAtSight::Plain` lane — the CLI's — ladders without one: `establish_auto` and `cosign_tier`
  never call `get_statechain_info`.) **This one is NOT self-healing, and the obvious summary of it is
  false.** It is not the case that "deposits and exits work without a pin and only receiving does
  not". The deposit IS booked (`IN_MEMPOOL`, then `UNCONFIRMED` / `CONFIRMED`) and gets no ladder,
  so it has **no exit material at all**: it cannot be conveyed and it cannot be unilaterally exited —
  **cooperative withdrawal is the only route out**, which needs the coordinator alive and willing.
  The flat backup used to give that coin a unilateral exit with no attestation involved; it no
  longer exists. Every later pass fails the same way until an identity is pinned or configured.
  Today this is **not-yet-deployable, not a live regression**: mainnet has no enclave provisioned at
  all, and neither do testnet/testnet3/testnet4/signet, so `attestation_identity_const` returns
  `None` for all of them and regtest alone carries a pin.

A **split sub-coin whose funding is un-broadcast** cannot root a trigger of its own, and does not
need one: an in-ladder child or spine tip is laddered by the split that created it, off `SP`, and
its chain reaches back to the parent's on-chain `F`.

Also shipped: **any amount** via the in-ladder split (a state tier spending `X_m.out[0]`, a
descendant of the trigger — admission floor **one satoshi** since REQ-83, with `min_child_value` =
1 560 sat at the shipped 3 sat/vB the floor of the two-rung band only; the thinner bands are payable
but do not exit unaided); **first-class
received children** (the claim completes the SE key handover, so a received piece pays onward
off-chain, whole or split); **Lightning both directions** on the ladder via a HODL-invoice latch.

## Status — what is running, and what is not

The dispatch answers **76 `SDK_E2E` ids** (numbers run to 94 and are **not** contiguous — unused
numbers exist); one of those ids, `SDK_E2E=22`, IS the `chaos22` fuzzer, so the count is 75 `sdk*`
flows plus chaos22, not 77 plus chaos22. `RGB_E2E` answers **8 ids — 4, 7 and 11–16**. The complete
upstream Mercury suite runs alongside. **Zero tests pin any design other than TES-R.**

**No E2E flow has been run against the regtest stack since the flat backup chain was removed.** What
IS measured is the unit level: `cargo test` is green for `mercuryrustlib` (387 passed),
`mercury-utexo-sdk` (152 passed) and the `ci-guards` suites (32 suites, no failures), and the E2E
crate compiles. Every E2E citation in these documents is therefore a traceability pointer, and the
ones marked *re-derived, pending run* are not evidence at all until they are run.

**Retired 2026-09-06, and what it did to the evidence.** The flat backup lane is gone from the
code (see the coin shape above). Every flow whose *subject* was that lane is **deleted, or
re-derived and pending run**. The backup-chain adversarial `sdk55` is **re-derived, NOT retired** —
it still exists and is still dispatched, with its padding and inversion attacks re-aimed at the
ladder, where a conveyed flat backup is refused before the census runs and the flat term is
identically zero. The rest, in the same category: the calendar half of `sdk86`
(now: no calendar clock on a received coin), the exit-headroom gate of `sdk82`/`sdk88`
(`check_exit_headroom_with_margin` has no caller; now: a child has no epoch to run out of), the
`num_sigs == 4` count of `sdk48` (now 3), the `flat_backups` term of `sdk76` (now 0), the
near-deadline leaf and carrier passes of `sdk34`/`sdk87` (the leaf loop is deleted;
`deadline_safety_due` has no laddered subject), `sdk71`'s flat conveyance licences (every licence
now refuses), `sdk39`'s branch-broadcast exit (now a walk on the coloured lane), the branch-lane
`RGB_E2E=1–3, 5, 6, 8–10` — **deleted outright**, files and dispatch arms alike, so those ids no
longer exist and running them is a no-op rather than a refusal assertion — with `RGB_E2E=4` and `7`
re-derived over `single_use` deposits; `sdk73` and `sdk78`, **also deleted outright** (`SDK_E2E=73`
and `78` no longer exist); and the flat-backup assertions of the upstream `tb01`/`tb05`/`ta03`.
None of them is evidence for a laddered claim until the re-derived flow has been run;
[build/testing-guide.md](../build/testing-guide.md) lists them flow by flow, and
`git diff -- clients/tests/rust/src/` is the authority on what has been re-derived so far. Flows
that never touched the flat lane (`sdk04`, `sdk51`, `sdk52`, `sdk65`–`sdk67`, `sdk75`, `sdk81`,
`sdk89`–`sdk92`, `sdk94`, …) stand unchanged; flows touched only to assert the three-co-sign shape
beside what they already proved (`sdk17`, `sdk30`, `sdk40`–`sdk50`, `sdk58`–`sdk60`, `sdk69`,
`sdk74`, `sdk79`, `sdk80`, `sdk84`, …) are re-derived, pending run. Note that `sdk43`, `sdk45`,
`sdk47`, `sdk50` and `sdk69` are in the SECOND list, not the first — each of them was touched, so
none is standing evidence.

Parts of the specification are **DESIGN, not built**, and each is marked as such in place. The
three largest:

- **The discharge round** (SPEC.md §5.4). *The description that stood here — "a source-scan design
  with nothing plant-and-run", with `disclosure` and `prevout_value` occurring "0× in `lockbox/`" —
  is SUPERSEDED and was false when this file was last written.* The enforcement point exists: the
  lockbox serves a `/collapse_grant` route, `witness::Disclosure` and `prevout_value` occur
  throughout `lockbox/src/witness.cpp`, `lockbox/src/db_manager.cpp` and `lockbox/src/server.cpp`,
  `freeze_root_and_store_collapse_sig` freezes the root and stores the collapse partial in one
  transaction, and the coordinator carries `server/src/endpoints/collapse.rs` with `sdk94` as its
  E2E flow (unchanged by the no-flat-backup rule, and not run since). What remains DESIGN is the
  round's *economics* — the operator float, the migration window and the pricing SPEC.md §5.4
  derives — not the SE-side refusal. Read SPEC.md §5.4 for what is built there; do not quote this
  index's older verdict.
- **De-trigger restoration.** The cooperative de-trigger is wired, and was measured end to end —
  the owner answers a griefer's confirmed `T` with a spend at zero CSV wait and the retained tiers
  die (`UtexoWallet::detrigger_to_owner`, `SDK_E2E=89`). That measurement predates the
  no-flat-backup rule: sdk89's own source is unchanged, but the deposit shape underneath it is not
  (three co-signs, no `tx1`), and it has not been re-run since. What does **not** exist is the
  restoration half:
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
