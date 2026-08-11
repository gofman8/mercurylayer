# Trust model — who trusts whom, what is verified instead, and what cannot be solved

> ## ⚠️ Direction of travel: ONE COIN TYPE
>
> The trust boundaries below are stated for both coin shapes as built today — *laddered* (TES-R) and
> *un-laddered* (RGB carriers and un-broadcast split sub-coins). **That is a transitional state, not
> the target architecture.** The decided direction is a single coin type; the un-laddered shape is
> being removed.
>
> Several residuals recorded here belong to that shape and go away with it — notably **B2**, the
> terminal-parent proofs that are checked by COUNT rather than cryptographically bound to the branch
> inputs. An independent review rates that CRITICAL; it is not being patched, because the code that
> contains it is scheduled for deletion rather than hardening.
>
> Two boundaries that do NOT go away, and should not be read as transitional: the single-SE trust
> unit itself, and the ~6.9-day **root** epoch (an unspent on-chain root plus the original
> depositor's maturing absolute-locktime backup — on-chain state, escapable only by an on-chain
> colored re-anchor that does not exist today).
>
> Mechanism and status: [CTESR-GATE.md](CTESR-GATE.md). Foundation landed; **colouring not yet
> wired**, so everything below remains accurate as-built.

Every party a user interacts with — sender, receiver, the statechain entity (SE), the watchtower,
the Bitcoin indexer, the RGB proxy, operators — and, for each: what flows between them, what the
code **verifies** (with the file and the test that proves it), what is **trusted** and why, and
the residual boundaries that no protocol change can remove. Companion explainers:
[invalidation-deep-dive.md](learn/invalidation-deep-dive.md) (the timelock machinery, incl. §1b
*why a timeout at all*), [exits.md](learn/exits.md), [tokens.md](learn/tokens.md); normative
requirements in [SPEC.md](SPEC.md), [PROTOCOL.md](PROTOCOL.md) (the TES-R ladder),
[CHILDREN.md](CHILDREN.md) (first-class split children),
[INVALIDATION-SPEC.md](INVALIDATION-SPEC.md), [GRANULARITY-SPEC.md](GRANULARITY-SPEC.md);
adversarial findings in [AUDIT-2026-07.md](AUDIT-2026-07.md).

The one-line summary: **nothing here asks the user to trust a counterparty. The trust that
remains is confined to (a) one well-known statechain assumption about the SE, (b) the user's own
view of the Bitcoin chain, and (c) someone being awake before deadlines — and (c) is delegable
without custody.** Everything else is verified, and this page says where.

### One protocol, two coin shapes

There is exactly **one protocol**. `claim()` establishes a TES-R ladder — trigger `T` → extension
`X_m` → state `S`, relative CSV, un-broadcast — for every fresh confirmed ROOT coin,
unconditionally; the per-deposit protocol switch is gone and no test pins the old lane. Under that
one protocol a coin has one of two **shapes**, and both are current:

- **LADDERED** — every plain deposit. Exit is the pre-signed tier chain. An idle coin **never
  ages** (no absolute locktime, 0 vB of on-chain rent) and renewal is off-chain and unbounded
  (`sdk43`, `sdk40` PART 3).
- **UN-LADDERED** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation — terminal-freeze, PROTOCOL.md §5.10, `sdk52`), and a split sub-coin whose
  funding is un-broadcast has no on-chain outpoint to root a trigger. These keep the signed-once
  backup and transfer by backup-chain handover. **This lane is load-bearing for tokens**, not a
  legacy remnant.

Read §2 with that split in mind: R4/R5 are the backup-chain lane's anti-sender checks; the ladder's
equivalent is the **R′ census** (`verify_bundle` / `verify_child_bundle` in
`clients/libs/rust/src/tesr.rs`), which proves the handed-over state carries the strictly-lowest
CSV and that every superseded state was disclosed and provably out-raced.

---

## 1. The parties and what flows between them

```
                       co-sign requests (blind)              chain reads,
        ┌──────────────────────────────────────► SE          broadcasts
        │                                        ▲ │              ▲
        │              key handover,             │ │ terminal     │
        │              ladder / backup chain,    │ ▼ queries      │
   SENDER ────────────────────────────────► RECEIVER ────────► BITCOIN
        │              consignment (RGB),        │            (via indexer)
        │              terminal-ancestor ids     │                ▲
        │                                        │ watch bundle   │
        └── stale backups (the attack) ──►chain  └──────────► WATCHTOWER(s)
                                                              (keyless)
```

- **Sender → Receiver** (via the SE's message relay): the transfer message — key-handover
  material (`t1`) plus, by coin shape, either the **TES-R bundle** (the pre-signed tier chain; for
  a split child also its ancestor segment and every superseded state the census must count) or the
  full **backup-tx chain** with the exit **branch** and terminal-ancestor id list for an
  un-laddered sub-coin — and (tokens) the RGB consignment.
- **User ↔ SE**: blind MuSig2 co-signing (`sign_first`/`sign_second`), key-share rotation on
  transfer, spend-budget/terminal state, deposit init, the encrypted message relay.
- **User ↔ Bitcoin (via an electrum indexer)**: tip height, tx/outpoint lookups, history,
  broadcasts.
- **User → Watchtower**: a keyless bundle — the `TesrBundle` (tier chain) for a laddered coin, the
  `WatchBundle` (branch + latest backup) for an un-laddered sub-coin. Pre-signed exit material
  only, in both cases (see §5).
- **User ↔ RGB proxy**: consignment upload/download (token transfers only).
- **User ↔ operators** (optional): deposit-token server (onboarding), refresh sponsor (fee
  rebates), SSP (Lightning swaps).

---

## 2. The receiver: what is verified on receive (the heart of the model)

A receiver **verifies, and does not trust,** the sender — and verifies most of what the SE says.
On every claim (`clients/libs/rust/src/transfer_receiver.rs` — client-side, running on the
receiver's machine; a laddered claim additionally runs the R′ census in
`clients/libs/rust/src/tesr.rs` — `verify_bundle` for a whole coin, `verify_child_bundle` for a
split child):

| # | Check | Defeats | Code | Proven by |
|---|---|---|---|---|
| R1 | Sender's Schnorr signature binding the coin's outpoint to the receiver's new pubkey (`tx0_txid ‖ vout ‖ new_user_pubkey`) | handover messages not authorized by the coin's owner | `verify_transfer_signature` (`lib/src/transfer/receiver.rs:160-180`, checked for the index-0 backup group) | every successful claim (`sdk01` et al.); reject paths in `unit::transfer_signature_tests` (replay-to-other-receiver, wrong-outpoint, forged-by-non-owner) |
| R2 | The receiver's NEW share + new server share combine to the coin's on-chain aggregate pubkey (else `IncorrectAggregatedPublicKey`); sender-key/t1 supporting checks | SE or sender handing over key material that doesn't control the coin | `get_new_key_info` (`lib/src/transfer/receiver.rs:664-692`); `validate_tx0_output_pubkey`, `validate_t1pub` | every claim (`sdk01` et al.) |
| R3 | Funding tx0 output pays the expected aggregate (`validate_tx0_output_pubkey`); outpoint **unspent** (all coins). Confirmation is a hard reject only for **exit-branch roots** (unspent **and height > 0** with ≥ `confirmation_target` — the combine-review mempool-root fix); a plain coin's claim completes and is booked `UNCONFIRMED` until `coin_status` confirms it | fake or spent funding; a sub-coin branch rooted in an unconfirmed/mempool tx | `verify_tx0_output_is_unspent_and_confirmed` (`transfer_receiver.rs:534-538`); `validate_branch` root checks (`transfer_receiver.rs:854-870, 942-947`) | every claim; `sdk31` (combine root); the height-0 branch-root path has no dedicated test yet |
| R4 | **Un-laddered lane**: backup-chain locktime in `(tip, tip + initlock]` — rejects `LocktimeTooLow` (an already-raceable floored chain) and `LocktimeTooHigh` (one locked *above* a fresh deposit's, which would overstate your safe window while ancestors' real backups matured). **Laddered lane (R′ census)**: the handed-over state carries the **strictly-lowest CSV**, no hidden lower state exists, and every superseded state is disclosed and provably out-raced | being handed an **already-raceable** coin, a forged over-long chain, or a ladder with a retained lower-CSV state | `lib/src/transfer/receiver.rs:461-466`; `verify_bundle` / `verify_child_bundle` (`clients/libs/rust/src/tesr.rs`) | `sdk54`, `sdk46` (census: a malicious sender bypassing his own client's guard is still rejected by the receiver); `sdk58` (11 adversarial child-bundle cases REJECT — aggregates, hidden state, Model-A, parent terminality, child-superseded race, count padding, value spoof) |
| R5 | Signature count at the SE == backup-tx count; backup chain decrements exactly `interval` per hop. On the ladder the same arithmetic is the **census count**: each off-chain hop costs exactly **one** co-signature and discloses exactly **one** superseded state, which the receiver counts (`child_num_sigs == 0 + 2 + 1` at depth 1) | hidden intermediate owners; sender keeping extra co-signed states | `transfer_receiver.rs:529` (count); `ladder_decrements_by_interval` in `validate_signature_scheme` (`lib/src/transfer/receiver.rs`); census counts in `verify_child_bundle` | every claim; interval reject paths in `unit::transfer_signature_tests::ladder_interval_check_rejects_wrong_and_increasing` (wrong gap, equal, increasing — no underflow panic); count-padding rejected in `sdk58`; the per-hop census arithmetic across two hops in `sdk60` (+ `sdk17`, partial second hop); the backup-chain count-mismatch reject remains E2E-implicit |
| R6 | **Branch validation** (sub-coins): every branch tx consensus-valid and connected root→leaf; root input on-chain, unspent, confirmed; every branch locktime ≤ tip (INV-4); value conservation per hop (INV-25); **non-tree branches rejected** (an outpoint consumed twice) | fabricated or double-spending exit branches | `validate_branch`, `reject_non_tree_branch` | `sdk58`, `terminal_parents_tests` |
| R7 | **Terminal ancestors** (sub-coins): one named ancestor per structural input the branch consumes (Σ inputs, so an N-carrier combine names all N), each reporting `terminal: true` at the SE | sender double-spending a branch parent via a fresh SE co-signature | `required_terminal_ancestors`, `verify_terminal_parents` | `sdk58`, `sdk31` VERIFY log |
| R8 | **RGB consignment** fully client-validated; amount booked = what the consignment assigns to the receiver's outpoint, under the cryptographically-derived contract id | token forgery, wrong-asset or wrong-amount claims — no proxy or issuer is trusted for token *rules or amounts* (chain anchoring resolves through the wallet's indexer and inherits §4/B3) | `accept_incoming_tokens` (REQ-21/22) | `sdk02`, `rgb13` |
| R9 | **Received split child: the key handover COMPLETES.** The claim rotates the SE share and the auth key, and `A_child` is **invariant** across the rotation (proved by passing the un-broadcast `SP` as the funding tx to `get_new_key_info`), so every pre-signed child tier stays valid and the **sender is permanently locked out**. The child is then first-class: payable onward whole (`child_retransfer`) or split (`child_in_ladder_pay`, a depth-2 `ancestors` chain) | a sender re-spending or re-transferring a child he has already paid away; a "received payment" that is only exit-able | `transfer_receiver.rs:900-930`; `verify_child_bundle` (`tesr.rs`, child terminality deliberately NOT required — the handover, not a freeze, is what makes the census durable); the coordinator's pending-transfer lock (`server/src/database/transfer_sender.rs:53`) covers the census→completion gap | `sdk60` (alice→bob→carol, the funding outpoint unspent throughout), `sdk17` (partial second hop) |

*Parameter provenance:* the un-laddered lane's window parameters (`initlock`, `interval`) come from
the SE's `GET /info/config` at claim time, and the fee-sanity baseline comes from the indexer's
`estimate_fee` — they parameterize the anti-*sender* checks and are themselves covered by the
§3/§4 trust items, not independently verified. The **ladder's** CSV schedule is *not* SE-served: the
decrement/floor/`m_max` cadence is the canonical, per-network `TesrParams` compiled into the client
(`lib/src/tesr.rs`, PROTOCOL.md §5.2), so a hostile SE cannot widen or narrow a coin's race window by
lying in `info/config`. `sdk44` drives a whole lifecycle — establish → renew to the budget → roll
over → exit — off that schedule alone.

What the receiver **cannot** verify (the honest list):

- **R-a. Ancestor-id substitution (blind-SE caveat, SPEC §14 / D2) — un-laddered lane only.** The
  terminal-ancestor *ids* conveyed with an exit branch are not cryptographically bound to the
  branch's outpoints (the SE is blind and cannot attest to the mapping). The Σ-inputs count check
  defeats *omission* but not *substitution* by a sender who controls other terminal coins.
  Compensating control: the receiver holds the complete locktime-free branch and can materialize
  immediately — an on-chain settle removes the exposure entirely; the watchtower does it
  automatically near the deadline. **The ladder does not have this gap**: `verify_child_bundle`
  never trusts a supplied id. It derives `A_parent` from the *fetched on-chain* `F.spk` and requires
  the SE's recorded aggregate for the claimed parent sid to equal it (and `UNIQUE(aggregate_xonly)`
  means only the real parent can), then walks each intermediate segment deriving its aggregate from
  the funding output it actually spends. A substituted id fails on the key, not on a name
  (`tesr.rs`, checks [1]/[2]/[4b]; the decoy-parent and Model-A variants are among `sdk58`'s 11
  rejects).
- **R-b. SE share deletion** — see §3.

**The sender needs no trust in the receiver**: a transfer is a one-way handover; until the
receiver completes the claim the sender can effectively cancel by **re-sending the coin**
(overwriting the pending relay message — proven, including its impossibility *after* the claim,
by `tm01`; the SSP's latch-gated receive additionally has an explicit abort, `sdk24`), and after
the claim the coin is simply no longer the sender's. (Payment-for-goods atomicity is out of
protocol scope — for atomic counterparty swaps use the Lightning latch (`tb04`) or invoices.)

---

## 3. The SE: blind co-signer — what it can, cannot, and must be trusted for

The SE holds **one share of a 2-of-2** MuSig2 key per coin and co-signs blindly: it does not see
the transaction it signs — no amounts, outpoints, or destinations. Blindness covers *content*,
not *traffic*: the SE does learn statechain ids, per-coin auth pubkeys, deposit-token ids,
`single_use`/`epoch_deadline`/spend-budget flags, signature counts, transfer timing (relay
polling), and the caller's network endpoint — a timing/velocity graph of the system, and it can
correlate deposits to exits by amount at the chain boundary. It can also censor its message
relay (delaying claims; the coin stays the sender's until re-sent or exited). Physically the
"SE" is an API server + database + **enclave(s)** (lockbox/SGX) holding the key shares, brokered
per co-sign; there is **no client-facing attestation** today, so "the share lives in an enclave"
is an operational claim the user trusts, not verifies. Discovery note: SEs advertise their
URL/terms via signed nostr events (server `NostrInfo`); wallets take the SE URL from config —
verify it out-of-band, the relay is not a trust anchor.

**Cannot do alone** (verified/structural):
- Steal: it never holds a full key, and every spend needs the owner's share (`sdk01`+every E2E).
- Forge your exit material after the fact: your tiers (laddered) or backups (un-laddered) are
  already signed and in your hands.
- Un-terminate a node: `set_spend_budget` takes `min(existing, new)` server-side
  (`server/src/database/deposit.rs:181-199`; the pure predicate is unit-tested in
  `invalidation_model::terminal_predicate_matrix`); budget *exhaustion* refusal is E2E-proven by
  `sdk04` (an in-ladder split leaves the parent TERMINAL at the SE, and a second split over it is
  refused for that reason — the test pins the cause negatively so a plumbing error cannot make the
  refusal pass vacuously). Terminal state is publicly auditable
  (`GET /statechain/spend_budget/<id>`).
- Double-hand-over a coin **behind the receiver's back**: nonce-atomicity (one message per
  signing nonce — INV-23, `sdk12`) plus receiver-side count checks (R5). Note the honest
  counter-example: an SE *willing* to fresh-sign twice can create two conflicting states
  (`sdk15` documents this trust floor); what protects the receiver is R5 — the second "owner"
  cannot present a consistent ladder/count without the SE visibly double-signing.

**Trusted for (the irreducible statechain assumption):**
- **T-SE-1: deleting/overwriting the previous owner's key share at transfer.** If a malicious SE
  *keeps* old shares and **colludes with a previous owner**, together they can fresh-co-sign an
  immediate spend — no timelock protects against it, because a fresh signature needs no backup.
  This is THE statechain trust unit, identical in Mercury and (as full-operator collusion) in
  Spark. *Why it cannot be verified*: any proof of erasure attests one instance of the data, and
  nothing prevents a copy having been made before the proof — verifying a negative over an
  adversary's storage is impossible from outside. Mitigations, not proofs: the enclave narrows
  "the operator kept it" to "the enclave leaked it" (but see §3 head — no client-side
  attestation), blindness means the SE cannot identify *which* coin to steal without the
  colluding old owner, collusion leaves publicly-queryable evidence (terminal receipts,
  signature counts), and the current owner racing with a fee-bump wins in practice
  (deep-dive §5.3) — but it is a race, not a proof. *Why not split the SE across N operators*
  (Spark's honest-1-of-n deletion)? It shrinks B1 only if ≥1 operator is honest, at the price of
  N-way liveness for every co-sign (any operator down freezes all cooperative paths) and an
  N-fold blindness/collusion surface; this design keeps one blind SE and spends the complexity
  budget on receiver-side verification and SE-free exits instead.
- **T-SE-2: liveness** — refusal to co-sign freezes only the *cooperative* paths. Unilateral exit
  is pre-signed and SE-independent — the SDK walks the TES-R chain (trigger → extension → state) as
  each relative CSV matures with no SE call at all (`sdk50`; `sdk40` PART 1 drives the same chain at
  the library level, and `sdk45` shows a **keyless** third party can drive it), and an un-laddered
  sub-coin exits by its pre-signed branch + backup — so freeze ≠ seize; worst case your coin becomes an
  on-chain exit ticket with a bounded wait. One true boundary: the **onboarding window** — the
  first backup (tx1) is co-signed only when the SE first *sees* the funding tx
  (`coin_status.rs:75-79`), so the window runs from funding broadcast until that co-sign; an SE
  that dies inside it strands the funding in the 2-of-2. Fund only after deposit init succeeds,
  and treat a missing first backup/`DepositConfirmed` as a reason to stop funding further coins
  (deep-dive §5.2).

---

## 4. Bitcoin, seen through an indexer: the user's own eyes

Everything above assumes the user can *see the chain and reach it*. That view goes through an
electrum-protocol indexer (`electrum_url`), and it is a genuine trust point — **the same SPV-level
assumption as every light wallet**, made explicit:

- **Trusted for**: tip height (drives every locktime floor/deadline decision — R4, watchtower
  margins), outpoint spent/unspent status (R3, R6), confirmation counts, broadcast delivery, and
  **fee-rate estimation** (`estimate_fee` drives the CPFP bump rate and the sender-backup fee
  sanity baseline; `max_fee_rate` caps only the over-payment direction).
- **A lying indexer could**: under-report the tip so a stale backup looks immature (delaying your
  exit past a deadline), report a spent root as unspent (making a receiver accept a dead branch —
  R3/R6 pass on false data), hide a hostile mempool spend, under-report the fee rate so a
  deadline exit under-bids the mempool, or silently drop your broadcasts.
  It could not steal by itself — it can only blind and delay you; funds move only via signatures
  it doesn't have.
- **Fail-closed behaviors in code**: an exit-branch root that is only in the mempool is rejected
  (`unspent.height > 0` before the confirmations math, `transfer_receiver.rs:942-947` — the
  combine-review mempool-root fix); branch broadcast distinguishes a mempool *conflict* (raced
  exit → `ExitBranchConflict` event, hard error) from idempotent re-broadcast; token-wallet
  balance fails closed when RGB state is unavailable (audit [23]).
- **Finality (reorgs)**: receiver acceptance is final modulo a reorg deeper than
  `confirmation_target` (regtest default 2, `SdkConfig::mainnet` default 3) — a deeper reorg
  undoes the very facts R3/R6 checked. Raise the target for large values, and size watchtower
  margins with reorg slack (B6).
- **Transport**: the dev defaults are plaintext (`http://` SE, `tcp://` electrum, `rpc://` RGB
  proxy). In production every channel (SE, electrum, RGB proxy, SSP) must run TLS/`ssl://`: an
  on-path attacker over plaintext is strictly stronger than a lying indexer — it can tamper with
  unauthenticated responses (e.g. `info/config`'s `initlock`/`interval`/fee rate), observe all
  metadata, and censor. (Transfer messages themselves are end-to-end ECIES-encrypted to the
  receiver's auth key regardless.) A `tor_proxy` option exists for SE HTTP calls only — the
  electrum connection is direct; run your own node for network-level privacy.
- **The remedy is architectural, not protocol**: run your own node + indexer (the regtest stack
  ships one; mainnet deployments should point `electrum_url` at their own electrs), and/or run
  multiple independent watchtowers on *different* indexer connections (§5) so one lying indexer
  cannot blind them all. Broadcasts can additionally go through any out-of-band path (a public
  broadcaster, a second node) — the pre-signed txs are portable.

**Bitcoin itself** is trusted for liveness and fee markets: an exit must confirm before a
deadline, so sustained full-block congestion compresses your margin — the watchtower margin
(`margin_blocks`) and CPFP top-ups (deep-dive §5.4) absorb this; choose margins accordingly.

---

## 5. The watchtower: delegation without custody

*Can the user run their own watchtower? Multiple? Do we trust it?* — Yes, yes, and **no trust
needed for custody**, by construction:

- **In-process by default.** The wallet's own background task (`start_background`) claims
  incoming transfers and runs the `auto_exit_due` watchtower pass (`auto_exit` default-on, margin
  `auto_exit_margin_blocks` = 288 ≈ 2 days): force-exiting plain un-laddered sub-coins /
  **materializing token carriers** near their deadlines (REQ-33, `sdk34`). Routine *background*
  re-anchoring is **off** by default (`background_auto_refresh = false`): a laddered coin has no
  floor to approach — it never ages and pays 0 vB of rent — and on the un-laddered lane refresh is
  folded into `transfer` and paid on demand as part of the payment fee (B4 economics; the
  `auto_refresh` pre-spend hook stays default-on), so a running wallet never silently shrinks a
  balance. The ladder's own defence is `defend_ladders()` — one `watch_pass` per adopted bundle, a
  no-op until someone broadcasts the trigger, since an un-broadcast coin never ages. It is
  **not** wired into `start_background`: schedule it per block from your own loop, or delegate it
  (next bullet). This is not a third party — it's your own process; "trusting the watchtower" here
  means trusting your own machine to be on **while you hold off-chain coins** (a wallet that is
  entirely offline past deadlines is B4's case — delegate, or exit before going dark).
- **Keyless delegation** (`sdk45`): everything a watchtower must broadcast is *already
  fully signed* and *pays only the owner* — in **both** coin shapes.
  - *Laddered*: the persisted `TesrBundle` (`tesr::persist` / `tesr::load`) is the tier chain,
    every tier paying the owner's own key. `tesr::watch_pass(cc, bundle)` runs one iteration from
    that bundle and an electrum connection alone — no wallet, no coin, no SE, no keys. `sdk45`
    serializes the bundle a user would hand a third party and asserts it contains none of
    `mnemonic`/`seckey`/`secret`/`private`/`privkey`/`xpriv`, then has the keyless tower defend an
    offline owner against a **hostile trigger** end-to-end (both assertions back-filled there from
    the retired `sdk35`). `sdk51` is the same defence run by the owner's own pass.
  - *Un-laddered*: `export_watch_bundle()` emits branch txs, deadlines, and (plain coins only) the
    latest backup tx. **No key-material fields exist on the bundle types at all** (unit-tested,
    `bundle_roundtrip_and_carrier_has_no_backup`). `watch_pass(bundle, electrum, margin)` in
    `rust-sdk/src/watchtower.rs` is the matching keyless pass.

  Hand either bundle to any machine, cron it anywhere. Two scope notes: `export_watch_bundle`
  covers **off-chain sub-coins only** — an on-chain-funded coin has no ancestor race for it to
  watch — and bundles of both kinds are **snapshots**: re-export after any operation that mints or
  replaces coins (transfer, claim, split, child re-transfer, **and refresh** — treat
  `WalletEvent::CoinRefreshed` as a re-export trigger).
- **The worst a malicious/buggy watchtower can do**: broadcast *early* — which settles the
  owner's coins on-chain **to the owner** (safe; costs only the off-chain-ness; proven in
  `sdk34`: bob keeps his sats and all 250 tokens; on the ladder an early tower merely walks the
  owner's own tiers to the owner's key, `sdk45`) — or *not act* (the identical risk as
  running no watchtower). It cannot redirect funds (it has no keys) and it cannot destroy tokens
  (a carrier's bundle entry structurally contains **no backup tx**, only the materializing
  branch — and a carrier is never laddered in the first place, `sdk52`).
- **Multiple watchtowers compose safely**: they all hold the same pre-signed transactions, so
  they can never conflict — a second tower's broadcast is an idempotent re-broadcast
  (`sdk45`, two independent towers). Redundancy is pure upside; diversity of *indexer
  connections* also hedges §4.
- **What remains trusted: availability.** *Someone* — your process, your cron, a third party, or
  several of them — must be awake: inside the margin before an un-laddered coin's absolute
  deadline, and within the CSV window once someone triggers a laddered coin (there is no calendar
  date for that one; the requirement is reactive, which is why an idle laddered coin costs nothing
  to hold). That is the (c) in the summary; it is delegable and redundant but not removable (see
  §7-B4).
- Privacy note: a bundle reveals the watched coins' exit txs (amounts, addresses) to the
  watchtower — a privacy cost, never a custody one.

---

## 6. Token-specific parties (RGB)

- **RGB validity is client-validated** — the receiver's own wallet validates the full consignment
  history against the Bitcoin anchors (R8). No proxy, SE, or issuer is trusted for token *rules,
  amounts, or history*; the *anchoring* of that history to Bitcoin is resolved through the
  wallet's own electrum indexer and inherits the §4 trust point (B3). (`rgb12`/`rgb13` are the
  negative tests.)
- **The RGB proxy** relays consignments; it is trusted for *availability* only (a dead proxy
  delays token transfers; claims retry idempotently — audit [8]). It cannot forge (validation is
  local) and it cannot steal.
- **Issuers** control issuance policy, not your holdings: there is deliberately no freeze
  (client-validated assets have no enforcement point — [tokens.md](learn/tokens.md)).

### What each party learns (privacy, not custody)

| Party | Learns | Mitigation |
|---|---|---|
| SE | statechain ids, auth pubkeys, sig counts, flags, transfer timing, your IP — not amounts/outpoints/destinations (blind) | `tor_proxy` (SE HTTP only); amount-correlation at the chain boundary remains |
| Indexer | every address/outpoint/tx your wallet ever queries — full coin linkage, live interest, your IP | run your own node/electrs |
| RGB proxy | consignment contents in transit (asset, amounts, history) | self-host the proxy |
| SSP | swap amounts, invoices, coin ids involved in swaps | choose/run your own SSP |
| Watchtower | the watched coins' exit txs (amounts, addresses) | split coins across towers; self-host |

---

## 7. Optional operators, and the boundaries that cannot be solved

**Optional operators** (all custody-free):
- **Deposit-token server** — *no relation to RGB tokens*: a "deposit token" is an **onboarding
  voucher**, upstream Mercury Layer's anti-spam + operator-revenue gate. Every new statechain
  slot consumes one, but slots come in two classes (REQ-35, `sdk36`):
  - **Onboarding slots** — fresh on-chain value entering the SE (a deposit address, a token
    issuance carrier). These consume a normal token: free from the SE when no token server is
    configured (`server/src/endpoints/deposit.rs`, capped per audit [26]; on mainnet only with
    the explicit operator opt-in `free_tokens_on_mainnet = true`), priced by the token server
    otherwise. Rationale: a slot is a permanent SE liability (enclave share, DB, co-signing
    duty), and a *blind* SE has no other billing point — transfers are free and unmetered, so
    pay-once-per-slot is the statechain fee model.
  - **Derived slots** — outputs of SE-co-signed flows over an *existing* statechain (off-chain
    split pieces/change **including in-ladder split children**, `transfer_many` recipients,
    combine outputs, refresh re-anchors).
    These re-house value already inside the SE, adding no on-chain onboarding surface, so they
    are **free**: the SE mints derived tokens itself (`POST /deposit/get_derived_token`, any
    network, never routed to the token server), gated on the parent's CURRENT-owner auth (the
    audit-[15] single-use nonce) and a per-parent **lifetime** cap
    (`max_derived_tokens_per_statechain`, default 64; 0 disables derived issuance). Without
    this, a paid deployment would charge every 2-output split 2× the onboarding fee (shipped
    token-server default is 10,000 sats) and every (auto-)refresh 1×, for zero new on-chain
    surface. The SDK never spends pooled/prepaid onboarding tokens on a derived slot, and falls
    back to them only if the SE lacks or refuses the endpoint. *Residual (blind SE)*: the SE
    cannot see how a slot is later funded, so a dishonest owner can point a fresh L1 deposit at
    a derived slot and dodge the fee for that slot — bounded by the per-parent lifetime cap and
    the audit-[26] outstanding-token cap, eliminable only by unblinding deposits or disabling
    derived issuance (`max_derived_tokens_per_statechain = 0`).

  **Deployment strategy (decided)**: production runs FREE — onboarding unpriced, the audit-[26]
  outstanding-token cap as the standing spam brake — and token-server pricing is deferred until
  spam actually appears; the derived-slot exemption above is what makes enabling it economically
  sane later. With a token server configured, the wallet reaches it **through the SE** (never
  directly; honest relay of pricing is part of the §3 trust). Worst case = losing one prepaid
  onboarding fee; it never touches existing coins (`SdkError::TokenPaymentRequired` surfaces
  cost instead of silently paying).
- **Refresh sponsor** (`refresh_sponsored`): rebates the refresh fee off-chain *after* the
  re-anchor. A sponsor that stiffs you costs exactly `fee` sats (you keep the refreshed coin);
  the failure surfaces as an explicit error ("re-anchor succeeded but the sponsor rebate
  failed", `refresh.rs`; happy path `sdk30`, **stiffing bounded-loss `sdk38`** — a broke
  sponsor errors while the user keeps the refreshed amount−fee coin). *Migration defect, fixed*:
  the rebate used to be sized at the old `fee + DUST_LIMIT` floor (442 sat), but a sponsor paying
  from a laddered coin rebates via an **in-ladder split**, whose child must fund its own extension
  and state tier before clearing dust — `min_child_value`, 1306 sat at the default 2 sat/vB. The
  under-sized rebate failed with `FeeTooHigh` *after* the user had already paid the on-chain
  re-anchor fee. The sponsor now rebates `max(fee_sats + DUST_LIMIT, min_child_value)` and absorbs
  the difference, so the user still ends ≥ whole.
- **SSP** (Lightning): swaps work in **both directions on the ladder** via the HODL latch and are
  preimage-atomic — exact-amount pay/receive `sdk63`/`sdk64`, non-exact (latched in-ladder split)
  `sdk65`/`sdk67`, failure/rollback `sdk66`/`sdk68`, adversarial `sdk19`/`sdk20`/`sdk24`. The SSP
  is trusted for liveness and quotes, not funds. One structural note: a **latched piece is the one
  case that stays terminalized**. It is deliberately left unclaimed until a preimage lands, which
  is precisely the window the temporary pending-transfer lock does *not* cover (that lock expires
  with the batch window), so the SE is asked to co-sign nothing further over it — closing the
  post-expiry rival window permanently. Every other in-ladder payment relies on the pending lock
  plus the receiver's prompt handover (R9) instead, which is what keeps ordinary children
  re-transferable. *Migration defect, fixed*: the one-call pay API minted its input via
  `ensure_exact_coin` and therefore refused **every** laddered coin; arbitrary-amount invoices are
  now payable from a laddered coin through the latched in-ladder split (`sdk65`).

### The honest list: boundaries that remain (numbered, with their mitigations)

| # | Boundary | Why it cannot be removed | Mitigation (not proof) |
|---|---|---|---|
| B1 | **SE share deletion / SE+old-owner collusion** (T-SE-1) | A blind 2-of-2 co-signer's memory cannot be proven erased from outside | enclave (lockbox/SGX); blindness (target selection needs the colluder); public audit trail; the owner's race head start |
| B2 | **Ancestor-id substitution** (R-a, SPEC §14) — **un-laddered lane only**; on the ladder the ancestor chain is key-derived from the on-chain funding and each segment's own funding output, so there is no id to substitute | For the branch lane, binding ids to outpoints would require the SE to see (un-blind) the transactions | Σ-count check; immediate/automatic materialization closes the window; laddered coins are structurally exempt (`verify_child_bundle` [1]/[2]/[4b], `sdk58`) |
| B3 | **Indexer honesty & liveness** (§4) | A light client's chain view is whatever its indexer serves | own node; multiple towers on distinct indexers; out-of-band broadcast |
| B4 | **Deadline liveness** — someone must act inside the margin. On the **laddered** shape there is no calendar deadline at all (an un-broadcast coin never ages); the requirement is *reactive* — once someone broadcasts the trigger, a defender must race the tiers per block. On the **un-laddered** shape the deposit-anchored deadline is absolute | Timelock security is *defined* by acting before maturity (relative CSV on the ladder, absolute locktime on the backup chain) | the `auto_exit_due` watchtower pass runs from the background watcher by default (**while the wallet process is alive**), `defend_ladders()` is the ladder's per-block pass (schedule it yourself); keyless delegation to N towers covers offline periods **except during a fee spike** — **[D31]** a keyless tower can broadcast the pre-signed tiers at their committed fee but **cannot fee-bump them** (a CPFP child needs an input it does not hold and a signature it cannot make), so if the relay floor rises above a tier's committed rate the defence falls back to the OWNER being online, or to an operator running the optional funded-tower variant; margins sized per B6 (`margin ≥ k_max·interval + 144`; SDK default 288). Routine background re-anchoring is default-**off** — refresh is folded into `transfer` as an on-demand fee — so it is not part of this defence |
| B5 | **Onboarding window** (T-SE-2 tail) | The first backup is co-signed only when the SE first sees the funding tx, so the window (funding broadcast → tx1 co-sign) cannot be closed by ordering alone | fund only after deposit init succeeds; treat a missing first backup / `DepositConfirmed` as a stop-funding signal (deep-dive §5.2) |
| B6 | **Audit [17] residual** (**un-laddered lane only**): the deposit-anchored deadline is late by `k·interval` for parents transferred k times pre-split. A laddered coin has no absolute deadline to be late about — its clock only starts when a trigger is broadcast, and the census bounds who can win that race (R4/R′) | Ancestor locktimes aren't conveyed to descendants today | margins absorb it (`margin ≥ k_max·interval + reorg slack + reaction time`); documented in INVALIDATION-SPEC §6 |
| B7 | **Loss of local state** — `wallet.db`/bundle loss is loss of funds (mnemonic alone is NOT a backup). This now covers the ladder too: a laddered coin's tier chain lives in the same backup rows (`tesr-*` for a whole coin, `ctesr-*` for a split child) alongside the un-laddered lane's exit ladder / `branch-*` / `parents-*`, and `export_recovery_bundle` snapshots all of them. Token wallets additionally require the entire `rgb_data_dir` (its own **plaintext** `rgb.mnemonic` seed + the RGB stash), which the recovery bundle deliberately does NOT embed | The SE is blind and cannot re-serve per-coin exit material; that's the privacy design | `export_recovery_bundle` after every operation (incl. child re-transfers and refreshes) + copy `rgb_data_dir`; watch bundles as partial redundancy; device-at-rest security for the plaintext RGB seed |
| B8 | **Payment atomicity** — a plain transfer is a gift, not an escrow | In-protocol delivery-vs-payment needs a shared arbiter | Lightning latch / invoices for atomic swaps; ordinary commerce risk otherwise |
| B9 | **Single live instance per wallet** — the wallet-record lock is in-process only; two processes/devices on one `wallet.db` (or a restored bundle beside a live original) can broadcast stale state against each other and corrupt the DB | No cross-process/cross-device coordination exists (and the blind SE cannot arbitrate) | one live instance per wallet; bundle restore is disaster recovery, not device sync |
| B10 | **Split/combine commit ordering** (external review finding 3) — a split/combine sets the parent's spend budget to terminal (`set_spend_budget … 1`) BEFORE the child signatures + backups are durably persisted, so a crash or backend fault in that window leaves the parent terminal while the child state is not fully recoverable, forcing a unilateral exit of the parent's *value* (no funds lost — the BTC exits via the latest backup — but the cooperative off-chain path for that operation is gone) | The budget MUST precede the co-signature: it is the SE-side monotonic guard (`database/deposit.rs`) that, with the MuSig2 one-shot secnonce consume, prevents a second conflicting co-signature of a terminal node (INV-19 fork). No reordering is safe. **The reviewer's proposed "persist the nonces and re-call sign/second on restart" fix is unsound**: the enclave atomically nulls the secnonce on the *first* partial signature (`lockbox/src/server.cpp`), so a replayed `sign/second` returns an error, never the original server partial sig — persisting nonces buys nothing once signing has finalized | unilateral exit recovers the BTC value; a future durable prepare/commit (persist the *assembled signed tx* before terminalizing, replay locally) would restore the cooperative path — deliberately not shipped as a half-mechanism (a persist with no tested recovery reader is false comfort) |

*Two neighbours of B10, both fixed rather than residual.* Same failure family — terminalize first,
fail second — and worth naming because they show where the ordering hazard actually bites:

- **In-ladder split admission (D1).** The guard used the old backup-fee floor (442 sat), but an
  in-ladder child funds its **own** extension and state tier before clearing dust
  (`min_child_value`, 1306 sat at 2 sat/vB). `establish_child` runs *after* the parent's spend
  budget is consumed and `SP` is co-signed, so admitting a child below that floor terminalized the
  parent and *then* failed with `FeeTooHigh`, stranding the parent as exit-only. The guard now
  takes `max(min_split_output, min_child_value)` and refuses **up front**, keeping the parent fully
  spendable (`transfer.rs`, `in_ladder_pay`). No funds were ever at risk; the cooperative path was.
- **Child status after a unilateral exit (D4).** A received child cannot be *cooperatively*
  withdrawn (its funding `SP.out[j]` is un-broadcast, so there is no confirmed outpoint to spend),
  so `withdraw` routes it to the unilateral exit and marks it `WITHDRAWING`. Such a coin has
  neither a withdrawal tx nor a withdrawal address — its progress is the pre-signed chain, not one
  txid — and treating that as an error made **every** subsequent status poll fail for the life of
  the coin (`coin_status.rs:154-164`).

Everything not in this table is **verified** — by the receiver's checks (§2), the SE's public
state (§3), client-side RGB validation (§6), or an E2E named above. If you find a trust
relationship not listed on this page, that is a documentation bug: please open an issue.

---

## 8. Test traceability (trust claims → proof)

| Claim | Test |
|---|---|
| Receiver rejects a raceable handover even from a guard-bypassing malicious sender | `sdk54`, `sdk46` (R′ census: no hidden lower-CSV state; the current state is strictly lowest) |
| Watch bundle is keyless (no key material); keyless tower defends an offline owner; two independent towers idempotent | `sdk45` (all three — the key-material and second-tower assertions were back-filled there from the retired `sdk35`), `sdk51` (the owner's own pass, same hostile trigger); unit `bundle_roundtrip_and_carrier_has_no_backup` for the un-laddered bundle type |
| Malicious sender's matured stale state fails after materialization; the honest owner's lowest-CSV state wins the race | `sdk51`, `sdk40` (PART 2), `sdk32` (C), `sdk34` (E) |
| Token carrier auto-materialized before clawback deadline; issued carrier untouched | `sdk34` |
| An RGB carrier is NEVER laddered (terminal-freeze) while a plain coin in the same wallet is; both shapes coexist in one wallet | `sdk52` |
| A laddered coin never ages: unbounded **off-chain** ladder renewal, zero on-chain bytes | `sdk43`, `sdk40` (PART 3) |
| Received split child is FIRST-CLASS: handover completes (sender locked out, `A_child` invariant), then paid onward off-chain — whole and split | `sdk60` (alice→bob→carol, funding outpoint unspent throughout), `sdk17` (partial second hop) |
| In-ladder split child bundle: valid child ACCEPTED, 11 adversarial variants REJECTED (aggregates, hidden state, Model-A, parent terminality, child-superseded race, count padding, value spoof) | `sdk58` |
| Terminal ancestors: per-input requirement + non-tree rejection | `sdk58`, `sdk31`, unit `terminal_parents_tests` |
| SSP pre-payment value gate reads TRUE coin value ([3] SATS branch-validated peek, [4] RGB consignment-derived amount) | `sdk37`; `sdk20` (SATS gate through real `execute_pay` + RLN) |
| Transfer-signature (R1) + backup-chain interval (R5) reject paths | `unit::transfer_signature_tests` |
| Ladder CSV cadence is client-canonical, not SE-served — a whole lifecycle (establish → renew to the budget → roll over → exit) driven off `TesrParams` alone | `sdk44` |
| Sponsored-refresh bounded loss (stiffing sponsor → user keeps refreshed coin) | `sdk38` |
| Depth-2 colored sub-coin exits on-chain, allocation preserved | `sdk39` |
| Mempool-root (height 0) rejection for branch roots | code `transfer_receiver.rs:942-947`; **RESIDUAL — no E2E**: an honest SDK flow cannot produce a 0-conf branch root (splitting needs a confirmed parent); reaching it requires a forged transfer message, so only the confirmed-root positive (`sdk31`) is exercised |
| SSP RGB pay through Lightning ([4] gate wiring end-to-end) | **RESIDUAL — no E2E**: needs the (unimplemented) cross-rail RGB latch bridge; the gate's validation logic is proven by `sdk37` and its SATS wiring by `sdk20` |
| SE nonce atomicity (no double-sign behind the receiver's back) | `sdk12` |
| Fresh double-sign trust floor — an SE *willing* to double-sign creates a race (the honest counter-example) | `sdk15` |
| SE refusal ≠ seizure (unilateral exit without SE) | `sdk50` (SDK walks the whole TES-R chain, no SE call), `sdk40` (PART 1, library level), `sdk45` (a KEYLESS third party can drive the same exit) |
| A terminal node is not re-spendable: an in-ladder split leaves the parent TERMINAL at the SE and a second split over it is refused (cause pinned negatively, so plumbing errors cannot pass vacuously) | `sdk04` |
| Stale-state clawback defeated by the CSV ladder + watchtower | `sdk51`, `sdk40` (PART 2), `sdk45` |
| Token state client-validated (forged/invalid consignments rejected) | `rgb12`, `rgb13` |
| SSP swap atomicity + adversarial refusals, both directions on the ladder | exact `sdk63`/`sdk64`, non-exact (latched in-ladder split) `sdk65`/`sdk67`, failure/rollback `sdk66`/`sdk68`, adversarial `sdk19`/`sdk20`/`sdk24` |
| An LN pay failure never strands the coin (exact lane restores it as exitable; non-exact rolls back) | `sdk68` (exact), `sdk66` (non-exact) |
| Sender pre-claim cancel = overwrite-by-resend (impossible after claim); SSP latch abort; receiver-failure paths | `tm01`, `sdk24`, `sdk19` |

**Retired evidence — read this before re-citing an old test id.** These E2Es were deleted during the
one-protocol migration. Where the claim survives, its evidence is listed above; where the claim
itself no longer exists under TES-R, say so rather than repointing:

| Retired | Status |
|---|---|
| `sdk03`, `sdk05`, `sdk06` (LN core) | superseded by `sdk63`/`sdk64`/`sdk67` (+ `sdk65` non-exact, `sdk66`/`sdk68` failure) |
| `sdk07`, `sdk08`, `sdk10` (exit / terminal) | superseded by `sdk50` (unilateral exit) and `sdk58`/`sdk04` (terminality) |
| `sdk13`, `sdk14` (stale state / race) | superseded by `sdk51`, `sdk40` PART 2, `sdk45` |
| `sdk18` (LN pay-failure reclaim) | superseded by `sdk68` (exact) + `sdk66` (non-exact rollback) |
| `sdk26`, `sdk27` (invalidation) | **claim obsolete** — idle laddered coins never age; the ladder plus terminality subsume the invalidation timer. Not repointed |
| `sdk28` (sats granularity) | **claim obsolete** — the in-ladder split has its own fee model (`tier_out_total` / `committed_fee_for_outputs` / `min_child_value`) |
| `sdk33` (auto-refresh) | **claim obsolete** — there is no ladder floor to approach (0 vB rent); unbounded off-chain renewal is `sdk43` |
| `sdk35` (trust boundaries) | claims back-filled: `sdk45` (bundle carries NO key material; a 2nd independent tower is idempotent), `sdk52` (carrier never laddered), `sdk51`, `sdk46`/`sdk47`/`sdk54` (R′ census) |
