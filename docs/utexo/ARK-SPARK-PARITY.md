# Parity with Spark and Ark out-of-round (Arkade) transactions

This maps our single-SE Mercury statechain (off-chain TES-R transfers + RGB) against the two
comparable off-chain-transfer designs: **Ark out-of-round (OOR) / Arkade** and **Spark**. The short
answer: our off-chain transfer **is** an out-of-round transaction — the SE plays the role of Ark's
ASP / Spark's operator, co-signing an off-chain state change with no on-chain footprint per payment —
and we are at parity with the single-coordinator (Ark) design on every functional axis, with one
honest trust difference from Spark (single SE vs. a FROST operator quorum).

## One protocol, two coin shapes

There is **one** protocol — TES-R. `claim()` establishes a ladder for every fresh confirmed **root**
coin, unconditionally; there is no protocol switch and no second lane. Inside that one protocol, two
coin *shapes* exist, **both current**:

- **Laddered** — every plain deposit. Trigger `T` → extension `X_m` → state `S`, all **un-broadcast**,
  with **relative** (CSV/BIP-112) locks, so nothing matures until someone puts `T` on-chain and an
  idle coin — or an entire idle split DAG — never ages (PROTOCOL.md §5.2).
- **Un-laddered** — an RGB **carrier** is deliberately *never* laddered: a plain tier spend of a
  carrier would destroy the allocation (terminal-freeze, PROTOCOL.md §5.10). A split sub-coin whose
  funding is still un-broadcast likewise cannot root a trigger. These keep the signed-once backup and
  transfer by **backup-chain handover**. This shape is **load-bearing for RGB assets**, not a legacy
  lane (`sdk52` proves the carrier ⊥ ladder invariant on a live wallet that also holds a laddered
  plain coin; `sdk39` exits a depth-2 colored sub-coin through its backup chain).

Every row below that cites a cost, a wait, or a deadline says which shape it is talking about.

## The core equivalence

| Concept | Ark / Arkade | Spark | Ours |
|---|---|---|---|
| Off-chain value unit | VTXO | leaf / node | statechain coin / split child |
| Coordinator | ASP (single) | FROST operators (threshold) | SE (single, blind) |
| Off-chain transfer | out-of-round tx (ASP co-signs) | key-handover, one-spend-per-node | SE key-handover + one blind co-signature on a fresh un-broadcast state (laddered) / colored split (carrier) |
| Unilateral exit | redeem / forfeit path | unilateral exit | un-broadcast TES-R ladder `T → X_m → S` (relative CSV) / signed-once backup chain (carrier) |

An "out-of-round transaction" in Ark is precisely a transfer that does **not** wait for the ASP's next
on-chain round: the ASP co-signs a new VTXO for the recipient off-chain, instantly. Our
`transfer()` does exactly this — the SE blind-co-signs a fresh state one CSV step **below** the one it
replaces (or, on a carrier, an un-broadcast colored split) and rotates its key share to the recipient;
nothing hits the chain. Demonstrated by `sdk01` (BTC), `sdk02`/`sdk16` (RGB), `sdk41`/`sdk49`
(laddered Model-A transfer), and chained multi-hop off-chain in `rgb03`/`sdk17`/`sdk60`.

**Received split children are first-class** (`docs/utexo/CHILDREN.md`) — this is the Spark
property, not a weakened variant. The claim completes the standard SE key handover, so the receiver
co-owns `A_child` (invariant across the rotation, which is what keeps the pre-signed exit chain valid)
and the sender is permanently locked out. The child can then be paid onward off-chain **whole**
(`child_retransfer`) or **split again** (`child_in_ladder_pay`, a depth-2 `ancestors` chain). Each hop
costs exactly one co-signature and discloses exactly one superseded state, which the receiver's census
counts and proves out-raced. `sdk60` runs alice → bob → carol with the funding outpoint unspent
throughout; `sdk17` does the partial (non-exact) second hop.

## Parity matrix (7 dimensions)

| Dimension | Ark OOR / Arkade | Spark | Ours | Parity |
|---|---|---|---|---|
| **Out-of-round / off-chain transfer** | Yes — ASP co-signs a VTXO OOR, no round, no on-chain tx | Yes — off-chain leaf transfer | Yes — SE key-handover + one blind co-signature on an un-broadcast state/split (`sdk01`, `sdk41`, `sdk49`); received children re-spend off-chain whole or partial (`sdk60`, `sdk17`) | **at parity** |
| **Coordinator trust** | Single ASP: can't steal (VTXO is a shared-output/2-of-2), can grief/withhold; must be honest for double-spend prevention | FROST quorum: honest-majority, no single operator can equivocate | Single blind SE: can't steal (2-of-2), can censor or double-spend-assist; blind to asset/amount | **at parity with Ark; weaker than Spark** |
| **Unilateral exit** | Redeem VTXO via the exit/forfeit path; cost + timeout | Unilateral exit with pre-signed state | *Laddered*: broadcast the un-broadcast ladder `T → X_m → S`, 372–828 vB, wait `E_m + Δ_k` ≤ 2,160 blocks (~15 d) fresh and **shrinking** 36 blocks per hop/renewal (`sdk50` via the public SDK, `sdk40` PART 1 against real consensus). *Un-laddered carrier*: the signed-once backup chain, ~267 vB / ~534 sats @2 sat/vB at depth 1 (`sdk39` exits depth 2) | **at parity** |
| **Double-spend / old-state prevention** | ASP refuses to re-sign a spent VTXO; round revocation of forfeited VTXOs | One-spend-per-node enforced by operators | Single-use / spend-budget terminal node (SE refusal) **plus consensus**: every fresh state carries a strictly lower CSV, so a stale one can never win the race, and an old epoch's parent becomes unconfirmable (`sdk40` PART 2). The receiver's census (`se_num_sigs == flat_backups + Σ conveyed_tiers`) proves no hidden state was withheld (`sdk58`, `sdk46`/`sdk47`/`sdk54`, `sdk60` at depth 2) | **at parity in mechanism**; same coordinator-honesty caveat as Ark (`sdk15` documents the fresh-double-sign floor) |
| **Liveness / expiry** | VTXOs **expire**; holder must refresh before expiry or the ASP reclaims — a hard be-online requirement | Off-chain state persists; exit is always available | Coins do **not** expire. A laddered coin has **no calendar clock at all** — the CSV ticks only after somebody broadcasts `T`, so an idle coin never ages and pays 0 vB rent; renewal and rollover are off-chain and unbounded ("off-chain forever", `sdk43`). A hostile trigger is the only clock, and a **keyless** watchtower answers it (`sdk51`, `sdk45`). The un-laddered carrier keeps its signed-once backup deadline, plus the optional epoch deadline (`rgb07`) | **stronger** — no forced refresh, no expiry, and zero idle footprint |
| **Onboarding** | Board a VTXO (on-chain boarding tx) or receive OOR | Receive off-chain | Receive with **nothing** — no prior UTXO; sender's split creates the coin (`sdk16`) | **at parity / stronger** |
| **Fees & scale** | OOR is free (off-chain); rounds batch on-chain exits | Off-chain transfers free | Off-chain transfers free and idle coins cost **0 vB**; laddered exit 372–828 vB; ~1k SE ops/s on a dev VM, horizontally scalable. One floor to know: a split child funds its **own** two tiers plus dust, so a piece must clear `min_child_value` (1,306 sat @2 sat/vB) — below it the SDK refuses the split up front (`sdk58`, `sdk59`) | **at parity** |

## Where we are honestly weaker

1. **Single SE vs. Spark's FROST quorum.** Our biggest trust delta. A single blind SE can censor or
   assist a double-spend (it can't steal — every coin is 2-of-2). Spark's threshold operator set means
   no single party can equivocate. We match Ark, which is also single-ASP. Closing it means
   threshold-signing the SE (FROST), the same architecture Spark uses.
2. **Terminal-node enforcement is server-side, not attested.** Our single-use/budget check lives in
   the SE's Rust server (operator-mutable), not the lockbox enclave, and the `/spend_budget` response
   is unsigned. A malicious operator can bypass the check and have the enclave co-sign a double-spend
   (`sdk15`). Ark's ASP has the analogous power. Closing it: move the check into the attested enclave
   + sign the terminal attestation so a lie becomes a portable fraud proof.
3. **No slashing / transparency log.** Ark and Spark deployments can add bonded operators + published
   state so misbehavior is punishable. We have Nostr wired for transfer transparency but do not yet
   publish per-co-signature commitments for independent watchers to audit.

## Where we are at parity or stronger

- **Out-of-round transfers, unilateral exit, and one-spend-per-node** are all present and tested.
- **Onboarding with nothing** (`sdk16`) — a receiver needs no prior on-chain UTXO; the sender's
  off-chain split mints the coin. This matches Ark's "receive OOR" and avoids the on-chain boarding tx.
- **First-class received children** (`sdk60`, `sdk17`) — the Spark property, in full. A received split
  piece is not an exit-only claim: the claim completes the SE key handover, so the receiver co-owns
  `A_child` and can pay it onward off-chain, whole (`child_retransfer`) or split again
  (`child_in_ladder_pay`). Multi-hop value moves with the funding outpoint untouched.
- **No forced expiry, and zero idle rent** — unlike Ark VTXOs, our coins don't expire *and* a laddered
  coin has no calendar clock at all: relative locks don't tick until someone broadcasts the trigger, so
  there is no mandatory refresh cycle and no per-coin on-chain rent. Renewal and rollover are off-chain
  and unbounded (`sdk43`). The optional epoch deadline (`rgb07`) remains if you want an Ark-style
  bounded lifetime.
- **Lightning both directions on the ladder** — a laddered coin pays and receives over Lightning via
  the HODL latch (`sdk63`, `sdk64`, `sdk67`; non-exact amounts in `sdk65`, failure/rollback in `sdk66`
  and `sdk68`). The LN-latched piece is the one case that stays terminalized: it sits unclaimed past
  the pending-transfer lock's window.
- **Native RGB assets** — we carry arbitrary RGB contracts (NIA/UDA/CFA/IFA) over the same rails,
  a richer asset model than Ark (BTC-centric) and orthogonal to Spark's native token scheme. This is
  exactly what the un-laddered carrier shape is for (`sdk52`, `sdk39`).

## Tests that demonstrate the OOR-equivalent semantics

| Property | Test |
|---|---|
| Out-of-round off-chain transfer (BTC / RGB) | `sdk01`, `sdk02`, `sdk16` |
| Off-chain transfer on the ladder (Model A) | `sdk41`, `sdk49` |
| Multi-hop off-chain (received value re-spent OOR) | `rgb03`, `sdk17` (partial second hop), `sdk60` (whole child, alice→bob→carol) |
| One-spend-per-node (double-spend prevention) | `rgb04`; census `sdk58`, `sdk46`/`sdk47`/`sdk54` |
| Unilateral exit + cost/wait — laddered | `sdk50` (public SDK walks `T → X_m → S`), `sdk40` PART 1 (real consensus), `sdk58` (11 adversarial cases) |
| Unilateral exit — un-laddered RGB carrier | `sdk39` (depth-2 colored exit), `sdk52` (carrier is never laddered) |
| Stale-state defeat + watcher | `sdk40` PART 2 (stale state dies at consensus), `sdk51` (watchtower defends a hostile trigger), `sdk45` (keyless tower, no key material in the bundle) |
| Off-chain forever (unbounded renewal + rollover) | `sdk43` |
| Coordinator trust floor (fresh double-sign) | `sdk15` |
| Onboarding with nothing | `sdk16` |
| Lightning both directions on the ladder | `sdk63`, `sdk64`, `sdk67`; `sdk65` (non-exact), `sdk66`/`sdk68` (failure paths) |

Two coverage claims this table used to carry are **gone on purpose, not repointed**: per-hop
*invalidation* scaling and the *sats-granularity* fee model were retired with the ladder. Idle coins
never age, so there is nothing to invalidate on a clock; and the in-ladder split carries its own fee
model (`tier_out_total` / `committed_fee_for_outputs`, `min_child_value`) rather than the old
backup-fee floor. Nothing about the security claims above depends on either.

## Verdict

Our off-chain transfer **is** an out-of-round transaction with the same shape as Ark/Arkade OOR and
Spark's off-chain transfer: instant, off-chain, coordinator-co-signed, unilaterally exitable — and,
with first-class children, re-spendable off-chain for arbitrarily many hops without ever touching the
chain. We are at functional parity with Ark's single-ASP design and with Spark's transfer semantics,
and ahead of Ark on liveness (no expiry, no forced refresh, 0 vB idle rent). The one real gap is
the **single-SE trust model vs. Spark's threshold operators**, plus the server-side (rather than
attested) enforcement of the terminal-node rule. Both are closed by the same move — threshold-signing
the SE and pushing single-use enforcement into the attested enclave — not by any client-side layer.
