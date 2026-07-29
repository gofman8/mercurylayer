# Utexo V2 migration — remove V1, make TES-R the only path

**Status (2026-07-29): EXECUTED — the migration is DONE. This file is now the historical record
of the plan and the reasoning behind it; it is no longer a to-do list.** V2DEF-2 … V2DEF-6 all
ran on the live SE + Core-30 bitcoind matrix. The end state is in "Outcome — as executed" below;
each stage's reasoning is kept verbatim underneath with **SUPERSEDED / OUTCOME** notes where
execution diverged from the plan, and every test id named in a stage has been repointed to the
suite that is actually live today.

This spec folds in the adversarial review of the two fund-safety pieces (receiver adoption
and RGB/Lightning). Two FATAL findings forced a model change and are resolved here; the
accepted/rejected findings are cited inline.

---

## Outcome — as executed (2026-07-29)

Read this before any stage below; where the two disagree, this section is current.

**1. There is ONE protocol.** `deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT` env are
**deleted**. `claim()` establishes a TES-R ladder for every fresh confirmed ROOT coin,
unconditionally. Zero tests pin the old lane. The staged flag designed in V2DEF-2 ("the mode gate")
and V2DEF-5 ("compat knob") did its job — a per-suite double-run through the flip — and was then
removed; there is no longer a knob that can opt a deposit out of the ladder.

**2. ONE protocol, TWO coin SHAPES — both current.** The un-laddered path is **not** "V1" and not
legacy:

- **LADDERED** — every plain deposit: trigger `T` → extension `X_m` → state `S`, relative CSV, all
  un-broadcast.
- **UN-LADDERED** — an **RGB carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation — terminal-freeze, PROTOCOL.md §5.10; proven by **sdk52**), and a **split
  sub-coin whose funding is un-broadcast cannot root a trigger** [B0]. These coins keep the
  signed-once backup and transfer by backup-chain handover.

That lane is **load-bearing for RGB tokens**, so the V2DEF-6 excisions aimed at it did **not** run:
`validate_signature_scheme`, `create_tx1`, and the non-bundle receiver branch are all still in the
tree and still exercised. `TransferMsg.protocol_version` survived as promised, but its meaning
changed: it is now a **conveyance-shape discriminant**, not a protocol selector — `0` = un-laddered
backup-chain handover, `2` = TES-R bundle, `3` = legacy child conveyance without handover, `4` =
child conveyance **with** the standard key handover.

**3. Model A shipped exactly as specified** (sender pre-signs the receiver-paying state; the
receiver verifies, never trusts). Proven end-to-end by **sdk49** — Bob adopts and then unilaterally
exits the conveyed chain to his own key.

**4. Received split children are FIRST-CLASS** (`docs/utexo/CHILDREN.md`, which builds
directly on the multi-hop census requirement stated below in V2DEF-3). The claim completes the
**standard SE key handover**, so the receiver co-owns `A_child` — invariant across the rotation,
which is exactly what keeps the pre-signed exit chain valid — and the sender is permanently locked
out. A child is paid onward off-chain **whole** (`child_retransfer`) or **split**
(`child_in_ladder_pay`, a depth-2 `ancestors` chain). Each hop costs exactly one co-signature and
discloses exactly one superseded state, which the receiver's census counts and proves out-raced.
**sdk60** (alice → bob → carol with the funding outpoint unspent throughout) and **sdk17** (partial
second hop) are the proofs. The exit-only child conveyance was a shipping simplification and is gone.

**5. The in-ladder split closed B1.** A split is a STATE tier `SP` spending `X_m.out[0]` — a
descendant of `T`, not a rival for `F` — so a retained trigger has nothing to race. **sdk58** (11
adversarial cases, all REJECT) + **sdk59** (end-to-end split payment).

**6. Lightning works both directions on the ladder via the HODL latch** (`docs/utexo/LIGHTNING.md`,
which supersedes the adaptor-signature detour and V2DEF-4's latch sketch): **sdk63** (V2 pay),
**sdk64** (V2 receive), **sdk67** (in-ladder receive), plus **sdk65** (non-exact pay) and the failure
paths **sdk66** / **sdk68**. The LN-latched piece is the **one** case that stays terminalized — it
sits unclaimed past the pending-transfer lock's window.

**7. On-chain refresh survived.** V2DEF-6's "delete `refresh.rs` entire file" did **not** run: the
`refresh_sponsored` product decision was resolved as **KEEP**, and `refresh` / `refresh_sponsored` /
`reanchor` / `auto_refresh_due` plus the `TransferQuote` renewal-fee fields are all still shipping.
**sdk30** and **sdk38** are live.

### Deleted test ids → where their claims live now

Nothing below lost coverage silently; a claim that genuinely became meaningless is marked
*obsolete* with the reason.

| Deleted | Its claim is now proven by |
|---|---|
| sdk03, sdk05, sdk06 (LN core) | **sdk63** (V2 pay), **sdk64** (V2 receive), **sdk67** (in-ladder receive); **sdk65** non-exact; **sdk66/sdk68** failure paths |
| sdk07, sdk08, sdk10 (exit / terminal) | **sdk50** (SDK-level unilateral exit walks the tier chain), **sdk58** (11 adversarial exit/terminal cases) |
| sdk13, sdk14 (stale state / watcher race) | **sdk51** (watchtower defends a hostile trigger), **sdk40 PART 2** (a stale state dies at consensus), **sdk45** (keyless tower) |
| sdk18 (LN pay-failure reclaim) | **sdk68** (exact) + **sdk66** (non-exact rollback) |
| sdk26, sdk27 (invalidation scale/time) | *obsolete under TES-R* — idle coins never age (0 vB rent); the ladder + terminality subsume the finite-ladder rent model. No repoint exists because the phenomenon does not. |
| sdk28 (sats granularity) | *obsolete* — the in-ladder split carries its own fee model (`tier_out_total` / `committed_fee_for_outputs` / `min_child_value`); the old backup-fee floor no longer governs a split |
| sdk33 (auto-refresh) | *obsolete* — there is no ladder floor to approach (0 vB rent); unbounded **off-chain** renewal is **sdk43** |
| sdk35 (trust boundaries) | **sdk45** (the watch bundle carries NO key material; a 2nd independent tower is idempotent — both back-filled into sdk45 before sdk35 was retired), **sdk52** (a carrier is never laddered), **sdk51**, **sdk46/47/54** (R′ census) |

### Defects found and fixed while executing this migration

- **D1** — the in-ladder split admission guard used the old backup-fee floor (442 sat), but a child
  funds its **own** two tiers + dust ⟹ `min_child_value` (1306 sat at 2 sat/vB). Admitting below it
  terminalized the parent and *then* failed, stranding it.
- **D2** — `refresh_sponsored` sized its rebate into that same dead window, so a sponsored refresh
  failed **after** the user had already paid the on-chain fee.
- **D3** — the one-call Lightning PAY API minted via `ensure_exact_coin` and therefore refused every
  laddered coin (i.e. every coin).
- **D4** — a child routed to a **unilateral** exit was marked `WITHDRAWING` but has no withdrawal
  tx, so status polling errored forever.

---

## Goal & principles

The product owner wants the **V1 design gone**: the decrementing absolute-`nLockTime` backup
ladder, on-chain refresh/reanchor, the deposit-anchored `auto_exit` deadline machinery, and
the `protocol_version < 2` receiver gates. The product becomes **TES-R (Utexo V2) only**.

Hard principles, held at every stage:

1. **The name "protocol version 2" stays.** `TransferMsg.protocol_version` and `tesr_ladder`
   remain on the wire. Only the V1 *design* (ladder mechanics, on-chain refresh, absolute-
   locktime deadlines) is removed.
2. **Every stage stays green.** V1 remains the working backbone until V2 covers its job for
   that surface. No stage may ship a broken deposit / transfer / exit / RGB / Lightning path.
3. **V1 code is deleted only in the LAST stage (V2DEF-6)**, and only after a pre-deletion gate
   proves the V1 branches are already dead code (unreachable-guard run).
4. **Fund-safety linchpins are preserved EXACTLY** (no stage may weaken them for even one line):
   - **L1 — No hidden co-signed state.** The receiver can account for every SE co-sign that
     exists for the coin (the `num_sigs` invariant).
   - **L2 — Current-owner-exits-first.** The current owner's exit matures strictly before any
     state a prior owner retains (V1: decrementing absolute locktime → V2: decrementing
     relative CSV).
   - **L3 — Non-custody.** 2-of-2 + share rotation + enclave deletion; blind single-SE; the SE
     alone can spend nothing.
   - **L4 — Receiver can always unilaterally exit what it accepted**, with zero further
     counterparty or SE cooperation, from the instant `claim()` returns.
5. **No Bitcoin soft forks.** Ships on today's mainnet: v3/TRUC, P2A anchors, relative CSV
   (BIP-68/BIP-112).
6. **Blind single-SE, exact-amount off-chain split/combine, RGB blind P2P, Lightning latch**
   are all preserved unchanged in kind.

> **How principles 1–3 actually resolved.** The goal was met — there is one protocol and no way to
> opt a deposit out of the ladder — but "the V1 design gone" turned out to mean *the escape hatch and
> the aging-ladder mechanics*, not *every non-laddered transaction shape*. The signed-once backup
> chain survives as the **un-laddered shape**, because an RGB carrier must never be laddered and an
> un-broadcast-funded sub-coin cannot root a trigger. Principle 3's unreachable-guard gate is what
> caught this before anything was cut. Principles 4–6 held: no linchpin was weakened, no soft fork is
> needed, and the split/combine + RGB blind P2P surfaces are intact (Lightning is preserved in kind
> but re-mechanised as a HODL latch — see V2DEF-4).

### The one architectural decision that governs everything below

The adversarial review of receiver adoption returned a **FATAL** verdict against "Model B"
(receiver mints its own owner-paying state *after* key rotation). This spec therefore adopts
**Model A** as the fund-safety core (see the next section). Model A is the faithful V2 analogue
of what V1 already does, and it dissolves three of the reviewer's hardest findings at once
(the claim-time exit-gap, the RGB anchor loss, and the O-1 counter-machine blocker). Every
other piece in this migration is re-expressed on top of Model A; where an input design assumed
"the receiver re-establishes its own ladder," this spec overrides it.

---

## Current state — what V2DEF-1 already delivered

V2 is already a **complete payment path**, validated live on the SE + Core-30:
deposit → establish → transfer(R′) → receive → exit / renew / rollover.

- `docs/utexo/PROTOCOL.md` — TES-R design (Trigger/Extension/State CSV tiers, all un-broadcast;
  off-chain renewal; rollover; co-op de-trigger; R′ receiver check §5.11; RGB terminal-freeze
  §5.10).
- `lib/src/tesr.rs` — tier builders (`build_trigger/extension/state/detrigger`, v3 + CSV + P2A),
  `cosign_tier_request`, `TesrParams` (mainnet/regtest schedule presets, `state_csv`/`ext_csv`,
  `needs_renewal`/`needs_rollover`).
- `clients/libs/rust/src/tesr.rs` — `TesrBundle` (trigger + `Vec<TesrLevel{extension,state}>` +
  `m` + params; persisted under `tesr-<id>` via `insert_raw_backup_txs`), `establish`/
  `establish_auto`, `renew`/`renew_auto`, `rollover`/`rollover_auto`, `cosign_detrigger`,
  `persist`/`load`, `exit_tiers`, `watch_pass` (keyless tower), and **`verify_bundle`** (as executed
  it lives at `clients/libs/rust/src/tesr.rs:1194`, over `verify_bundle_ex`, with the child census
  `verify_child_bundle` alongside it), which enforces `se_num_sigs == v1_backups + tiers.len()`
  over the tiers **present in the conveyed bundle**. This equality is the hinge of the whole
  migration (see the fund-safety core) and survived execution unchanged in kind — the shipped form
  adds a `superseded_ok` term for the state each hop legitimately discloses and replaces.
- `clients/libs/rust/src/transfer_sender.rs::execute()` — loads a coin's persisted bundle and
  conveys it (`TransferMsg.protocol_version=2` + `tesr_ladder` JSON) via
  `create_transfer_update_msg_with_branch`.
- `clients/libs/rust/src/transfer_receiver.rs::validate_encrypted_message` — the two V1 receiver
  checks (`num_sigs` at ~589; `validate_signature_scheme` the V1 decrementing-ladder check at
  ~623) are gated behind `protocol_version < 2`; `>= 2` runs `verify_bundle` instead.
- `lib/src/transfer/mod.rs` — `TransferMsg` carries serde-default `protocol_version` + `tesr_ladder`.
- E2E green: sdk40 (consensus core), sdk41 (clean transfer + lock-out after rotation), sdk42
  (persistence), sdk43 (rollover — also the home of unbounded **off-chain** renewal coverage),
  sdk44 (schedule cadence), sdk45 (keyless watchtower — later back-filled with the trust-boundary
  claims retired from sdk35), sdk46 (R′ count vs live SE), sdk47 (FULL R′ transfer). All eight are
  still live.

What was **not** yet built when this spec was written, and is the subject of the stages below:
V2-native deposit, routing the wallet's four flows (transfer/exit/renewal/watchtower) through TES-R
by default, RGB + Lightning on the tier model, migrating the ~50 suites, and removing the second
lane. **All of it has since landed** — with the two corrections recorded in "Outcome — as executed":
the un-laddered shape was kept (it is load-bearing for RGB carriers and un-broadcast-funded
sub-coins, not dead code), and Lightning ships on a HODL latch rather than the sketch in V2DEF-4.

---

## The fund-safety core: receiver ladder adoption

This is the load-bearing section. It replaces the input "Model B" design and resolves the
FATAL/serious findings from the adversarial review.

### The problem restated

When Bob receives a V2 coin, the sender (Alice) conveys a ladder whose final **state** pays
Alice and whose tiers were co-signed while Alice owned the coin. The aggregate key `A` is
invariant across the transfer (V1 key-rotation: `s→s'`, `o→o'`, `o'+s' = o+s = A`), so every
`A`-paying tier Alice signed stays *spendable* after rotation — which is exactly why Alice's
stale, Alice-*paying* state is dangerous and must be out-raced, not "cancelled." Bob needs a
complete, **Bob-paying** exit chain that matures before anything Alice retains, and he must have
it with no further SE or counterparty cooperation.

### Rejected: Model B (receiver mints its own state after rotation) — FATAL

Model B had the receiver co-sign its own owner-paying `S'` *after* the key rotation completed.
The review found this is a **claim-time unilateral-exit regression and a standing SE
griefing/theft primitive**, strictly worse than V1:

> Rotation structurally *precedes* the `S'` co-sign. Once `send_transfer_receiver_request_payload`
> rotates `s→s'` (Alice's share deleted), a malicious SE can simply refuse the post-rotation
> co-sign. Bob then holds share `o'` but **no** fully-co-signed transaction paying Bob — the only
> complete chain he holds is Alice's `F→T→X→S_k`, which pays **Alice**. Bob can neither mint his
> own state (needs the SE's `s'`) nor exit without paying Alice; an SE colluding with Alice drains
> him. This violates L4.

**Accepted as FATAL. Model B is rejected in full.** All input pieces that referenced "receiver
re-establishes its own ladder inside `claim()`" (routing-defaults §"RECEIVER RE-ESTABLISH",
deposit-native, test-migration sdk48) are overridden by Model A below.

### Chosen: Model A — sender pre-signs the receiver-paying state (verify, don't trust)

This is the exact V2 lift of what V1 already does at `transfer_receiver.rs:583` (V1 conveys a
backup already co-signed to pay `new_user_pubkey`; the receiver just verifies it). In V2:

1. During `transfer_sender::execute()`, while Alice is still the owner (2-of-2 = `{o, s}`,
   aggregate `A`), Alice co-signs **one new state `S'_{k+1}`** that:
   - spends the deepest extension output `X_m.out[0]`,
   - **pays BOB's backup/exit address** (conveyed in the transfer handshake, exactly as the
     receiver's backup address is conveyed in V1),
   - carries **relative CSV `Δ_{k+1} = D0 − (k+1)·δ`** (one `δ` lower than Alice's own
     `S_k`, so it matures `δ` blocks earlier),
   - carries the RGB anchor for any colored allocation (see "RGB under Model A" below).
   This is **+1 SE co-sign** — the same cadence as V1's "one decrementing backup per transfer."
2. Because `A` is invariant, `S'_{k+1}` is a **complete, valid** transaction the instant it is
   signed and stays valid after rotation. Alice conveys the full bundle (trigger + all
   extensions + all states, ending at `S'_{k+1}` paying Bob) in `tesr_ladder`.
3. Bob's R′ (`verify_bundle` + structural checks) **verifies** — it does not trust — that
   `S'_{k+1}` pays Bob, sits at CSV `D0 − (k+1)·δ`, spends the correct parent, has the right
   amount/anchor, and that the `num_sigs` count is fully accounted (below). "The sole stated
   reason to reject Model A — 'safety depends on the sender honestly choosing a lower CSV' — is
   false; R′ verifies the CSV." (Accepted, verbatim, from the review.)
4. The transfer then rotates `s→s'`, deleting Alice's share. Bob now holds `o'` **and** a
   complete `F→T→X_m→S'_{k+1}` chain paying Bob, broadcastable unilaterally with zero further
   cooperation.

**Why Model A eliminates the exit-gap:** Bob's self-paying exit exists and is verified *at
claim*, before rotation returns. There is no window in which Bob holds share `o'` but no
Bob-paying tx. **L4 is satisfied at claim, identically to V1.** The `RECEIVED_PENDING_LADDER`
coin status, the synchronous re-establish, and the crash-recovery sweep that Model B required
are **all unnecessary and dropped** — a strict simplification.

### Sig-count accounting: full-disclosure counting (no enclave counter needed)

The review's serious findings #1/#2 (the O-1 counter machine is underdetermined from
`(level,m,k)`, and a blind SE cannot bind a declared `k` to a tx's real CSV) are **real** and
they **kill the "convey only the latest tiers + attested counter" approach.** This spec does not
use that approach. Instead it uses the mechanism V1 already relies on and that `verify_bundle`
already implements:

> **Convey the full co-signed history in the bundle, and require
> `se_num_sigs == v1_backups + tiers.len()` over every tier present.**

Why this is sound under a **blind** SE with **no enclave change**:

- The SE increments `num_sigs` **unconditionally on every co-sign** (it already does this in
  `sign.cpp`; it is enclave-autonomous and needs no tier awareness).
- The bundle carries **every** co-signed tier (trigger, all extensions from renewals, all states
  including superseded ones). `verify_bundle` counts them (`tiers.len()`).
- A hidden co-signed state — the only way a prior owner steals — would bump `num_sigs` **without**
  appearing in the conveyed bundle ⟹ `se_num_sigs > v1_backups + tiers.len()` ⟹ **reject.**
- Bob additionally checks (structural R′) that among the disclosed states, the one paying **him**
  sits at the **lowest** CSV (highest `k`); every other disclosed/superseded state is at a
  strictly higher CSV and loses the maturity race by construction (L2).

This is precisely V1's proof (count == disclosed backups) lifted to CSV tiers. It is sound for
**multi-hop, renewal, and rollover** because the equality is over *conveyed* tiers, not an
inferred formula — so the review's "underdetermined counter" objection does not apply. **The
O-1 enclave counter machine is therefore NOT a blocker for default-V2** (a significant departure
from the input designs, which treated O-1 as a hard gate).

**Cost and its bound.** The bundle grows by one state per hop and by tiers on renewal. This is
bounded because: (a) each hop consumes one `δ` of state headroom, so a coin reaches the CSV
floor after `⌊(D0 − d_floor)/δ⌋` hops (mainnet ≈ `(1440−144)/36 = 36`); (b) at the floor,
adoption **refuses in-place and forces a co-op re-anchor** (fresh `F`, `k=0`, `m=0`), which
**resets the bundle to three tiers**; (c) rollover bounds renewal depth per level. So the
conveyed bundle is at most a few dozen small pre-signed txs before a re-anchor collapses it —
the same rent V1 paid, now paid only on genuinely hot coins.

**O-1 downgraded to an optimization.** If a future session wants compact bundles (convey only
current tiers + an attested position), soundness under blindness then *requires* binding the
tier position to the **signing key**, not to a declared integer — e.g. a per-tier key tweak
`H_tag(k)` so the SE co-signs at position `k` under `A ⊕ tweak(k)` and the receiver checks the
disclosed state is spendable under `tweak(attested_k)`. That is an enclave change and is
explicitly **out of scope** for shipping default-V2. Until then, full-disclosure counting is the
sound, no-enclave-change default. (This also resolves the review's fixable "same-CSV race": two
states at the same `k` ⟹ `num_sigs` one too high ⟹ reject, no monotone-enforcement needed —
though the SE strict-monotone-`k` refusal remains valid *defense-in-depth* if O-1 is ever built.)

### The exact `num_sigs` definition (precondition — pin before building on it)

Review serious #4 and the LN serious "failed-latch poisons `num_sigs`" both hinge on **which
SE operations bump `num_sigs`.** Correctness of L1 requires:

> `num_sigs` counts **only finalized (second-round-released) tier/backup co-signs.** It MUST NOT
> count: the transfer key-rotation handshake, an aborted/withheld latch batch, or any first-round
> nonce that was never released.

This must be **verified in the server/enclave and pinned by test** before default-V2 flips (an
aborted latched renewal that advanced `num_sigs` would make a legitimately-owned coin fail every
future `verify_bundle` = permanent un-transferability). See sdk-latch-abort regression in V2DEF-3/4.

### RGB under Model A (resolves the review's FATAL #2)

The review's RGB FATAL against Model B was that Bob's freshly-minted `S'` is a *different* tx
than Alice's RGB-anchored `S_k`, so the colored anchor lives in a tx Bob never broadcasts. **Under
Model A this dissolves:** Alice *constructs* `S'_{k+1}` (the tx Bob will actually broadcast) and
therefore **anchors the RGB commitment into `S'_{k+1}` and hands Bob the matching blind-P2P
consignment** — exactly as V1 anchors into the conveyed receiver-paying backup. No fresh
consignment generation by a blind receiver is needed. The terminal-freeze count check (below,
V2DEF-4) still applies to colored ancestors. RGB coins are therefore **not** barred from the
default-V2 path under Model A.

### Stale-tier supersession (L2, made explicit)

Nothing is ever "revoked"; the current owner always out-races. After any `T + X_m` confirm,
Alice's `S_k` (CSV `D0−k·δ`) and Bob's `S'_{k+1}` (CSV `D0−(k+1)·δ`) both spend `X_m.out[0]` — a
mutually-exclusive on-chain race that Bob's lower CSV wins by `δ` blocks. Key rotation is the
anti-rewind: Alice cannot obtain an `S` at `k+2` because that needs an SE co-sign against the
rotated `A`, which the SE performs only for the new owner's auth key. Sizing `δ` vs. mainnet
congestion is memory **O-2** and must set `d_floor`/`δ` margins so Bob wins under fee spikes.

### Fund-safety linchpin summary for the core

- **L1** — moved from V1's `num_sigs == backup_count` to `se_num_sigs == v1_backups + tiers.len()`
  over the **fully-disclosed** bundle. Hidden co-sign ⟹ count high ⟹ reject. Residual trust =
  the enclave honestly incrementing `num_sigs` (pre-existing B1 floor; no new trust).
- **L2** — receiver's `S'_{k+1}` at `D0−(k+1)·δ` matures `δ` before any state Alice retains;
  rotation is the anti-rewind. Structural analogue of V1's decrementing absolute locktime.
- **L3** — unchanged: the pre-signed `S'_{k+1}` co-sign is one more blind-MuSig2 `cosign_tier`
  round-trip; the SE sees a 32-byte sighash, learns nothing, no enclave rebuild.
- **L4** — satisfied **at claim**: Bob holds a complete Bob-paying `F→T→X→S'` chain before
  `claim()` returns. No pending-ladder window.

---

## Stage V2DEF-2 — V2-native deposit (+ the migration mode flag)

Goal: a fresh deposit auto-establishes and persists a TES-R ladder at CONFIRMED rest, so new
coins are `protocol_version=2` **without breaking the ~50 V1 sig-count tests.**

### The mode gate

Two equivalent gates exist; this spec uses **both, layered**:

1. **Version signal = bundle presence.** A coin is V2 iff a persisted `tesr-<statechain_id>` row
   exists (already `transfer_sender.rs:282`'s source of truth). No new `Coin.protocol_version`
   field is added on the *routing* side — bundle-presence is the single source of truth, is
   restore-safe (rides the recovery bundle via `get_all_backup_txs`), and self-gates: a coin can
   never be "v2 with no exit material."
2. **Establish gate = an explicit config flag** so deposit *decides whether to create* a bundle:
   add `deposit_protocol_version: u32` to `SdkConfig` (seeded from env `UTEXO_PROTOCOL_DEFAULT`),
   **default `1`** in both `regtest()` and `mainnet()` during migration. Auto-establish runs only
   when `== 2`. With the default at 1, no deposit gains extra SE co-signs, so every suite that
   asserts `num_sigs == backup_count` (sdk10/sdk12 at the time of writing) is byte-for-byte
   unaffected.

   > **OUTCOME.** Gate 1 (bundle presence) is what shipped and is still the routing truth. Gate 2
   > was a **migration scaffold only**: `deposit_protocol_version` and `UTEXO_PROTOCOL_DEFAULT` were
   > carried through the flip and then **deleted**. `claim()` now establishes a ladder for every
   > fresh confirmed ROOT coin unconditionally, and nothing can opt a deposit out. Of the two
   > count-asserting suites named here, **sdk10 is gone** (its exit/terminal claims live in **sdk50**
   > and **sdk58**); **sdk12** is live and now asserts the tier counts.

### The establish hook

In the SDK `claim()` pass (`wallet.rs`), after the second `update_coins`, for each coin where:
`status == CONFIRMED` **and** `duplicate_index == 0` **and** `!single_use` **and**
`deposit_protocol_version >= 2` (**as executed: this clause is gone — the hook is unconditional**)
**and** `tesr::load(...) == None` (idempotency) — call
`tesr::establish_auto(&cc, &mut coin, &coin.backup_address, network)` then `tesr::persist`, save
the coin, emit `WalletEvent::LadderEstablished{statechain_id}`.

- **The exit payee is `coin.backup_address`** = seed-derived `P2TR(user_pubkey)` (`lib/src/wallet/mod.rs`,
  the same address V1's `create_tx1` pays, recoverable from the mnemonic). **Not** the
  `bitcoin_core::getnewaddress()` the tests use — that key is outside the wallet hierarchy and
  would make the coin unrecoverable. Production establish MUST pass `coin.backup_address`.
- **CONFIRMED, not IN_MEMPOOL** — the trigger `T` spends `F` and R′ requires `F` on-chain and
  unspent; a confirmed `F` is a sound tree anchor that cannot be reorged out from under the
  pre-signed tiers.
- **`single_use` and duplicate coins are excluded** — single-use RGB tree nodes have their own
  branch-exit model and are not re-signable; excluding them keeps the off-chain split/combine DAG
  and RGB blind carriers untouched. **This exclusion is permanent, not transitional:** it is what
  makes the UN-LADDERED shape a first-class lane rather than a leftover (an RGB carrier must never
  be laddered — PROTOCOL.md §5.10; **sdk52**), joined by any split sub-coin whose funding is
  un-broadcast and therefore cannot root a trigger [B0].

### Does a V2-native coin still need a V1 `tx1`? — staged

- **Stage 2a (ship first):** flag-gated coin keeps its V1 `tx1` (`create_tx1` unchanged in
  `check_deposit`) **and** gets the auto-established ladder. This is exactly sdk47's shape
  (`tx1` + 3 tiers); at transfer, `verify_bundle` runs with `v1_backups=1`, `tiers=3` ⟹
  `num_sigs=4`. Exit/withdraw/`estimate_exit_cost` keep reading the V1 backup rows. Minimal blast
  radius. **The added state that Model A pre-signs at transfer is +1 more; the receiver's
  full-disclosure count already accounts for it.**
- **Stage 2b:** `check_deposit` skips `create_tx1` for `protocol_version >= 2` coins (same
  mechanism as the `single_use` skip). `num_sigs=3`, `v1_backups=0`. **Requires** the exit paths
  (`withdraw`, `unilateral_exit`, `estimate_exit_cost`, `auto_exit_due`) to drive the tier ladder
  / `watch_pass` instead of the V1 backup rows — i.e. Stage 2b must **not** land before the exit
  migration in V2DEF-3.
- **Stage 2c:** flip `SdkConfig` default to `2`. Ships only after the ~50 V1 suites are migrated
  or pinned (V2DEF-5) and after the partial-establish atomicity fix (below).

> **OUTCOME — Stage 2a is the shipped shape; 2b was never taken, deliberately.** A laddered root
> coin still gets its one signed-once backup at deposit confirmation (`coin_status::check_deposit`
> → `create_tx1`), so the census baseline is a **constant**, pinned in code as
> `tesr::PARENT_V2_BASELINE = 1` (with `CHILD_V2_BASELINE = 0` for a split child slot, which is never
> funded on-chain and so never runs `check_deposit`). Those two constants are what let a split-child
> receiver — who cannot observe the parent's pre-establish history — run `verify_child_bundle`'s
> exact-equality census **independently of the sender**. Stage 2c happened and then went further: the
> flag itself was deleted, so the ladder is unconditional rather than defaulted.

### Partial-establish atomicity (accepted open risk — must fix before Stage 2c)

`establish_auto` is 3 non-atomic co-signs (T, X_0, S_0). A failure after co-signing T/X but
before persisting the full bundle leaves `num_sigs` ahead of the persisted bundle; a naive retry
co-signs 3 more ⟹ `num_sigs` overshoots ⟹ `verify_bundle` rejects ⟹ coin un-transferable (funds
still exit-safe via `tx1` in Stage 2a, but stuck for transfer). **Mitigation (required):** make
establish **resumable** — persist tier-by-tier and, on a later `claim()` pass, resume co-signing
only the missing tiers so `num_sigs` converges to exactly `tiers.len()`; alternatively mark a
partially-established coin and refuse further establish. This is the one place the coin's own
`num_sigs`-vs-bundle accounting can drift; it is local to establish and does **not** require O-1.

### Files touched

`clients/libs/rust-sdk/src/{config.rs,wallet.rs,events.rs}`,
`clients/libs/rust/src/{deposit.rs,coin_status.rs,tesr.rs}`, `lib/src/wallet/mod.rs`,
new `clients/tests/rust/src/sdk48_v2_native_deposit.rs`.

### Coexistence / fund-safety / gating tests

- Coexistence: at flag 1, `get_deposit_address`/`check_deposit`/`claim()` are byte-identical to
  today. At flag 2 (Stage 2a) tiers are *added on top of* the unchanged `tx1` — a strict superset;
  the coin's signed-once backup exit still works AND it carries a conveyable ladder. Mixed estates
  interoperate (receiver dispatches on the conveyance shape; sender conveys a bundle only if
  `tesr::load` finds one). **As executed this stopped being "coexistence during a migration" and
  became the steady state: one protocol, a laddered shape and an un-laddered shape, dispatched by
  bundle presence.**
- Fund-safety: L1 via `verify_bundle` (2a: 4; 2b: 3); L4 via seed-derived `backup_address`; L2 via
  schedule-driven decrementing CSV; L3 via identical blind-MuSig2 round-trip (no enclave rebuild).
- Gating tests: **sdk48** (auto-persist, payee == `backup_address`, `num_sigs` == stage count,
  `verify_bundle` passes, transfer→receive via R′); idempotency (two `claim()` ⟹ one bundle);
  exclusion (single_use + duplicate ⟹ no ladder); resume/failure (inject mid-establish SE failure,
  re-run `claim()`, assert `num_sigs == tiers.len()`, transfers green); **regression** — full ~50
  suites at flag 1 with unchanged `num_sigs`.
  **Landed as `sdk48_v2_native_deposit` (live).** The resume/idempotency half also has a dedicated
  survivor, **sdk56** (keystone retry is idempotent); the exclusion half is **sdk52** (a carrier
  never gains a ladder). The "full suite at flag 1" regression was a migration-time gate only — the
  flag is gone, so the standing regression is simply the whole live matrix.

---

## Stage V2DEF-3 — route transfer/exit/renewal/watchtower through TES-R by default

Goal: wire the rust-sdk `UtexoWallet`'s four flows to dispatch on version (= bundle presence)
and, for V2 coins, use the tier ladder. The routing lands **dormant** (no coin has a bundle until
V2DEF-2's flag flips), so the full existing suite runs byte-for-byte.

### Version classification

Add `async fn v2_coin_ids(&self) -> Result<HashSet<String>>` = `get_all_backup_txs` once, keep
keys starting with `tesr-`, strip the prefix. Each flow computes this set once (mirrors the
existing `carriers` set) for O(1) `is_v2(id)`; the full bundle is loaded via `tesr::load` only
when a flow acts. **No `Coin.protocol_version` field** — avoids the 2-write consistency gap.

### Transfer — Model A pre-sign (overrides the input "receiver re-establish")

- **Sender side (`transfer_sender::execute`, `transfer.rs`):** split
  `auto_refresh_before_spend()` into `auto_maintain_before_spend()`. V1 coins → unchanged
  reanchor+confirm-wait. V2 coins → `tesr_renew_due`: if the deepest extension CSV is within
  `renew_margin` of `e_floor`, `renew_auto`; if it returns rollover-due, `rollover_auto`;
  re-persist. This is **off-chain, zero on-chain bytes, no confirm-wait**, and it is CSV-budget-
  driven, not wall-clock-driven (idle coins never age). Then, per the fund-safety core, **the
  sender co-signs `S'_{k+1}` paying the receiver's conveyed exit address at CSV `D0−(k+1)·δ`,
  anchors any RGB commitment into it, appends it to the bundle, and conveys the full bundle** at
  `protocol_version=2`.
- **Receiver side (`transfer_receiver::validate_encrypted_message` + `claim()`):** R′ =
  `verify_bundle` (full-disclosure count) + structural checks that the deepest state pays **Bob**
  at the lowest CSV. **No re-establish, no `RECEIVED_PENDING_LADDER` status, no synchronous
  re-sign** (all deleted vs. the input design). Bob's exit is complete at claim.

### Unilateral / cooperative exit

- **Unilateral (`wallet.rs::unilateral_exit`):** for a V2 coin, load the bundle and broadcast
  `tesr::exit_tiers()` in order (trigger first — no CSV — then each extension/state as its
  relative timelock matures), idempotent across passes. Factor a shared
  `broadcast_exit_tiers(bundle)` reused by `watch_pass`.
- **Cooperative withdraw (`wallet.rs::withdraw`, `refresh.rs`):** `withdraw::execute` already does
  a fresh SE co-signed spend of the coin's current on-chain outpoint `F`; for a laddered coin just
  skip `broadcast_branch_if_any` (it has no `branch-*` rows). The coin goes terminal (WITHDRAWING →
  WITHDRAWN), so the extra co-sign of `F` never affects a later receiver's R′ count.

  > **OUTCOME.** Cooperative withdraw never needed its own suite: with one protocol, every coin
  > `sdk01` withdraws is laddered, so `sdk01` (deposit → transfer → cooperative exit to L1) is the
  > live coverage, with `sdk11` covering the withdrawal fee quote. **D4** was found here — a split
  > child routed to a **unilateral** exit was marked `WITHDRAWING` even though it has no withdrawal
  > tx, so status polling errored forever; the two exits now report distinct terminal states.

### Watchtower

Dispatch per coin in the background loop. V1 → existing `auto_exit_due` (deposit-anchored deadline)
+ optional `auto_refresh_due`. V2 → `tesr::watch_pass(cc, &bundle)` each poll: **reactive** (no-op
unless `F` is spent by someone broadcasting the trigger), then drives the owner's tier exit.
**No deadline pass for V2** — an idle un-broadcast coin never ages. Add `tesr_renew_due` to the
background pass for CSV-budget maintenance. `export_watch_bundle` emits the `tesr-<id>` JSON (the
`TesrBundle` **is** the keyless watch bundle — every tier pays the owner, no key material).
`refresh.rs::auto_refresh_due` gains a guard to **skip** V2 coins; `reanchor()` **hard-errors** on
a V2 coin (a plain re-anchor would strand the bundle); public `refresh()` on a V2 coin routes to
off-chain `tesr_renew`.

> **OUTCOME.** This is what shipped, and it is why `refresh.rs` was *not* deleted in V2DEF-6: the
> on-chain re-anchor still serves the un-laddered shape, and `refresh_sponsored` was kept as a
> product feature (**sdk30**, **sdk38** live). **D2** was found here — `refresh_sponsored` sized its
> rebate into the dead window below `min_child_value`, so the sponsored leg failed *after* the user
> had already paid the on-chain fee; the rebate is now `max(fee + DUST_LIMIT, min_child_value)`.

### The `num_sigs` cross-hop concern — resolved

The input flagged "cumulative `num_sigs` across a hop breaks the next receiver's exact count" as a
**hard blocker gated on O-1.** **Under Model A + full-disclosure counting this is not a blocker:**
each hop adds exactly one state (`+1`) and that state is **conveyed** in the bundle, so
`se_num_sigs == v1_backups + tiers.len()` holds at every hop, multi-hop, and after renew/rollover.
The only precondition is the exact-`num_sigs` definition above (finalized co-signs only).

> **OUTCOME — this multi-hop census is what made received split children first-class**
> (`docs/utexo/CHILDREN.md` builds on this paragraph). Each hop costs **exactly one**
> co-signature and discloses **exactly one** superseded state, which the receiver's census counts
> and proves out-raced — so a child is paid onward off-chain **whole** (`child_retransfer`) or
> **split** (`child_in_ladder_pay`, a depth-2 `ancestors` chain) with no on-chain footprint. The
> claim completes the **standard SE key handover**, so the receiver co-owns `A_child` (invariant
> across the rotation — which is precisely what keeps the pre-signed exit chain valid) and the
> sender is permanently locked out. Proven by **sdk60** (alice → bob → carol, funding outpoint
> unspent throughout) and **sdk17** (partial second hop). The child census runs against the
> `PARENT_V2_BASELINE`/`CHILD_V2_BASELINE` constants, so the receiver never trusts a sender-supplied
> count.

### Files touched

`clients/libs/rust-sdk/src/{wallet.rs,transfer.rs,refresh.rs,watchtower.rs,config.rs,lib.rs}`,
`clients/libs/rust/src/{tesr.rs,transfer_sender.rs}`, new
`clients/tests/rust/src/{sdk49_wallet_v2_transfer.rs,sdk50_wallet_v2_watchtower.rs,sdk51_wallet_v2_withdraw.rs}`
— **as landed the three suites are `sdk49_model_a_transfer.rs`, `sdk50_v2_unilateral_exit.rs`,
`sdk51_v2_watchtower.rs`** (cooperative withdraw folded into `sdk01`; see the note above).

### Coexistence / fund-safety / gating tests

- Coexistence: with no bundle present, `v2_coin_ids()` is empty, every dispatcher takes the V1
  branch; the ~50 suites run unchanged. The "mode flag / staged flip" is subsumed by
  bundle-presence — there is no global boolean that can break V1.
- Fund-safety: L1 via `verify_bundle` on receive (full disclosure); L2 via Model A's `k+1` lower
  CSV; L3 unchanged; L4 at claim (Model A). The near-floor **re-anchor F-race** (Alice's retained
  trigger also spends `F`) is handled by keeping the coin non-spendable until the re-anchor
  **confirms**, and by a co-op de-trigger of Alice's trigger if she races it (document the F-level
  race).
- Gating tests, **as landed** (the numbering shifted by one from the plan):
  **sdk49** Model-A transfer (conveyed deepest state pays Bob; Bob adopts and `unilateral_exit`
  confirms the whole chain); **sdk50** the *SDK* `unilateral_exit()` walking the tier chain as each
  relative CSV matures, with no signed-once backup broadcast; **sdk51** background watchtower
  (someone else spends `F` ⟹ `defend_ladders()` drives the owner's exit; idle coin ⟹ zero on-chain
  footprint over many polls); cooperative withdraw → **sdk01**; **latch-abort / failed-pay
  regression** (an aborted latch leaves the coin re-transferable and its count clean) → **sdk68**
  (exact) and **sdk66** (non-exact rollback), with **sdk53** pinning that the old blanket LN-latch
  refusal is lifted; **census cannot be padded or inverted** (the property behind "never a silent
  mis-route") → **sdk54** (`verify_bundle` count padding, S1) and **sdk55** (conveyed backup chain
  padding/inversion, S2); whole-matrix regression green.

---

## Stage V2DEF-4 — RGB terminal-freeze anchoring + Lightning latch on tiers

Goal: convert colored split/combine to the TES-R tx format, add the count-based terminal-freeze
receiver check, and compose the Lightning latch — with the two RGB FATAL findings resolved.

### Carrier model

A token carrier is an on-chain colored root `F = P2TR(A)` plus a **signed-once colored DAG** of
splits/combines. The colored split/combine **IS** the colored STATE tier and the **only** tier a
carrier has: **no sats T/X/S tiers, no colored trigger, no colored extension, no renewal, no
rollover, no materialization deadline.** Justification: (1) idle carriers are un-broadcast so
never age; (2) terminal-freeze — every colored ancestor is set `spend_budget=1` and terminalized
**before** its colored child is co-signed, so each seal has exactly one valid closing; (3) a token
"hop" is a **new** colored split (new DAG node), not a horizontal re-sign.

> **OUTCOME.** This is the shipped carrier model, and it is the reason the **UN-LADDERED shape is
> permanent**: a carrier must never gain a T/X/S ladder, because a plain tier spend would destroy the
> allocation (PROTOCOL.md §5.10). **sdk52** pins it on the live stack — in one wallet the plain coin
> carries a ladder, the carrier carries none, and an off-chain RGB transfer still settles.

### Two structural rules that resolve the RGB FATAL findings

1. **Carriers are born from a fresh `F` (`v1_backups == 0` by construction).** (Review FATAL #1 —
   accepted.) A single scalar `num_sigs` on a **sats-then-carrier** seal cannot distinguish
   "4 sats-tiers + 1 colored" from "3 sats-tiers + 2 colored," which admits a hidden colored
   double-spend (Alice closes seal `F` to Bob *and* Carol; one confirms, the other's allocation is
   voided = token loss). **Fix:** carriers must be minted via `fund_statechain`/`register_statechain`
   over a fresh `F` that never carried sats V1 backups or sats tiers, so the colored-closing count
   is exactly `num_sigs` and the check `num_sigs == 1 per colored ancestor` is unambiguous. The
   migration MUST assert no code path colors an existing sats-tiered coin. (This makes the O-1
   per-seal colored counter an *optimization*, not a blocker — same downgrade as the sats core.)
2. **The count-based terminal-freeze check is the SOLE colored accept gate.** (Review FATAL #2 —
   accepted.) The existing INV-20 boolean check (`verify_terminal_parents`) proves only
   `terminal == true` (budget exhausted = no *more* co-signs); it does **not** prove exactly one
   was **ever** signed. If budget was ever `>1`, Alice co-signs seal `F` to Bob and Carol, *then*
   drops budget to 0; both see `terminal == true` = double-spend. **Fix:** the receiver's accept
   gate for every colored ancestor is `num_sigs == v1_backups(=0) + (disclosed colored closings)`
   — the count-based analogue of the sats `verify_bundle` linchpin — replacing the boolean check
   for all colored branches. **New V1 colored issuance is disabled at the FIRST migration stage**
   so the boolean-only-exposed V1 carrier population only shrinks.

### Validation-mode dispatch bound to SE-authenticated metadata (review serious — accepted)

`validate_encrypted_message` must decide "carrier vs sats coin" from **SE-authenticated coin
metadata** (`statechain_info` / the seal's registered record resolved from the SE, and
`unspendable_as_btc_outpoints`/`is_token_carrier`), **never** from sender-supplied message fields
(presence of `tesr_ladder` vs consignment). Otherwise a malicious sender crafts a colored `v2`
message that routes to the weaker path (downgrade attack). Hard-select: sats coin → `verify_bundle`;
carrier → the colored count check above.

### Colored-tx format change

Convert `create_colored_split_tx`/`create_colored_combine_tx`/`create_colored_backup_tx` (and the
`lib/src/transaction.rs` `get_unsigned_split_psbt`/`get_unsigned_combine_psbt`/`get_unsigned_backup_psbt`
they call) from V1 (version 2 + absolute decrementing `nLockTime` via `initlock`/`interval`) to
TES-R form: **version 3, `lock_time=0`, each `TxIn.sequence = csv_blocks(Δ)`** with
`Δ = TesrParams.state_csv(k)` for the hop, **append the P2A output** (`P2A_VALUE=240`,
`p2a_script()` = `OP_1 0x4e73`) so `Σout = Σin − committed_fee − P2A_VALUE`. The `rgb.color()`/
`color_blinded()` step is unchanged (the opret/tapret commitment rides a v3 tx byte-identically;
the SE sees the same sighash). All behind the same `protocol_version`/config gate as the sats side.

**Required fix (review fixable — confirmed real):** the vout filter at `rgb.rs` (~127, ~265, ~391)
filters only `!o.script_pubkey.is_op_return()`; the P2A output (`OP_1` push, **not** an OP_RETURN)
would be miscounted as a spendable sub-coin, corrupting the vout→sub-coin map. Exclude **both**
`is_op_return()` **and** `p2a_script()` at all three sites; give P2A a stable pre-coloring index in
the rgb-lib `output_map`; assert `color_psbt_and_consume` preserves `nVersion=3` and the P2A output
through the PSBT round-trip (O-3, pinned fork).

Call sites (`tokens.rs::{colored_transfer, colored_combine_transfer, batch_transfer_tokens}`):
replace `server_info.{initlock,interval}` with the CSV `Δ` from `TesrParams`; keep
`set_spend_budget(carrier_id, 1)` verbatim (the terminal-freeze enforcement); update
`fee_reserve`/`min_split_output` for `committed_fee + P2A`.

### Exit liveness under TRUC 1P1C (review serious — accepted with mitigations)

- **Multi-owner sibling griefing.** A colored split with outputs to different parties (piece→Bob,
  change→Alice) is un-broadcast; TRUC permits only one unconfirmed v3 child of the split. Alice can
  park a low-fee v3 child on her change output, and Bob's child (different output, not an RBF
  replacement) cannot be broadcast until the split confirms. **Mitigation:** for any colored split
  with `>1` distinct-owner output, the sender must **broadcast + confirm the shared split at
  handover** (sacrificing un-broadcast only for genuinely multi-owner splits), or structure each
  recipient's exit so it never shares an unconfirmed v3 ancestor with another party's spendable
  output.
- **Token-only receiver CPFP under a fee spike.** A pure-token wallet holds no sats UTXO to attach
  a P2A-bumping child; if market feerate exceeds `committed_fee_rate`, the pre-signed exit is valid
  but unconfirmable and carriers have no deadline. **Mitigation:** guarantee a bump path
  independent of the receiver's sats balance — keyless-tower-funded P2A bump in `watch_pass`,
  and/or a documented conservative `committed_fee` ceiling. (The anyone-can-spend P2A slot makes a
  griefer's competing child RBF-defeatable — the safe half — but the *no-sats-to-bump* case must
  be covered.)

### Lightning

Token latch is **unchanged**: `latch_tokens`/`latch_tokens_se_preimage` build the (now v3) colored
split, then `create_external_hash_latch`/`create_pre_image` lock the piece's release on the
preimage/`batch_id` exactly as today; the latch gates the key-handover, orthogonal to tx format.
`start_lightning_swap`'s token-carrier guard is preserved verbatim. §5.12's "renewal co-signs gated
by the same preimage" bites **sats coins only** (carriers never renew): when `transfer()` runs
`renew_auto` under a latch, the `X_{m+1}` and `S'_{k+1}` `cosign_tier` calls must ride the **same
`batch_id`** so the SE withholds finalization until `confirm_pending_invoice` — reusing the existing
latch batch primitive, **no new SE endpoint, no enclave change.** Tie-in to the sats core's exact-
`num_sigs` rule: an aborted latch batch advances `num_sigs` by **zero** (else the coin is poisoned).

> **SUPERSEDED by `docs/utexo/LIGHTNING.md`.** The carrier half above is what shipped; the *sats*
> half is not. Gating the Model-A `S'` co-sign on a preimage cannot work — the SSP must validate
> ownership **before** it pays, and any ownership proof it can validate is also broadcastable — so
> the sats lane pivoted to a **HODL-invoice latch** at the same trust bar: the SSP runs a **pre-pay
> census** (`verify_bundle` over the conveyed ladder, `num_sigs` read from the enclave sig-count)
> and only then calls `send_payment`. Lightning now works **both directions on the ladder**:
> **sdk63** (V2 pay), **sdk64** (V2 receive), **sdk67** (in-ladder receive), **sdk65** (non-exact
> pay), and the failure paths **sdk66** (non-exact rollback) / **sdk68** (exact reclaim). The blanket
> refusal of a latched laddered transfer is lifted — **sdk53** pins that. **The LN-latched piece is
> the ONE case that stays terminalized**: it sits unclaimed past the pending-transfer lock's window.
> **D3** was found here — the one-call Lightning PAY API minted via `ensure_exact_coin` and so
> refused every laddered coin, i.e. every coin.

### Files touched

`clients/libs/rust/src/rgb.rs`, `lib/src/transaction.rs`, `lib/src/tesr.rs`,
`clients/libs/rust-sdk/src/{tokens.rs,lightning.rs}`, `clients/libs/rust/src/transfer_receiver.rs`,
`clients/libs/rust-rgb/src/lib.rs`; delete `rgb.rs::refresh_rgb_anchor_self_transfer` +
`RgbStatechainStatus::RgbAnchorRefresh*` **in V2DEF-6**.

### Coexistence / fund-safety / gating tests

- Coexistence: colored-tx format gated like the sats `protocol_version`; default V1 through
  migration; new V1 colored issuance disabled at the **first** stage. Receiver accepts both v2 and
  v3 colored branches (`validate_offchain_chain_info` is format-agnostic) but only the **count-based**
  terminal-freeze gate accepts. **Sig-count-neutral:** a carrier has exactly one co-sign per hop in
  both V1 and V2, so sdk02/sdk09 token assertions do not change.
- Fund-safety: L1 extended to tokens (`num_sigs == 1` per colored ancestor, carriers fresh-`F`);
  terminal-freeze via `set_spend_budget(carrier,1)` before the single co-sign; anchors only in
  signed-once txs; L4 via broadcasting the pre-signed colored ancestor chain to the resting output;
  L3 blind SE unchanged (colored v3 sighash indistinguishable). LN atomicity: latch preserved,
  `validate_pending_token` still fires pre-payment.
- Gating tests, **as landed**: **sdk52** is the anchor suite — carrier ⊥ ladder (the fresh-`F` /
  "no path ladders a carrier" assertion) with an off-chain RGB transfer settling alongside a laddered
  coin. Colored split/combine + booked-vs-consignment stay on **sdk02**, **sdk09**, **sdk29**,
  **sdk31**, **sdk39** and the whole **rgb01–rgb14** family (all live, all sig-count-neutral as
  predicted); the carrier watchtower/materialization half is **sdk32** + **sdk34**. Terminal-freeze
  and count-padding adversarials are **sdk54**/**sdk55**. Lightning is the HODL suite set
  **sdk63/64/65/66/67/68** (+ **sdk23** RGB↔LN swap, **tb04** latch), not the preimage-gated-renewal
  sketch above. Still-open (no dedicated suite, tracked in "Residual risks"): token-only CPFP under a
  fee spike, and the multi-owner-split exit ordering.

---

## Stage V2DEF-5 — migrate the ~50 test suites

The matrix is dispatched from `clients/tests/rust/src/main.rs` via `SDK_E2E`/`RGB_E2E` indices and
run by `clients/tests/run_all_suites.sh`. Migration toggles the `UTEXO_PROTOCOL_DEFAULT` knob
(V2DEF-2) per run; `run_all_suites.sh` passes it alongside `SDK_E2E` so each suite is pinned.

> **The knob is gone.** It existed only for the double-run phases below; today every suite runs on
> the one protocol and nothing is pinned. The dispositions of every id named in this stage are in
> the OUTCOME block after the buckets — read that before trusting a test id here.

### Inventory — five buckets (`clients/tests/rust/src`)

**A. V1 exit-mechanic suites (REPLACE with CSV-relative equivalents, not re-run):**
- `sdk07_unilateral_exit` — receiver-backup-before-sender ordering → V2: `exit_tiers`, assert
  receiver state-CSV matures before the carried stale sender state. **Note the Model A change:** the
  carried stale sender state is Alice's `S_k`; the receiver's is the sender-pre-signed `S'_{k+1}`.
- `sdk13_stale_state` → V2 CSV de-trigger race (base: sdk40 Part 3).
- `sdk14_watcher_race` → un-broadcast tiers never age; deadline becomes CSV maturity (base: sdk45).
- `sdk26_invalidation_scale` / `sdk27_invalidation_time` — **keep** the split/branch-structure
  asserts (orthogonal, preserved); **replace** the absolute-locktime-decrement asserts with CSV
  cadence (overlaps sdk44).
- `tb05_timelock` — `backup_transactions.len()==3` + locktime height-diff → V2: tier count + relative
  CSV delay.
- `tv01` — upstream deposit→transfer (Group D).

**B. Refresh / on-chain re-anchor (DELETE — V2 renewal is off-chain):**
- `sdk30_refresh` → coverage moves to sdk42 (lifecycle) + sdk43 (rollover); DROP after.
  **`refresh_sponsored` (operator rebate) has no V2 analogue — product decision required** (ship
  sponsored-rollover or drop the feature+tests); blocks deleting sdk30/38 until decided.
- `sdk33_auto_refresh` — "re-anchor inside `transfer()`" concept is gone; DROP unless product wants
  an auto-renew-in-transfer UX (`needs_renewal`), which must be specified first.
- `sdk38_sponsor_stiff` — tied to sponsored path; DROP or re-express (same product decision).

**C. `auto_exit` deadline / watchtower (re-express against keyless `watch_pass`):**
- `sdk34_token_watchtower` → `watch_pass`; RGB terminal-freeze still needs an exit trigger, coverage
  stays via the keyless bundle.
- `sdk35_trust_boundaries` — UPDATE in place: drop the `auto_refresh` half, keep the `watch_pass` half.
- `sdk32_token_over_time` — UPDATE: materializable-forever via un-broadcast tiers + `watch_pass`.

**D. Transfer/receive suites with no explicit V1 ladder asserts (become V2 when default flips;
audit for hard-coded counts):** sdk01–06,08–12,15–21,23–25,28,29,31,36,37,39; tb01–04; ta01–03;
tm01; tv01; rgb01–14; chaos22. Concrete hits: `tb05` (moved to A); `sdk10` tampers the `parents-<id>`
structural rows (orthogonal, survives); sdk28/26/39 assert branch/split lengths (structure,
survive). **RGB suites exercise V2 carriers end-to-end once flipped — the real regression surface;
flip them LAST within Group D and give them a full double-run.**

**E. Already-V2, green — sdk40–47.** Keep as the V2 backbone/oracle (sdk46/47 assert exact
`se_num_sigs` vs the live SE = the L1 linchpin).

**F. Harness / non-protocol** — `bitcoin_core`, `electrs`, `rgb_dump`, `rln`, `UNIT`, `UPSTREAM`.
`UPSTREAM` (original Mercury V1 library suite) is intrinsically V1 and dies with V1 code — freeze at
a git tag or drop from the matrix in Phase 4.

### OUTCOME — the actual disposition of every id named above

The plan's buckets survived; several verdicts did not. **Retirement discipline held throughout: a
suite was deleted only after its surviving property was re-verified green inside a live suite in the
same session, back-filling the property first where it was unique** (that is how sdk35's claims got
into sdk45).

- **A (exit mechanics).** `sdk07`, `sdk08`, `sdk10` — **deleted**; their claims are **sdk50** (the
  SDK walks the tier chain to a unilateral exit) and **sdk58** (11 adversarial exit/terminal cases,
  all REJECT). `sdk13`, `sdk14` — **deleted**; their claims are **sdk51** (the watchtower defends a
  hostile trigger), **sdk40 PART 2** (a stale state dies at consensus) and **sdk45** (keyless tower).
  `sdk26`/`sdk27` — **deleted, not repointed**: under TES-R idle coins never age (0 vB rent) and the
  ladder + terminality subsume the finite-ladder rent model, so the invalidation phenomenon they
  measured no longer exists. `tb05_timelock` — **kept and re-expressed** (tier count + relative CSV);
  it is live. `tv01` — live.
- **B (refresh).** The product decision was resolved as **KEEP**, so the bucket was *not* deleted:
  `sdk30_refresh` and `sdk38_sponsor_stiff` are **live**, and `refresh`/`refresh_sponsored`/
  `reanchor` still ship for the un-laddered shape. Only `sdk33_auto_refresh` was **deleted**, as
  *obsolete*: there is no ladder floor to approach any more; unbounded **off-chain** renewal is
  covered by **sdk43**. Separately `sdk28` (sats granularity) was **deleted as obsolete** — the
  in-ladder split has its own fee model (`tier_out_total` / `committed_fee_for_outputs` /
  `min_child_value`) and the old backup-fee floor no longer governs a split. **D1** came out of that
  fee model: the split admission guard still used the 442-sat backup floor, but a child funds its own
  two tiers + dust ⟹ `min_child_value` (1306 sat at 2 sat/vB); admitting below it terminalized the
  parent and *then* failed, stranding it.
- **C (deadline / watchtower).** `sdk34` and `sdk32` — **kept and updated in place**, both live.
  `sdk35_trust_boundaries` — **deleted**, but only after its unique claims were back-filled: the
  watch bundle carries **no key material** and a **second independent tower is idempotent** now live
  in **sdk45**; the carrier-never-laddered boundary in **sdk52**; the defence itself in **sdk51**;
  the census boundary in **sdk46/47/54**.
- **D.** The list as written names ids that no longer exist. Live today: sdk01, sdk02, sdk04, sdk09,
  sdk11, sdk12, sdk15, sdk16, sdk17, sdk19, sdk20, sdk21, sdk23, sdk24, sdk25, sdk29–32, sdk34,
  sdk36–68, chaos22, tb01–05, ta01–03, tm01, tv01, rgb01–14. Deleted from the group: sdk03, sdk05,
  sdk06 (LN core → **sdk63/64/67**, plus **sdk65** non-exact and **sdk66/68** failure), sdk18 (LN
  pay-failure reclaim → **sdk68** exact, **sdk66** non-exact rollback), sdk08/sdk10 (→ A above),
  sdk28 (→ B above). The RGB family did flip last and is green.
- **E.** sdk40–47 are still the backbone/oracle, now joined by sdk48–68.

**The two suites that took longest to clear** were `sdk17` (needed first-class children — the last
genuine feature gap, closed by the key-handover child conveyance) and `chaos22` (the only concurrency
suite). Both are live on the one protocol.

### The required new fund-safety suite (hard prerequisite, not optional)

**sdk48 — Model A pre-sign + stale-sender-tier race** (**landed as `sdk49_model_a_transfer`** — the
number sdk48 went to the V2-native-deposit suite; **sdk54** is its adversarial companion, attacking
the count this suite assumes honest): after Alice
transfers, assert (a) the **sender-pre-signed** `S'_{k+1}` pays **Bob** and matures strictly before
Alice's retained `S_k` (win the CSV race on `X_m.out[0]`); (b) `se_num_sigs` accounts for the full
disclosed bundle with **no hidden co-sign** (`== v1_backups + tiers.len()`, checked vs the live SE);
(c) Bob can unilaterally exit **at claim** with no further SE cooperation (L4, Model A — no
pending-ladder window). This is the end-to-end proof of the fund-safety core.

### Compat knob

Explicit selector on `ClientConfig` (`protocol_default: u8`, seeded from `UTEXO_PROTOCOL_DEFAULT`,
default 1) consumed at deposit/receive: `==2` ⟹ auto-establish (skip V1 backup generation);
`==1` ⟹ exact current behavior. `transfer_sender.rs:282` already selects `protocol_version=2` iff a
bundle exists, so the knob only controls *whether a bundle is created*.

> **DELETED.** The knob (and its `UTEXO_PROTOCOL_DEFAULT` env) was scaffolding for the phased flip
> and no longer exists in `SdkConfig`/`ClientConfig`. What survives is the half that was never a
> knob: the sender still selects the conveyance shape by asking whether a bundle exists
> (`tesr::load`), and `claim()` establishes one for every fresh confirmed root coin unconditionally.

### Order

- **Phase 0** — land the knob defaulting to 1. Matrix unchanged; require byte-identical PASS set to
  the pre-change baseline (`LOGDIR` `summary.txt` diff clean).
- **Phase 1** — author the bucket-A/B/C V2 replacements + **sdk48** at flag 2, V1 originals still at
  flag 1. Both green simultaneously.
- **Phase 2** — flip Group-D suites to flag 2 one at a time, running **both** flags per suite (add a
  `PROTO` env alongside `SDK_E2E` in `run_all_suites.sh::run()`), then drop the flag-1 run once the
  flag-2 run is green. Flip RGB (+chaos22) last with a full double-run.
- **Phase 3** — flip the global default to 2; re-run the whole matrix; delete the bucket-A/B/C
  originals now that replacements are green; any surviving V1-only suite is explicitly pinned flag 1.
- **Phase 4** (co-scheduled with V2DEF-6) — remove the flag-1 code path + the knob; delete V1-only
  suites; delete/freeze `UPSTREAM`; update `run_all_suites.sh` `discover()`/dispatch. `verify_bundle`'s
  `v1_backups` term goes to 0 for pure-V2 sats coins, so **sdk46/47 expected counts must change in
  lockstep** with the deposit-tier change (`se_num_sigs == tiers.len()`).

> **OUTCOME — Phases 0–4 all ran, with one correction.** The `v1_backups → 0` prediction was
> **wrong** and was not taken: Stage 2b never landed, so a laddered root coin still carries its one
> signed-once backup and the baseline is the constant `tesr::PARENT_V2_BASELINE = 1` (a split child
> slot, never funded on-chain, is `CHILD_V2_BASELINE = 0`). sdk46/47 kept counting
> `se_num_sigs == v1_backups + tiers`, and that conveyed-backup term is itself attacker-facing —
> **sdk55** exists because it was briefly left unvalidated on the laddered lane.

### Files touched / tests

`clients/tests/rust/src/main.rs`, `clients/tests/run_all_suites.sh`,
`clients/libs/rust-sdk/src/config.rs`, `clients/libs/rust/src/transfer_sender.rs`, and the bucket-A/C
suite files listed above; new `sdk48`. Honor the **shared-CWD `wallet.db` collision gotcha** — each
suite runs in an isolated CWD so the doubled Phase-2 matrix does not cross-contaminate. Budget CI:
Phase 2 doubles Group-D runtime (many slow live-SE/LN suites) — gate the double-run to a nightly matrix.

---

## Stage V2DEF-6 — delete V1 code + purge docs

> **OUTCOME — this stage ran, but its premise was corrected mid-flight and the excision is
> deliberately PARTIAL.** The pre-deletion unreachable-guard proof is what corrected it: the
> non-bundle receiver branch is **not** dead code. It is the UN-LADDERED shape — RGB carriers, which
> must never be laddered (a plain tier spend destroys the allocation), and split sub-coins whose
> un-broadcast funding cannot root a trigger [B0]. So the lane **stayed**, and with it
> `validate_signature_scheme`, `ladder_decrements_by_interval` (now the defence proven by **sdk55**),
> and `create_tx1`. What actually got removed is the thing that made two *protocols*: the
> `deposit_protocol_version` / `UTEXO_PROTOCOL_DEFAULT` escape hatch. `protocol_version` on the wire
> stopped being a protocol selector and became a **conveyance-shape discriminant** — `0` un-laddered
> backup-chain handover, `2` TES-R bundle, `3` legacy child conveyance, `4` child conveyance **with**
> the key handover. Read every "DELETE" below against the per-group notes.

Entry gate (all three must hold): (1) V2 is the default (sender always emits `protocol_version=2` +
`tesr_ladder`); (2) every E2E suite is migrated (V2DEF-5); (3) no V1 coins can be minted
(deposit→establish is the only path). **Pre-deletion proof:** run the full matrix with
`panic!`/assert-unreachable planted in `validate_signature_scheme` and the `protocol_version < 2`
receiver branch; a green run proves those paths are dead **before** excision. *(Gate (1) and (2) hold.
Gate (3) holds for root coins — every fresh confirmed root coin is laddered by `claim()`,
unconditionally. The pre-deletion proof is what disproved the assumption behind the excision: those
branches are reachable **by design**, for carriers and un-broadcast-funded sub-coins.)*

This is a **surgical excision, not a file-level `rm`.** The blind-MuSig2 co-sign engine, deposit
keygen, transfer key-rotation, off-chain split/combine PSBT builders, RGB coloring, withdraw, the
keyless watch bundle, and `get_backup_txs` persistence are **SHARED infra that TES-R itself calls**
(`lib/src/transaction.rs` co-sign primitives at `tesr.rs:527,535`) and **MUST survive.**

### DELETE — Group A (pure V1 ladder, lib) — **NOT EXECUTED**

> The whole group was cancelled by the pre-deletion proof. `validate_signature_scheme` is still
> called on the un-laddered lane, and `ladder_decrements_by_interval` (INV-5) turned out to be
> load-bearing on the **laddered** lane too: a laddered coin still conveys its signed-once backups
> and still feeds their count into `verify_bundle`, so INV-5 is what stops a sender padding or
> inverting that term — the two attacks in **sdk55**. `create_tx1` and `get_user_backup_address`
> likewise survive (Stage 2b was never taken). The "MOVE, don't delete" caution on
> `verify_blinded_musig_scheme` et al. was correct and is now moot — nothing was deleted.

- `lib/src/transaction.rs`: `get_unsigned_backup_tx` (zero callers — but it is a `#[uniffi::export]`
  binding; confirm no nodejs/web-wallet FFI consumer before deleting — open risk); `calculate_block_height`
  (collapse callers to `get_locktime_for_withdrawal_transaction`, then delete); `get_user_backup_address`
  (only caller is `create_tx1`).
- `lib/src/transfer/receiver.rs`: `validate_signature_scheme`, `ladder_decrements_by_interval`,
  `verify_if_locktime_is_reasonable_tx_version_and_output_size`, the R5 ladder tests. Delete
  `verify_transaction_signature`/`verify_blinded_musig_scheme`/`verify_transaction_sequence`/
  `reconstruct_transaction` **only if** `verify_bundle`/branch validation does not reuse them — **CAUTION
  (open risk):** `verify_blinded_musig_scheme`'s SE-challenge soundness check and the sequence/reconstruct
  canonical-form checks may be legitimately reusable by branch/tier validation; **MOVE, don't delete**, if so.

### DELETE — Group B (SDK on-chain refresh) — **NOT EXECUTED**

> The blocking product decision (below, "`refresh_sponsored` … blocks deleting sdk30/38") was
> resolved as **KEEP**. `refresh.rs` is intact — `refresh`, `refresh_sponsored`, `rebate_refresh_fee`,
> `reanchor`, `auto_refresh_due`, `auto_refresh_before_spend`, `RefreshResult` — as are the
> `auto_refresh*` config fields and `TransferQuote`'s `renewal_fee_sats`/`stuck_coins` (so the
> flagged API break never happened). **sdk30** and **sdk38** are live. Only the *laddered* lane is
> off it: a re-anchor of a laddered coin would strand its bundle, so that path renews off-chain
> instead. **D2** was fixed here (sponsored rebate sized into the dead window below
> `min_child_value`, failing after the user had paid the on-chain fee).

- `clients/libs/rust-sdk/src/refresh.rs` — **entire file** (`refresh`, `refresh_sponsored`,
  `rebate_refresh_fee`, `reanchor`, `auto_refresh_due`, `auto_refresh_before_spend`, `coin_near_final`,
  `RefreshResult`).
- `lib.rs`: remove `pub mod refresh;` + `pub use refresh::RefreshResult;`.
- `config.rs`: delete `auto_refresh`, `auto_refresh_margin_blocks`, `background_auto_refresh` + defaults.
- `transfer.rs`: delete the two `auto_refresh_before_spend()` calls and the renewal/re-anchor-fee
  arithmetic in `quote_transfer`; reduce `TransferQuote` to network fee only. **API break** (removes
  `renewal_fee_sats`/`stuck_coins`) — coordinate with web-wallet/nodejs bindings (open risk).
- `wallet.rs`: delete the `auto_refresh_due` background call. `tokens.rs`: fix the `auto_refresh_due`
  doc-comment.

### EDIT — Group C (deposit backup + generic tx flow lose ladder args) — **PARTIAL**

> `create_tx1` **stayed**: a laddered root coin still gets exactly one signed-once backup at deposit
> confirmation (`coin_status::check_deposit`), which is what pins the census baseline to a constant
> (`tesr::PARENT_V2_BASELINE = 1`) that a split-child receiver can rely on without observing the
> parent's history. The `initlock`/`interval` plumbing cleanups did happen where the laddered lane
> owns the flow; the un-laddered lane still uses them.

- `deposit.rs`: delete `create_tx1` (+ `coin.locktime` set). V2 deposit → `tesr.establish`.
- `clients/libs/rust/src/transaction.rs`: keep `new_transaction` (withdraw needs it) but drop
  `initlock`/`interval`/`qt_backup_tx`.
- `withdraw.rs`, `utils.rs`, `wallet.rs`: drop `initlock`/`interval` plumbing; keep
  `get_locktime_for_withdrawal_transaction` (withdrawal privacy locktime).
- `lib/src/transaction.rs`: drop `initlock`/`interval`/`qt_backup_tx` from the split/combine/backup
  PSBT builders + `get_partial_sig_request`; `rgb.rs` updates its calls.

### EDIT — Group D (protocol_version gates → unconditional V2) — **RE-AIMED**

> `verify_bundle` did **not** become unconditional and `tesr_ladder` did **not** become mandatory,
> because a carrier legitimately conveys no ladder. Instead the receiver **hard-selects on the
> conveyance shape**, which is exactly the anti-downgrade rule V2DEF-4 demanded: bundle ⟹ tier
> census; no bundle ⟹ backup-chain validation (`validate_backup_chain_v2` + INV-5, attacked in
> **sdk55**); child bundle ⟹ `verify_conveyed_child` / `verify_child_bundle`, with
> `protocol_version = 4` additionally requiring the key handover to complete before the coin is
> usable. Every value the census compares against is fetched **authoritatively** — the funding
> outpoint from chain, `num_sigs` from the live SE via `/info/statechain` — never from the message,
> which is what makes the shape selection safe. The negative test that shipped is therefore not
> "reject a message without a ladder" but "**a sender cannot pad or invert the count its shape is
> checked against**" — **sdk54**/**sdk55**. The same census also runs **pre-pay** on the SSP side
> before an irreversible Lightning leg (LIGHTNING.md §2).

- `clients/libs/rust/src/transfer_receiver.rs`: make `verify_bundle(...)` unconditional; delete the
  `else if statechain_info.num_sigs != backup_transactions.len()` V1 branch and the whole
  `if protocol_version < 2 { validate_signature_scheme(...) }` block. `tesr_ladder` becomes required
  (error if absent). **New negative test:** a message with `protocol_version` omitted/`<2` or missing
  `tesr_ladder` must now be **rejected**.
- `lib/src/transfer/sender.rs`: retire `create_transfer_update_msg` (the v0 wrapper) or default it to
  version 2; senders always call `..._with_branch(..., 2, Some(ladder))`.
- `lib/src/transfer/mod.rs`: **KEEP** `protocol_version`/`tesr_ladder` fields (the name stays); drop
  the serde-default-to-V1 semantics from docs.

### REWRITE (NOT delete) — Group E (deadline/watchtower, V1 absolute → V2 CSV)

`wallet.rs::{estimate_exit_cost, deposit_anchored_exit_deadline, auto_exit_due}` + `watchtower.rs`
`WatchEntry::{deadline_block, backup_tx, backup_locktime}`: the deposit-anchored `H_deposit+initlock`
deadline and the maturing-absolute-locktime backup sweep are V1. V2's exit-race is CSV-relative and
starts only on trigger. **The keyless bundle + `watch_pass` STRUCTURE is preserved (L4 linchpin)**; the
deadline is re-derived from the tier CSV schedule and the `backup_tx` sweep field removed. **Sequence
this rewrite BEFORE deleting `deposit_anchored_exit_deadline`** (open risk — else off-chain sub-coins
could be left with no watchtower deadline = fund-safety gap).

### TESTS

Delete `sdk13/14/26/27/30/33`, `tb05`, and the ladder/decrement assertions in `chaos22_{oracle,cheats}`;
migrate sdk10/sdk12 exact-count + `sdk12_adversarial`'s `get_partial_sig_request` calls to the V2 tier
count (already required by V2DEF-5); update **sdk46/47** to `se_num_sigs == tiers.len()` in the same
commit as the deposit-tier change. Delete V1-only suites in the same commit that removes their APIs so CI
does not compile-fail.

> **OUTCOME.** Deleted: `sdk13`, `sdk14`, `sdk26`, `sdk27`, `sdk33` (plus, from other buckets,
> `sdk03/05/06`, `sdk07/08/10`, `sdk18`, `sdk28`, `sdk35`). **Kept:** `sdk30` (refresh stayed a
> product feature) and `tb05` (re-expressed as tier count + relative CSV). `chaos22` was re-expressed,
> not gutted — it is the only concurrency suite and is live. `sdk12` migrated to the tier counts and
> is live; `sdk10` was deleted, its claims absorbed by **sdk50**/**sdk58**. `sdk46/47` were **not**
> changed to `se_num_sigs == tiers.len()` — see the Phase-4 note in V2DEF-5: the signed-once backup
> baseline stayed at 1. Every id deleted here is repointed in "Deleted test ids → where their claims
> live now" at the top of this file; the two entries with **no** repoint (`sdk26`/`sdk27` and
> `sdk28`/`sdk33`) are obsolete-by-construction, and the reason is recorded there rather than dropped.

### DOCS PURGE (keep only TES-R; retain the name "protocol version 2")

> **Executed by the 2026-07-29 docs sweep**, with the checklist below amended by what actually
> shipped: docs must describe **one protocol with two coin shapes**, never "V1/V2 coexisting" and
> never "the V1 lane" — the un-laddered shape is current and load-bearing for RGB carriers and
> un-broadcast-funded sub-coins. `refresh()`/`refresh_sponsored`/`auto_refresh` API docs are
> **kept**, not removed (the feature shipped). Every doc that cited a now-deleted test id had that
> citation repointed to a live suite, or the coverage claim removed outright where the property is
> obsolete — no doc may claim evidence that no longer exists.

- Rewrite V2-only: `SPEC.md`, `TRUST-MODEL.md`, `learn/{core-concepts,transfers,exits,trust-model,tldr}.md`
  — remove decrementing-ladder / initlock-interval / 7-day-refresh prose; fold `PROTOCOL.md` in as
  canonical.
- Remove/rewrite the finite-ladder rent model wholesale: `INVALIDATION-SPEC.md`,
  `learn/{invalidation,invalidation-deep-dive}.md`, `research/invalidation-economics.md` — V2 eliminates
  ladder rent ("idle coins never age, 0 vB rent"), so the invalidation-economics premise is void.
- Scrub V1 refs from `GRANULARITY-SPEC.md` + `learn/granularity-deep-dive.md` +
  `research/granularity-economics.md` (keep split/combine mechanics; drop ladder-locktime framing),
  `AUDIT-2026-07.md`, `PLAN.md`, `PROGRESS.md`, `PARITY.md`, `ARK-SPARK-PARITY.md`,
  `research/{protocol-notes,sdk-notes}.md`, `build/{wallet-sdk,api-reference,testing-guide}.md` (remove
  `refresh()`/`refresh_sponsored`/`auto_refresh` API docs). **Grep for cross-references and fix/stub
  dangling links; do not just delete files** (INVALIDATION/economics are cross-referenced by RGB and
  granularity docs).

### Fund-safety (each linchpin preserved by a substitution made BEFORE its V1 form is deleted)

- **L1** — delete `num_sigs == backup_transactions.len()` only because `verify_bundle` already enforces
  the strictly-stronger `se_num_sigs == v1_backups + tiers.len()`. **CAUTION:** `v1_backups` is really
  "non-tier SE co-signs" and still counts RGB-colored backups even after V1 is gone — do **not** assume it
  goes to 0; audit the count balances for a pure-V2 RGB coin, and consider renaming to `non_tier_cosigns`
  (open risk). **The caution was right.** `v1_backups` never went to 0 — it is 1 for every laddered
  root coin (`PARENT_V2_BASELINE`) and 0 only for a never-funded split-child slot
  (`CHILD_V2_BASELINE`), and the term is attacker-supplied, so it is validated in its own right by
  INV-5 (**sdk55**). The shipped equation also carries a `superseded_ok` term for the state each hop
  legitimately discloses; **sdk54** is the suite proving that term cannot be padded.
- **L2** — the absolute-`nLockTime` ladder check was **not** deleted: `verify_bundle` validates the
  decrementing-**relative**-CSV chain on the laddered shape, and `ladder_decrements_by_interval`
  still validates the conveyed signed-once backups on both shapes (same "latest matures first").
- **L3/L4** — exit MATERIAL (split/combine builders, `exit_tiers`, keyless bundle + `watch_pass`) is in
  the KEEP set; only the V1 absolute-locktime backup sweep is deleted, replaced by the tier chain
  broadcast. Non-custody untouched (none of the deleted code is on keygen/rotation/co-sign).

### Files touched

`lib/src/{transaction.rs,transfer/receiver.rs,transfer/sender.rs,transfer/mod.rs}`,
`clients/libs/rust/src/{deposit.rs,transaction.rs,transfer_receiver.rs,withdraw.rs,utils.rs,wallet.rs,rgb.rs}`,
`clients/libs/rust-sdk/src/{refresh.rs,lib.rs,config.rs,transfer.rs,wallet.rs,watchtower.rs,events.rs,tokens.rs}`,
`clients/tests/rust/src/`, `docs/utexo/`.

---

## Per-stage summary — files, coexistence/gating, fund-safety, live gate tests

Columns 2–4 are the plan-time view (kept as written). The last column is **as landed** — every id in
it is a suite that exists today.

| Stage | Core change | Coexistence / gate | Fund-safety preservation | Live tests that gate it (as landed) |
|---|---|---|---|---|
| **V2DEF-2** V2-native deposit | auto-establish + persist ladder at CONFIRMED, behind `deposit_protocol_version` (default 1) | flag 1 = byte-identical; flag 2 (2a) adds tiers over the unchanged signed-once backup (superset). *Flag since deleted — establish is unconditional* | L1 `verify_bundle` (baseline stayed at 1 backup + tiers); L4 seed-derived `backup_address`; L2 schedule CSV; L3 no enclave rebuild | **sdk48** (auto/payee/count/verify/transfer); idempotency/resume → **sdk56**; exclusion (carrier gets no ladder) → **sdk52** |
| **V2DEF-3** route wallet flows | dispatch on bundle-presence; Model A sender pre-sign; off-chain renew; `watch_pass`; withdraw | lands dormant (no bundle ⟹ un-laddered branch); flip = V2DEF-2 flag | L1 full-disclosure count (cross-hop safe); L2 `k+1` lower CSV; L4 at claim (no pending-ladder); F-race handled by confirm-before-spend | **sdk49** (Model A), **sdk50** (SDK unilateral exit), **sdk51** (watchtower), **sdk01** (cooperative withdraw), **sdk54/55** (census adversarial) |
| **V2DEF-4** RGB + LN on tiers | colored split/combine → v3/CSV/P2A; count-based terminal-freeze; carriers fresh-`F`; SE-authenticated dispatch; latch composition | format gated; sig-count-neutral (sdk02/09 unchanged) | L1 `num_sigs==1`/ancestor (fresh-`F`); terminal-freeze count sole gate; anchors in signed-once tx; L4 broadcast colored chain | **sdk52** (carrier ⊥ ladder), **sdk02/09/29/31/39** + **rgb01–14** (colored split/combine), **sdk32/34** (carrier watchtower), **sdk54/55**; LN = **sdk63/64/65/66/67/68** (+ **sdk23**, **tb04**, **sdk53** guard lifted) |
| **V2DEF-5** migrate ~50 suites | knob-gated per-run flip; replace A/B/C, flip D, add the Model-A proof | Phase 0 baseline-identical; Phase 2 double-run both flags; knob then deleted | L1 sdk46/47 vs live SE; L2 CSV form in **sdk50/51**; L4 at-claim exit in **sdk49** | whole matrix green on the one protocol; isolated CWD per suite |
| **V2DEF-6** delete the escape hatch + purge docs | partial, surgical excision: the flag goes, the un-laddered shape STAYS; watchtower rewritten; docs purged | unreachable-guard run showed the non-bundle branch is reachable **by design** (carriers, un-broadcast-funded sub-coins) | L1/L2 substituted by `verify_bundle` + INV-5 before anything was removed; L3/L4 material in KEEP set | **sdk40–68** green; **rgb01–14**; split/combine **sdk29/31/39/58/59**; **sdk60/17** (child hops); withdraw **sdk01**; LN **sdk63–68**; `cargo build -D warnings` |

---

## Open risks & the order of operations

### Order of operations (dependency-correct)

1. **Pin the exact `num_sigs` definition** (finalized co-signs only; rotation handshake and aborted
   latch batches count zero) — a precondition for L1 across V2DEF-3/4/5. Verify in server/enclave; test
   with the latch-abort regression.
2. **V2DEF-2 Stage 2a** (auto-establish behind flag, `tx1` retained) + the **resumable-establish
   atomicity** fix.
3. **V2DEF-3** routing (dormant), including Model A sender pre-sign and the **Group-E watchtower
   rewrite** — the rewrite must land **before** any deposit-anchored-deadline removal.
4. **V2DEF-2 Stage 2b** (drop `tx1`) — only after step 3's exit paths drive the tier bundle.
5. **V2DEF-4** RGB/LN (carriers fresh-`F`, count-based terminal-freeze, SE-authenticated dispatch,
   P2A vout fix, TRUC exit mitigations); disable new V1 colored issuance at its first stage.
6. **V2DEF-5** test migration Phases 0–3; land **sdk48** (Model A fund-safety proof) in Phase 1.
7. **V2DEF-2 Stage 2c** (flip default to 2) — after suites migrated and atomicity fixed.
8. **V2DEF-6** delete V1 + docs (Phase 4), after the pre-deletion unreachable-guard run is green.

> **DONE, in this order, with two documented deviations:** step 4 (Stage 2b, drop the signed-once
> backup) was **not taken** — the baseline constant is better than a zero — and step 8 became a
> partial excision that removed the protocol-selecting flag while keeping the un-laddered shape. The
> Model-A proof landed as **sdk49**. Two things not in this list turned out to be prerequisites and
> were built along the way: the **in-ladder split** (B1's real fix — sdk58/sdk59) and **first-class
> children** via the standard key handover (sdk60/sdk17), without which the last suites could not
> come off the old lane.

### Accepted adversarial findings (folded in)

- **FATAL (receiver adoption): claim-time exit-gap under Model B** → **Model A adopted** (sender
  pre-signs the receiver-paying state under the old epoch; receiver verifies via R′). Eliminates the
  gap, drops `RECEIVED_PENDING_LADDER`, and removes O-1 from the critical path.
- **FATAL (receiver adoption): RGB anchor loss under adoption** → dissolved by Model A (sender
  constructs and anchors the tx Bob broadcasts; hands over the matching consignment).
- **FATAL (RGB/LN): hidden colored double-spend via `num_sigs` conflation** → **carriers born fresh-`F`
  (`v1_backups==0`)** so colored count == `num_sigs`, unambiguous.
- **FATAL (RGB/LN): boolean terminal-freeze insufficient** → **count-based check is the sole colored
  accept gate**; new V1 colored issuance disabled at the first stage.
- **Serious (RGB/LN): validation-mode downgrade** → dispatch bound to **SE-authenticated metadata**,
  never sender fields.
- **Serious (RGB/LN): TRUC sibling griefing + token-only CPFP** → multi-owner splits confirmed at
  handover; tower-funded P2A bump / documented `committed_fee` ceiling.
- **Serious (both): `num_sigs` decomposition / failed-latch poisoning** → the pinned finalized-only
  definition (step 1).
- **Fixable (RGB/LN): P2A vout filter bug** → exclude both `is_op_return()` and `p2a_script()` at all
  three `rgb.rs` sites; assert `nVersion=3` round-trips.
- **Fixable (both): same-CSV race, resumable establish, extension-`H_tag` assertion, atomic wire
  migration, committed-fee+P2A min-output retune** → folded into the relevant stages.

### Rejected / downgraded findings

- **Rejected: the (i)+(ii) counter-based R′ formulation** (input receiver-adopt) — the review is
  correct that it is unsound under a blind SE (underdetermined count; declared-`k` decoupled from real
  CSV). Superseded by **full-disclosure counting**, which needs no counter.
- **Downgraded: the O-1 enclave counter machine — from HARD BLOCKER to optional optimization.** Under
  Model A + full-disclosure counting, default-V2 (including multi-hop, renewal, rollover) is sound with
  **no enclave change**. O-1 becomes relevant only if compact bundles are later desired, and would then
  require per-tier key tweaks (`H_tag(k)`) to bind position to key under blindness — explicitly out of
  scope here.
- **Rejected: Model A's stated weakness ("safety depends on the sender's honest CSV choice")** — false;
  R′ verifies the CSV. (Per the review.)

### Residual risks — status after execution

- **O-2 (`δ` vs congestion): STILL OPEN.** Size `d_floor`/`δ` so the receiver's `k+1` state wins the
  maturity race under mainnet fee spikes; gates the near-floor re-anchor threshold. Nothing in the
  regtest matrix can settle a mainnet fee-spike margin.
- **Bundle growth on hot coins: bounded, priced.** The re-anchor at the floor resets the bundle, and
  the transfer fee model (B4 refresh-as-fee) survived the migration, so `quote_transfer` still
  surfaces the renewal component to the user.
- **`v1_backups` naming/audit: RESOLVED as an audit, still un-renamed.** It never went to 0 — the
  balances are pinned as `PARENT_V2_BASELINE = 1` / `CHILD_V2_BASELINE = 0`, the term is validated by
  INV-5 (**sdk55**), and the shipped equation adds `superseded_ok` (**sdk54**). The `non_tier_cosigns`
  rename was not done; the name still reads as if it counted a dead protocol.
- **Shared-infra reuse in Group A deletion: MOOT** — Group A was not executed;
  `verify_blinded_musig_scheme` and friends were never moved or deleted.
- **FFI/API breaks: DID NOT HAPPEN.** `get_unsigned_backup_tx` is still exported and `TransferQuote`
  kept `renewal_fee_sats`/`stuck_coins`, so the nodejs daemon and web-wallet bridge needed no
  coordination.
- **Server/enclave `num_sigs` semantics: CONFIRMED SUFFICIENT** for the shipped equation (no enclave
  rebuild was needed), and the signed-once backups never stopped being created, so the scenario this
  risk worried about did not arise. The **SGX lane remains the standing caveat**: every suite named in
  this document runs against the dev lockbox, not the production enclave.
- **`refresh_sponsored` product decision: RESOLVED — KEEP.** sdk30/38 stay; **D2** (rebate sized into
  the dead window below `min_child_value`) was found and fixed as a consequence.
- **Still uncovered, carried forward from V2DEF-4:** token-only CPFP under a fee spike (a pure-token
  wallet with no sats to bump P2A) and the multi-owner colored split's TRUC sibling ordering. Both
  mitigations are specified above; neither has a dedicated live suite.
