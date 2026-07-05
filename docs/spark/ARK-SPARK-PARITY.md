# Parity with Spark and Ark out-of-round (Arkade) transactions

This maps our single-SE Mercury statechain (off-chain split/branch transfers + RGB) against the two
comparable off-chain-transfer designs: **Ark out-of-round (OOR) / Arkade** and **Spark**. The short
answer: our off-chain transfer **is** an out-of-round transaction — the SE plays the role of Ark's
ASP / Spark's operator, co-signing an off-chain state change with no on-chain footprint per payment —
and we are at parity with the single-coordinator (Ark) design on every functional axis, with one
honest trust difference from Spark (single SE vs. a FROST operator quorum).

## The core equivalence

| Concept | Ark / Arkade | Spark | Ours |
|---|---|---|---|
| Off-chain value unit | VTXO | leaf / node | statechain coin / sub-coin |
| Coordinator | ASP (single) | FROST operators (threshold) | SE (single, blind) |
| Off-chain transfer | out-of-round tx (ASP co-signs) | key-handover, one-spend-per-node | SE-co-signed un-broadcast split → branch |
| Unilateral exit | redeem / forfeit path | unilateral exit | branch (locktime-free) + backup ladder |

An "out-of-round transaction" in Ark is precisely a transfer that does **not** wait for the ASP's next
on-chain round: the ASP co-signs a new VTXO for the recipient off-chain, instantly. Our
`transfer()` does exactly this — the SE co-signs an un-broadcast split, the recipient gets a branch
coin, nothing hits the chain. Demonstrated by `sdk01` (BTC), `sdk02`/`sdk16` (RGB), and chained
multi-hop off-chain in `rgb03`/`sdk03`.

## Parity matrix (7 dimensions)

| Dimension | Ark OOR / Arkade | Spark | Ours | Parity |
|---|---|---|---|---|
| **Out-of-round / off-chain transfer** | Yes — ASP co-signs a VTXO OOR, no round, no on-chain tx | Yes — off-chain leaf transfer | Yes — SE co-signs un-broadcast split/branch (`sdk01`) | **at parity** |
| **Coordinator trust** | Single ASP: can't steal (VTXO is a shared-output/2-of-2), can grief/withhold; must be honest for double-spend prevention | FROST quorum: honest-majority, no single operator can equivocate | Single blind SE: can't steal (2-of-2), can censor or double-spend-assist; blind to asset/amount | **at parity with Ark; weaker than Spark** |
| **Unilateral exit** | Redeem VTXO via the exit/forfeit path; cost + timeout | Unilateral exit with pre-signed state | Branch (locktime-free) + decrementing-locktime backup; ~267 vB, ~534 sats @2sat/vB, ~1000-block deadline (`sdk07`,`sdk14`) | **at parity** |
| **Double-spend / old-state prevention** | ASP refuses to re-sign a spent VTXO; round revocation of forfeited VTXOs | One-spend-per-node enforced by operators | Single-use / spend-budget terminal node (SE refusal) + locktime ladder defeats stale backups (`sdk08`,`sdk13`) | **at parity in mechanism**; same coordinator-honesty caveat as Ark (`sdk15` documents the fresh-double-sign floor) |
| **Liveness / expiry** | VTXOs **expire**; holder must refresh before expiry or the ASP reclaims — a hard be-online requirement | Off-chain state persists; exit is always available | Coins do **not** expire by default (optional epoch deadline, `rgb07`); the only clock is the exit deadline if the SE misbehaves (~1 week) | **different / arguably stronger** — no forced refresh, at the cost of leaning more on SE honesty |
| **Onboarding** | Board a VTXO (on-chain boarding tx) or receive OOR | Receive off-chain | Receive with **nothing** — no prior UTXO; sender's split creates the coin (`sdk16`) | **at parity / stronger** |
| **Fees & scale** | OOR is free (off-chain); rounds batch on-chain exits | Off-chain transfers free | Off-chain transfers free; exit ~534 sats; ~1k SE ops/s on a dev VM, horizontally scalable | **at parity** |

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
- **No forced expiry** — unlike Ark VTXOs, our coins don't expire, so there's no mandatory refresh
  cycle (the trade is more reliance on SE honesty; the optional epoch deadline exists if you want
  Ark-style bounded lifetime).
- **Native RGB assets** — we carry arbitrary RGB contracts (NIA/UDA/CFA/IFA) over the same rails,
  a richer asset model than Ark (BTC-centric) and orthogonal to Spark's native token scheme.

## Tests that demonstrate the OOR-equivalent semantics

| Property | Test |
|---|---|
| Out-of-round off-chain transfer (BTC / RGB) | `sdk01`, `sdk02`, `sdk16` |
| Multi-hop off-chain (received value re-spent OOR) | `rgb03`, `sdk03` |
| One-spend-per-node (double-spend prevention) | `sdk08`, `rgb04` |
| Unilateral exit + cost/deadline | `sdk07`, `sdk14` |
| Stale-state defeat + watcher | `sdk13`, `sdk14` |
| Coordinator trust floor (fresh double-sign) | `sdk15` |
| Onboarding with nothing | `sdk16` |

## Verdict

Our off-chain transfer **is** an out-of-round transaction with the same shape as Ark/Arkade OOR and
Spark's off-chain transfer: instant, off-chain, coordinator-co-signed, unilaterally exitable. We are at
functional parity with Ark's single-ASP design and with Spark's transfer semantics; the one real gap is
the **single-SE trust model vs. Spark's threshold operators**, plus the server-side (rather than
attested) enforcement of the terminal-node rule. Both are closed by the same move — threshold-signing
the SE and pushing single-use enforcement into the attested enclave — not by any client-side layer.
