# Trust model — who trusts whom, what is verified instead, and what cannot be solved

Every party a user interacts with — sender, receiver, the statechain entity (SE), the watchtower,
the Bitcoin indexer, the RGB proxy, operators — and, for each: what flows between them, what the
code **verifies** (with the file and the test that proves it), what is **trusted** and why, and
the residual boundaries that no protocol change can remove. Companion explainers:
[invalidation-deep-dive.md](learn/invalidation-deep-dive.md) (the timelock machinery, incl. §1b
*why a timeout at all*), [exits.md](learn/exits.md), [tokens.md](learn/tokens.md); normative
requirements in [SPEC.md](SPEC.md), [INVALIDATION-SPEC.md](INVALIDATION-SPEC.md),
[GRANULARITY-SPEC.md](GRANULARITY-SPEC.md); adversarial findings in
[AUDIT-2026-07.md](AUDIT-2026-07.md).

The one-line summary: **nothing here asks the user to trust a counterparty. The trust that
remains is confined to (a) one well-known statechain assumption about the SE, (b) the user's own
view of the Bitcoin chain, and (c) someone being awake before deadlines — and (c) is delegable
without custody.** Everything else is verified, and this page says where.

---

## 1. The parties and what flows between them

```
                       co-sign requests (blind)              chain reads,
        ┌──────────────────────────────────────► SE          broadcasts
        │                                        ▲ │              ▲
        │              key handover,             │ │ terminal     │
        │              backups, branch,          │ ▼ queries      │
   SENDER ────────────────────────────────► RECEIVER ────────► BITCOIN
        │              consignment (RGB),        │            (via indexer)
        │              terminal-ancestor ids     │                ▲
        │                                        │ watch bundle   │
        └── stale backups (the attack) ──►chain  └──────────► WATCHTOWER(s)
                                                              (keyless)
```

- **Sender → Receiver** (via the SE's message relay): the transfer message — key-handover
  material (`t1`), the full backup-tx ladder, the exit **branch** for sub-coins, the
  terminal-ancestor id list, and (tokens) the RGB consignment.
- **User ↔ SE**: blind MuSig2 co-signing (`sign_first`/`sign_second`), key-share rotation on
  transfer, spend-budget/terminal state, deposit init, the encrypted message relay.
- **User ↔ Bitcoin (via an electrum indexer)**: tip height, tx/outpoint lookups, history,
  broadcasts.
- **User → Watchtower**: a `WatchBundle` — pre-signed exit material only (see §5).
- **User ↔ RGB proxy**: consignment upload/download (token transfers only).
- **User ↔ operators** (optional): deposit-token server (onboarding), refresh sponsor (fee
  rebates), SSP (Lightning swaps).

---

## 2. The receiver: what is verified on receive (the heart of the model)

A receiver **verifies, and does not trust,** the sender — and verifies most of what the SE says.
On every claim (`clients/libs/rust/src/transfer_receiver.rs` — client-side, running on the
receiver's machine):

| # | Check | Defeats | Code | Proven by |
|---|---|---|---|---|
| R1 | Sender's Schnorr signature binding the coin's outpoint to the receiver's new pubkey (`tx0_txid ‖ vout ‖ new_user_pubkey`) | handover messages not authorized by the coin's owner | `verify_transfer_signature` (`lib/src/transfer/receiver.rs:160-180`, checked for the index-0 backup group) | every successful claim (`sdk01` et al.); no dedicated tamper-negative yet |
| R2 | The receiver's NEW share + new server share combine to the coin's on-chain aggregate pubkey (else `IncorrectAggregatedPublicKey`); sender-key/t1 supporting checks | SE or sender handing over key material that doesn't control the coin | `get_new_key_info` (`lib/src/transfer/receiver.rs:664-692`); `validate_tx0_output_pubkey`, `validate_t1pub` | every claim (`sdk01` et al.) |
| R3 | Funding tx0 output pays the expected aggregate (`validate_tx0_output_pubkey`); outpoint **unspent** (all coins). Confirmation is a hard reject only for **exit-branch roots** (unspent **and height > 0** with ≥ `confirmation_target` — the combine-review mempool-root fix); a plain coin's claim completes and is booked `UNCONFIRMED` until `coin_status` confirms it | fake or spent funding; a sub-coin branch rooted in an unconfirmed/mempool tx | `verify_tx0_output_is_unspent_and_confirmed` (`transfer_receiver.rs:534-538`); `validate_branch` root checks (`transfer_receiver.rs:854-870, 942-947`) | every claim; `sdk31` (combine root); the height-0 branch-root path has no dedicated test yet |
| R4 | Backup-ladder locktime in `(tip, tip + initlock]` — rejects `LocktimeTooLow` (an already-raceable floored ladder) and `LocktimeTooHigh` (a ladder locked *above* a fresh deposit's, which would overstate your safe window while ancestors' real backups matured) | being handed an **already-raceable** coin, or a forged over-long ladder | `lib/src/transfer/receiver.rs:461-466` | `SDK_E2E=35` (D2: malicious sender bypasses his own client's guard; receiver still rejects) |
| R5 | Signature count at the SE == backup-tx count; ladder decrements exactly `interval` per hop | hidden intermediate owners; sender keeping extra co-signed states | `transfer_receiver.rs:529` (count); `validate_signature_scheme` (`lib/src/transfer/receiver.rs:330-338`) | every claim; no dedicated wrong-count/wrong-interval negative yet |
| R6 | **Branch validation** (sub-coins): every branch tx consensus-valid and connected root→leaf; root input on-chain, unspent, confirmed; every branch locktime ≤ tip (INV-4); value conservation per hop (INV-25); **non-tree branches rejected** (an outpoint consumed twice) | fabricated or double-spending exit branches | `validate_branch`, `reject_non_tree_branch` | `sdk10`, `terminal_parents_tests` |
| R7 | **Terminal ancestors** (sub-coins): one named ancestor per structural input the branch consumes (Σ inputs, so an N-carrier combine names all N), each reporting `terminal: true` at the SE | sender double-spending a branch parent via a fresh SE co-signature | `required_terminal_ancestors`, `verify_terminal_parents` | `sdk10`, `sdk31` VERIFY log |
| R8 | **RGB consignment** fully client-validated; amount booked = what the consignment assigns to the receiver's outpoint, under the cryptographically-derived contract id | token forgery, wrong-asset or wrong-amount claims — no proxy or issuer is trusted for token *rules or amounts* (chain anchoring resolves through the wallet's indexer and inherits §4/B3) | `accept_incoming_tokens` (REQ-21/22) | `sdk02`, `rgb13` |

*Parameter provenance:* R4/R5's window parameters (`initlock`, `interval`) come from the SE's
`GET /info/config` at claim time, and the fee-sanity baseline comes from the indexer's
`estimate_fee` — they parameterize the anti-*sender* checks and are themselves covered by the
§3/§4 trust items, not independently verified.

What the receiver **cannot** verify (the honest list):

- **R-a. Ancestor-id substitution (blind-SE caveat, SPEC §14 / D2).** The terminal-ancestor *ids*
  are not cryptographically bound to the branch's outpoints (the SE is blind and cannot attest to
  the mapping). The Σ-inputs count check defeats *omission* but not *substitution* by a sender who
  controls other terminal coins. Compensating control: the receiver holds the complete
  locktime-free branch and can materialize immediately — an on-chain settle removes the exposure
  entirely; the watchtower does it automatically near the deadline.
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
- Forge your ladder after the fact: your backups are already signed and in your hands.
- Un-terminate a node: `set_spend_budget` takes `min(existing, new)` server-side
  (`server/src/database/deposit.rs:181-199`; the pure predicate is unit-tested in
  `invalidation_model::terminal_predicate_matrix`); budget *exhaustion* refusal is E2E-proven by
  `sdk08`. Terminal state is publicly auditable (`GET /statechain/spend_budget/<id>`).
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
  is pre-signed and SE-independent (`sdk07`), so freeze ≠ seize; worst case your coin becomes an
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
  incoming transfers, **auto-refreshes** aging coins (REQ-32, `SDK_E2E=33`, `auto_refresh`
  default-on), and runs the `auto_exit_due` watchtower pass (`auto_exit` default-on, margin
  `auto_exit_margin_blocks` = 288 ≈ 2 days): force-exiting plain sub-coins / **materializing
  token carriers** near their deadlines (REQ-33, `SDK_E2E=34`). This is not a third party — it's
  your own process; "trusting the watchtower" here means trusting your own machine to be on
  **while you hold off-chain coins** (a wallet that is entirely offline past deadlines is B4's
  case — delegate, or exit before going dark).
- **Keyless delegation** (`SDK_E2E=35`): everything a watchtower must broadcast is *already
  fully signed* and *pays only the owner*. `export_watch_bundle()` emits exactly that — branch
  txs, deadlines, and (plain coins only) the latest backup tx. **No key-material fields exist on
  the bundle types at all** (unit-tested), and the E2E asserts the wallet's mnemonic and
  key-material keywords are absent from the JSON. `watch_pass(bundle, electrum, margin)` runs a
  watch iteration from the bundle and an electrum connection alone — no wallet, no database, no
  SE, no keys (`rust-sdk/src/watchtower.rs`). Hand the bundle to any machine, cron it anywhere.
  Two scope notes: the bundle covers **off-chain sub-coins only** — a flat coin has no ancestor
  race to watch; its aging is auto-refresh's job (which needs the wallet + SE) — and bundles are
  **snapshots**: re-export after any operation that mints or replaces coins (transfer, claim,
  split, **and refresh, including background auto-refreshes** — treat `WalletEvent::CoinRefreshed`
  as a re-export trigger).
- **The worst a malicious/buggy watchtower can do**: broadcast *early* — which settles the
  owner's coins on-chain **to the owner** (safe; costs only the off-chain-ness; proven in
  `SDK_E2E=35(B)`: bob keeps his sats and all 250 tokens) — or *not act* (the identical risk as
  running no watchtower). It cannot redirect funds (it has no keys) and it cannot destroy tokens
  (a carrier's bundle entry structurally contains **no backup tx**, only the materializing
  branch).
- **Multiple watchtowers compose safely**: they all hold the same pre-signed transactions, so
  they can never conflict — a second tower's broadcast is an idempotent re-broadcast
  (`SDK_E2E=35(B)`, two independent towers). Redundancy is pure upside; diversity of *indexer
  connections* also hedges §4.
- **What remains trusted: availability.** *Someone* — your process, your cron, a third party, or
  several of them — must be awake inside the margin before a deadline. That is the (c) in the
  summary; it is delegable and redundant but not removable (see §7-B4).
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
- **Deposit-token server**: sells onboarding slots — reached **through the SE** (the wallet never
  contacts it directly; the SE relays pricing/payment details, so honest relay is part of the §3
  trust). Worst case = losing one prepaid onboarding fee; it never touches existing coins
  (`SdkError::TokenPaymentRequired` surfaces cost).
- **Refresh sponsor** (`refresh_sponsored`): rebates the refresh fee off-chain *after* the
  re-anchor. A sponsor that stiffs you costs exactly `fee` sats (you keep the refreshed coin);
  the failure surfaces as an explicit error ("re-anchor succeeded but the sponsor rebate
  failed", `refresh.rs:98-101`; `sdk30` covers the happy path — no stiffing E2E yet).
- **SSP** (Lightning): swaps are preimage-atomic (`sdk03/05/06`, adversarial `sdk18-20`) — the
  SSP is trusted for liveness and quotes, not funds.

### The honest list: boundaries that remain (numbered, with their mitigations)

| # | Boundary | Why it cannot be removed | Mitigation (not proof) |
|---|---|---|---|
| B1 | **SE share deletion / SE+old-owner collusion** (T-SE-1) | A blind 2-of-2 co-signer's memory cannot be proven erased from outside | enclave (lockbox/SGX); blindness (target selection needs the colluder); public audit trail; the owner's race head start |
| B2 | **Ancestor-id substitution** (R-a, SPEC §14) | Binding ids to outpoints would require the SE to see (un-blind) the transactions | Σ-count check; immediate/automatic materialization closes the window |
| B3 | **Indexer honesty & liveness** (§4) | A light client's chain view is whatever its indexer serves | own node; multiple towers on distinct indexers; out-of-band broadcast |
| B4 | **Deadline liveness** — someone must act inside the margin | Timelock security is *defined* by acting before maturity | auto-refresh + the `auto_exit_due` watchtower run from the background watcher by default (**while the wallet process is alive**); keyless delegation to N towers covers offline periods; margins sized per B6 (`margin ≥ k_max·interval + 144`; SDK default 288) |
| B5 | **Onboarding window** (T-SE-2 tail) | The first backup is co-signed only when the SE first sees the funding tx, so the window (funding broadcast → tx1 co-sign) cannot be closed by ordering alone | fund only after deposit init succeeds; treat a missing first backup / `DepositConfirmed` as a stop-funding signal (deep-dive §5.2) |
| B6 | **Audit [17] residual**: the deposit-anchored deadline is late by `k·interval` for parents transferred k times pre-split | Ancestor locktimes aren't conveyed to descendants today | margins absorb it (`margin ≥ k_max·interval + reorg slack + reaction time`); documented in INVALIDATION-SPEC §6 |
| B7 | **Loss of local state** — `wallet.db`/bundle loss is loss of funds (mnemonic alone is NOT a backup); token wallets additionally require the entire `rgb_data_dir` (its own **plaintext** `rgb.mnemonic` seed + the RGB stash), which the recovery bundle deliberately does NOT embed | The SE is blind and cannot re-serve per-coin exit material; that's the privacy design | `export_recovery_bundle` after every operation (incl. refreshes) + copy `rgb_data_dir`; watch bundles as partial redundancy; device-at-rest security for the plaintext RGB seed |
| B8 | **Payment atomicity** — a plain transfer is a gift, not an escrow | In-protocol delivery-vs-payment needs a shared arbiter | Lightning latch / invoices for atomic swaps; ordinary commerce risk otherwise |
| B9 | **Single live instance per wallet** — the wallet-record lock is in-process only; two processes/devices on one `wallet.db` (or a restored bundle beside a live original) can broadcast stale state against each other and corrupt the DB | No cross-process/cross-device coordination exists (and the blind SE cannot arbitrate) | one live instance per wallet; bundle restore is disaster recovery, not device sync |

Everything not in this table is **verified** — by the receiver's checks (§2), the SE's public
state (§3), client-side RGB validation (§6), or an E2E named above. If you find a trust
relationship not listed on this page, that is a documentation bug: please open an issue.

---

## 8. Test traceability (trust claims → proof)

| Claim | Test |
|---|---|
| Receiver rejects floored/raceable handover even from a guard-bypassing malicious sender | `SDK_E2E=35` (D1 honest guard, D2 malicious bypass → `LocktimeTooLow`) |
| Watch bundle is keyless; keyless tower protects sats + tokens; two towers idempotent; early broadcast safe | `SDK_E2E=35` (A)(B) |
| Malicious sender's matured stale backups all fail after materialization | `SDK_E2E=35` (C), `sdk13`, `sdk34` (E) |
| Token carrier auto-materialized before clawback deadline; issued carrier untouched | `SDK_E2E=34` |
| Auto-refresh keeps coins off the floor invisibly; opt-out respected | `SDK_E2E=33` |
| Terminal ancestors: per-input requirement + non-tree rejection | `sdk10`, `sdk31`, unit `terminal_parents_tests` |
| Mempool-root (height 0) rejection for branch roots | code `transfer_receiver.rs:942-947`; no dedicated test yet |
| SE nonce atomicity (no double-sign behind the receiver's back) | `sdk12` |
| Fresh double-sign trust floor — an SE *willing* to double-sign creates a race (the honest counter-example) | `sdk15` |
| SE refusal ≠ seizure (unilateral exit without SE) | `sdk07`, `sdk08` |
| Stale-state clawback defeated by ladder + watcher | `sdk13`, `sdk14` |
| Token state client-validated (forged/invalid consignments rejected) | `rgb12`, `rgb13` |
| SSP swap atomicity + adversarial refusals | `sdk03/05/06`, `sdk18/19/20` |
| Sender pre-claim cancel = overwrite-by-resend (impossible after claim); SSP latch abort; receiver-failure paths | `tm01`, `sdk24`, `sdk19` |
