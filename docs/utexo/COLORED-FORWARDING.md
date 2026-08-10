# Colored forwarding — why every RGB recipient is a leaf, and what to do about it

Status: design decision, 2026-07-30. Supersedes nothing; amends `PROTOCOL.md` §5.10 (see §3.8).
All `file:line` references are against the working tree at the time of writing. Comments in the
tree are being rewritten by a concurrent process — **trust the code, not the comments**.

---

> ## ⛔ RETIRED, 2026-08-10 — WCH WILL NOT BE BUILT. §1 STILL STANDS.
>
> **Owner decision.** Read this before anything below it, because the recommendation and the
> analysis have different fates and it matters which one you are quoting.
>
> **Dead: §2.1 and §3 — the recommendation to build WCH.** It was never built. The identifiers this
> document names have zero occurrences anywhere in `clients/`, `server/` or `lib/`:
> `colored_whole_transfer`, `WCH_SAFETY_MARGIN`, `min_backup_output`. Do not resurrect them.
>
> **Why the recommendation expired rather than being rejected.** §2.1 offered WCH as the cheap
> alternative to the coloured ladder, and §2.2 gated that ladder on "*if and only if* partial
> forwarding and real unilateral exit are worth a quarter of engineering". **CTES-R was subsequently
> built** — the gate passed against the live stack ([CTESR-GATE.md](CTESR-GATE.md)) and `sdk75` is
> the first genuine unilateral exit of an RGB allocation. So the either/or this recommendation rests
> on no longer exists, and the expensive branch is the one that happened.
>
> Two things follow, and the second is the decisive one:
>
> * **Whole-carrier conveyance already exists on the coloured lane** — `build_colored_receiver_state`
>   (`clients/libs/rust/src/tesr.rs:1903`), named in the code's own refusal text as the way "to convey
>   the whole carrier". Onward forwarding is `transfer_colored_child`
>   (`clients/libs/rust-sdk/src/tokens.rs:4332`). WCH's *mechanism* is not missing; only its
>   legacy-lane spelling is.
> * **WCH would never have delivered unilateral exit.** §3.8 preserves terminal-freeze *by design* —
>   "WCH never ladders a carrier". Building it would have committed the protocol to "RGB carriers
>   have no SE-free exit" as a permanent property. CTES-R is the only thing that retires that.
>
> **Alive and unaffected: §1.** The proof that the legacy coloured lane makes every RGB recipient a
> structural leaf — a piece minted at exactly `TOKEN_PIECE_SATS` can never exceed
> `TOKEN_PIECE_SATS + fee_reserve`, unsatisfiable *for every value of the constant* — is correct,
> load-bearing, and still cited as background by `PROTOCOL.md:21`, `SPEC.md:22`,
> `GRANULARITY-SPEC.md:18` and `CTESR-GATE.md:11`. Those citations remain valid.
>
> **What this decision does NOT do.** It does not fix that defect. The refusal at
> `clients/libs/rust-sdk/src/tokens.rs:3325` is still a bare refusal with no whole-carrier fallback,
> so for as long as `SdkConfig::colored_ladder` ships `false` the structural-leaf defect is LIVE in
> the shipped lane. Skipping WCH is a bet that CTES-R becomes the normative RGB lane. If that bet is
> ever reversed — see `SPEC-ROADMAP.md` **D1** — this document is where the cheap fix is written up,
> and it should be un-retired rather than rewritten.
>
> §§2.2, 4 (staged plan), 5 (rejected alternatives), 6 and 7 are historical: read them as the
> reasoning that produced CTES-R, not as work items. §4.1's `claim()` laddering hole is a real,
> separate defect and is **not** retired by this decision.

---

---

## 1. The problem

`colored_transfer` (`clients/libs/rust-sdk/src/tokens.rs:501-763`) ALWAYS performs a colored split.
The recipient's piece is minted at exactly `TOKEN_PIECE_SATS = 1_500`
(`clients/libs/rust-sdk/src/tokens.rs:23`), and the split is refused unless the carrier is strictly
larger than the piece plus a burned reserve — `fee_reserve = (carrier_sats / 100).clamp(300, 2_000)`,
refuse if `TOKEN_PIECE_SATS + fee_reserve >= carrier_sats` (`tokens.rs:558-565`). Alice deposits a
3,000-sat carrier holding an RGB allocation and pays Bob: `1500 + 300 < 3000`, so the split
proceeds, Bob receives a piece of **exactly** 1,500 sats and Alice keeps 1,200. Bob then tries to
pay Carol: `1500 + 300 >= 1500` — refused, *"carrier coin too small (1500 sats) for a token
split"*. Because a recipient always receives **exactly** `TOKEN_PIECE_SATS` while forwarding
requires **strictly more** than `TOKEN_PIECE_SATS + fee_reserve`, and `fee_reserve >= 300 > 0`, the
inequality `TOKEN_PIECE_SATS + fee_reserve < TOKEN_PIECE_SATS` is unsatisfiable **for every value of
the constant** — raising 1,500 to 5,000 or 50,000 changes nothing, because the received piece is
minted at the same constant it must exceed. Every RGB recipient is therefore structurally a LEAF,
after exactly one hop, forever; `sdk32_token_over_time.rs:220-224` pins this as expected behaviour.
The only escapes today are receiving a *second* carrier and combining (sdk31) or exiting on-chain.

---

## 2. Recommendation

**Ship the cheap fix now. It does not reach full Spark parity, and the expensive fix does not
reach it either.** Both, honestly:

### 2.1 Now — WCH, Whole-Carrier Handover (~1 week + guards)

Add a `token_amount == carrier_amount` **fallback** inside the existing refusal at
`tokens.rs:558-565`: when the split is impossible and the sender is forwarding the *whole*
allocation, skip the split entirely and do an ordinary statechain key handover of the carrier coin
itself. The carrier UTXO does not move, so the RGB seal is never closed and the allocation cannot be
destroyed. The receive side needs **no** change — `accept_incoming_tokens` (`tokens.rs:1262-1348`)
keys on "some backup row carries an envelope", derives branch txids from `branch-<statechain_id>`,
and books at the coin's own funding outpoint; all three are inherited byte-identically by a
whole-carrier hop.

This fixes the reported Bob→Carol failure exactly, at **zero sats locked and zero sats burned per
hop**, and turns "1 hop, structurally" into "tens of hops per epoch". It does **not** fix partial
forwarding out of a minimal piece — that is a separate, smaller change (§3.7, denominations).

WCH is **not** free of prerequisites. Three defects found during review must land first or with it
(§4, stages 0–1); one of them (the `claim()` laddering hole, §4.1) is a silent asset-destruction
primitive that WCH would otherwise arm.

### 2.2 Later — colored ladder (CTES-R), if and only if partial forwarding and real
unilateral exit are worth a quarter of engineering

Colouring every TES-R tier (T, X, S all carrying an opret transition) retires terminal-freeze and is
structurally sound — the SE is provably RGB-blind and the anti-theft census is arithmetically
invariant under colouring. It buys the first genuine **unilateral colored exit** (today
`unilateral_exit` refuses carriers outright, `clients/libs/rust-sdk/src/wallet.rs:1039-1050`) and
in-ladder colored splits. It also **raises** the minimum splittable carrier ~2.6× (≈4,800 sats at
2 sat/vB vs 1,800 today), **removes off-chain colored combine**, needs an rgb-lib fork primitive,
and requires a per-tier seal-blinding fix without which rival tiers collapse to the same `OpId` and
hop 2 fails opaquely. Estimated 11–16 engineer-weeks. Do not start it without the two experiments
in §6.

### 2.3 The part nobody can fix off-chain

Every off-chain colored subtree hangs off one confirmed on-chain root whose original depositor
retains a backup maturing at `h_deposit + lockheight_init` — 1,000 blocks ≈ **6.9 days** on the
deployed profile (`server/Settings.toml:2-3`), and `validate_branch` re-checks the root unspent and
confirmed at every claim. `refresh` refuses carriers outright
(`clients/libs/rust-sdk/src/refresh.rs:150-157`), and `refresh_rgb_anchor_self_transfer`
(`clients/libs/rust/src/rgb.rs:494-660`) has **zero callers** and *consumes* a rung rather than
restoring one, so it is not the renewal it looks like.

This deadline is a property of on-chain state (an unspent root, a maturing absolute-locktime tx).
**Only an on-chain transaction can change an on-chain fact.** No design in this document — and no
design that keeps the current root model — removes it. The honest ceiling for RGB on Utexo is:

> **unlimited off-chain hops within a ~7-day epoch, then one on-chain transaction per carrier tree
> per epoch.**

Spark's bar is "unlimited, ever, no per-transfer on-chain cost". We can reach *no per-transfer*
on-chain cost. We cannot reach *no* on-chain cost without a colored re-anchor primitive, and that
primitive is itself an on-chain tx. Amortised across a wide tree the per-user-visible-transfer
on-chain cost goes to zero, which is the strongest true claim available. Say that, not more.

---

## 3. The chosen design — WCH (Whole-Carrier Handover)

### 3.1 Transactions

A whole-carrier hop creates **exactly one** new transaction: the ordinary receiver-paying backup
`B_k` that `transfer_sender::create_backup_transactions` already builds for every transfer
(`clients/libs/rust/src/transfer_sender.rs:130-178`). Un-broadcast, nVersion 2, one P2TR output,
absolute nLockTime one `interval` below the previous backup.

Nothing else is built. Specifically NOT built: a colored split tx, a new RGB transition, a new
consignment, new sub-coin `tx1`s, new derived statechain slots.

### 3.2 What each transaction spends

`B_k` spends the carrier's funding outpoint — the same outpoint every previous backup
`B_0..B_{k-1}` spent. This is the replacement model: `create_tx_out` recomputes
`amount_out = coin.amount − ceil(112 × fee_rate)` from the coin's **funding** amount every time
(`lib/src/transaction.rs:109-152`), so the frozen fee does **not** compound across hops — `B_1` and
`B_50` have identical output value.

### 3.3 What carries the RGB transition

**Nothing new, and that is the point.** The only transition in play is the one inside the original
colored split tx that minted the carrier, assigning `A` to the seal `split_txid:piece_vout`. That
seal is never closed off-chain by any hop. The `ConsignmentEnvelope { c, a, s }` written onto the
piece's first backup row is forwarded byte-identically hop after hop, because
`create_backup_transactions` clones every existing row whose previous-outpoint matches the coin
(`transfer_sender.rs:139-148`) — including row 0, which carries `rgb_consignment` — and only the
NEW row gets `rgb_consignment: None` (`transfer_sender.rs:167`). The exit branch (`branch-<id>`) and
the terminal-ancestor list (`parents-<id>`) are likewise re-conveyed by the sender and re-persisted
by the receiver (`transfer_receiver.rs:1034-1094`).

Consequence: consignment size, branch depth and `validate_branch` cost are **frozen at mint-time
values for the carrier's whole life**. Each hop is O(1), not O(depth). This is a structural
advantage over any split-per-hop design, which would wall out at the 25-tx mempool ancestor limit.

### 3.4 What the SE signs

One blind MuSig2 partial signature over `B_k`'s taproot sighash. The SE remains fully RGB-blind:
`grep -rin 'rgb\|consignment\|colored' server/src lockbox/src enclave` yields four comments and zero
code. It sees an ordinary transfer of an ordinary coin. **No server, lockbox or enclave change.**

`set_spend_budget(carrier, 1)` (as at `tokens.rs:614-620`) must be **SKIPPED** on this path.
Terminalising the carrier bricks the next hop, and there is nothing to terminalise against — no new
colored tx exists that anyone could rival. Note `set_sig_budget` is monotone-tightening and
irreversible (`server/src/database/deposit.rs:228-246`), so a carrier left capped by a failed
earlier split is permanently one-shot; that must be pre-checked (§3.6, guard (d)).

### 3.5 What the receiver verifies

Everything it verifies today, unchanged, and nothing new:

1. `verify_latest_backup_tx_pays_to_user_pubkey` — `B_k` pays the receiver's key.
2. `verify_if_locktime_is_reasonable_tx_version_and_output_size` on every conveyed backup — nVersion
   2, ≤1 OP_RETURN, `tip < lock_time ≤ tip + initlock`; and the exact-decrement rule
   `ladder_decrements_by_interval`, satisfied because `L_k = L_0 − interval·k`.
3. `num_sigs == backup_transactions.len()` (`clients/libs/rust/src/transfer_receiver.rs:721`) — the
   old-state-invalidation census. Each hop appends exactly one backup row and consumes exactly one
   SE co-sign, so exact equality is preserved at every `k`. A sender who secretly obtained an extra
   lower-locktime co-sign shows `num_sigs > len` and is rejected.
4. `validate_branch` over the inherited branch — tree shape, root on-chain/unspent/confirmed, INV-4
   `lock_time <= tip` on branch txs, non-negative fee, script verification. Identical input,
   identical result.
5. `verify_terminal_parents` against the inherited ancestor set. Terminality is monotone (a
   budget-exhausted parent stays exhausted), so this passes at every hop with no new terminalisation.
6. `accept_incoming_tokens` — `validate_offchain_chain_info(env.c, txids)`, then
   `accept_offchain_amount(env.c, txids, coin.utxo_txid, coin.utxo_vout)`, then `booked == env.a`,
   then `import_asset_offchain` + `register_statechain`. The tuple (envelope, branch txids, coin
   outpoint) is bit-identical to hop 1, so it books the same amount at the same seal.

### 3.6 How old state is invalidated at every hop

By the un-laddered lane's own two mechanisms, unchanged:

* **Decrementing absolute locktime.** `L_k = L_0 − interval·k` (`lib/src/transaction.rs:154-169`).
  A prior owner's retained backup has a strictly *higher* locktime than the current owner's, so it
  matures later and loses the race.
* **Exact census.** `num_sigs == backups.len()`. One co-sign, one row, per hop, forever.

Honest caveat, stated because it is real: a prior owner cannot **steal** the allocation (their
backup is uncolored — broadcasting it burns the token and nets them ~1,388 sats) but they **can
destroy** it once their locktime matures. Stale backups mature at `L_0 > L_0−10 > … > L_{k-1}`, so
the *earliest*-maturing threat is always the immediate predecessor, one `interval` (10 blocks) ahead
of the current owner's own maturity. The threat is therefore dominated by one party at any depth,
not by `k` accumulated parties — but the *headcount* of parties who can burn the asset for free does
grow with `k`, and the current holder has **no shipped counter-move**: `unilateral_exit` refuses
carriers (`wallet.rs:1039-1050`) and `auto_exit_due` materialises the branch only.

**This is why guard (b) below is mandatory and must carry a real margin, not zero.**

**Guards — all must run BEFORE any SE round-trip.** Today every one of these fails only *after* a
co-sign, stranding the carrier and consuming a rung.

* **(a) Fee/dust.** `carrier_sats − ceil(112 × backup_fee_rate) >= 330`, mirroring `create_tx_out`
  (`lib/src/transaction.rs:120-131`). A 1,500-sat piece hits this at ≈10.4 sat/vB.
* **(a′) Fee-rate band — the constraint that usually binds first, and it is two-sided.**
  `verify_transaction_signature` rejects with `FeeTooLow` when `(fee_rate + tolerance) <
  current_fee_rate` **and** with `FeeTooHigh` when `(fee_rate − tolerance) > current_fee_rate`
  (`lib/src/transfer/receiver.rs:465-478`), with `fee_rate_tolerance` defaulting to 5.0
  (`clients/libs/rust/src/client_config.rs:66`), and it runs on **every** conveyed row — including
  row 0, whose fee was frozen at mint time and can never be rebuilt. So the inherited chain is only
  claimable while `|current_rate − mint_rate| <= 5 sat/vB`. Mint at 20, hold three days, rate falls
  to 5 → the receiver's claim fails `FeeTooHigh` and the coin strands `IN_TRANSFER`. Recover row 0's
  implied rate and refuse the hop up-front on mismatch.
* **(b) Predecessor-burn margin.** Let `D_burn = min(locktime over the carrier's EXISTING backup
  rows)` = `L_{k-1}`. Refuse unless `D_burn − tip >= WCH_SAFETY_MARGIN`. **Recommend ≥ 144 blocks
  (~1 day)** — enough for the receiver to claim, accept the consignment, and act. A naive
  `L_k > tip` check would legally hand a receiver a carrier whose allocation anyone can destroy in
  11 blocks. This caps WCH at roughly `(initlock − 144)/interval ≈ 85` hops from mint and, more
  importantly, makes the *tail* hops refuse rather than deliver a doomed asset.
* **(c) Root deadline.** Refuse if `tip + auto_exit_margin_blocks >= exit_deadline_block`, so a hop
  is never handed to a receiver who must immediately materialise.
* **(d) Terminality pre-flight.** `GET /statechain/spend_budget/<carrier_id>`; refuse with a typed
  error if terminal. A carrier left over from a failed `colored_combine_transfer` is permanently
  capped by the monotone `set_sig_budget`.
* **(e) No ladder.** `tesr::load(cc, wallet, carrier_id)?.is_none()`. Never let
  `transfer_sender::execute`'s `tesr::load` branch silently upgrade a colored hop to
  `protocol_version = 2` — see §4.1.

### 3.7 Partial forwarding — denominations, not a smarter split

The prompt's suggested escape ("convey a colored split that is built but left un-broadcast") does
not work, because **the split is already un-broadcast** — `create_colored_split_tx` produces a
signed tx that is never broadcast; the split lives entirely off-chain and is materialised only on
exit. Broadcast was never the cost. **Sats are.** Each sub-coin must be a real funding output ≥
`min_split_output = DUST_LIMIT + ceil(112 × fee_rate)` (442 at 1 sat/vB), plus a burned
`fee_reserve`. The un-broadcast trick buys nothing not already banked.

What does work is **carving several denominated pieces in one split**. `create_colored_split_tx`
already takes a `Vec<SplitOutput>` and `register_split_subcoins_n` already returns N ids —
`colored_transfer` simply never uses N > 2. Emit pieces at token amounts 1, 2, 4, …, 2^(K−1) (or a
fixed denomination set) in ONE split, one SE co-sign, one branch hop, one consignment. The recipient
then pays any subset-sum by whole-carrier handover (§3.1–3.6) and uses `colored_combine_transfer`
for the remainder. This is Spark's leaf-denomination model, and it converts "sats burned per hop"
into "sats locked per denomination", re-usable across unlimited hops.

Cost, plainly: `K × TOKEN_PIECE_SATS` locked plus ONE `fee_reserve`. A 3,000-sat carrier still
affords only K = 1; K = 4 needs ≥ 6,300 sats. Denominations do not conjure sats.

### 3.8 Documentation impact

`PROTOCOL.md` §5.10's terminal-freeze rule ("RGB carriers are NEVER laddered") stays **true** — WCH
never ladders a carrier. What changes is the corollary readers draw from it, that a carrier is a
leaf. Add: a carrier is re-transferable **as a whole** on the un-laddered absolute-locktime lane,
bounded by `min(ladder rungs, root deadline, fee-rate band)`.

---

## 4. Staged plan

Each stage is independently landable, independently testable, and ordered by dependency. **No stage
touches `enclave/` or `lockbox/`.** The SE is RGB-blind and its three co-sign gates (`single_use`,
`sig_budget`, `epoch_deadline`, `server/src/endpoints/sign.rs:88-140`, re-checked at :285-300) are
already correct for this work — which is the single best property of this plan, given the known
SGX-lane-untested gap. **If any future change requires the SE to distinguish a colored tx from a
plain one, stop and re-cost: that is the expensive, historically-broken-on-one-lane path.**

### Stage 0 — close the `claim()` laddering hole (MANDATORY PREREQUISITE) — lockbox-only

`claim()` runs the TES-R auto-establish loop at `clients/libs/rust-sdk/src/wallet.rs:451-521`, but
`book_incoming_token` → `accept_incoming_tokens` → `register_statechain` runs only afterwards
(:532, :555). So during the ladder loop a freshly received carrier is not yet in
`unspendable_as_btc_outpoints()` and `is_token_carrier` returns **false**. The only thing saving
this today is the ROOT-ONLY `f_on_chain` guard (:496-504): every carrier a receiver sees today is a
split sub-coin funded by an un-broadcast split tx, so `transaction_get` fails and the loop skips it.

`auto_exit_due` (default ON) **materialises** received carriers near their deadline, putting the
funding tx on-chain. A materialised carrier forwarded by WCH would hit `f_on_chain == true`,
`is_token_carrier == false`, `tesr::load == None` → **`establish_auto` ladders an RGB carrier**, which
(i) co-signs an un-timelocked trigger `T` spending `F` (verbatim [B1]) that every past holder keeps
forever, (ii) violates terminal-freeze so every subsequent exit burns the asset, and (iii)
propagates silently, because `transfer_sender::execute` sees the ladder and flips the next hop to
`protocol_version = 2`.

**Change:** in the ladder loop, before `f_on_chain`, skip any coin whose backup rows carry an
`rgb_consignment` (`get_backup_txs(sid).iter().any(|b| b.rgb_consignment.is_some())`) — authoritative
at claim time, no RGB round-trip. Belt-and-braces: move the `book_incoming_token` loop above the
ladder loop, keeping the fail-CLOSED `None` behaviour. Add a receiver-side refusal: a transfer
arriving with both `protocol_version >= 2` and an `rgb_consignment` on any backup row is never
legitimate — reject in `validate_encrypted_message`.

**E2E (sdk70):** materialise a received carrier, forward it, assert the receiver has **no** ladder
(`tesr::load == None`) and that `unilateral_exit` still refuses the coin.

### Stage 1 — rgb-lib fork: seal re-entry and re-registration — lockbox-only (fork repin)

Two defects that WCH makes reachable and that silently lose tokens:

* **D1 — spent flag survives re-registration.** `mark_utxos_spent` sets `spent = true`;
  `register_statechain_utxo` sets `spent: Set(false)`, but `set_txo`'s `ON CONFLICT (Txid, Vout)`
  update list contains only `Exists` and `BtcAmount` — **never `Spent`**. Under WCH the carrier
  outpoint is stable and circulating, so A→B→A round trips are normal, and on the return hop
  `accept_incoming_tokens` succeeds completely while `list_allocations` (via `list_unspents`, which
  excludes spent txos) never surfaces the allocation. Balance silently does not increase; the tokens
  are unspendable; no error anywhere. **Fix:** add `Spent` to the on-conflict update set when the
  incoming value is `false`, or have `register_statechain_utxo` explicitly clear it when `get_txo`
  finds a pre-existing row.
* **D2 — `mark_spent` on an unclosed seal.** In the split lane `mark_spent` is truthful (a real
  transition closed the seal). A WCH hop must not call it eagerly on a seal nothing closes: if the
  transfer does not complete (receiver never claims, rollback, crash), the carrier returns as a
  spendable coin whose allocation `list_allocations` will never surface again, and **no SDK path
  re-registers a coin the wallet already owns**. **Fix:** either defer `mark_spent` until the
  transfer is observed complete, or add an SDK `reregister_carrier(statechain_id)` that re-runs the
  `accept_incoming_tokens` body for a self-owned coin, called from `update_coins` reconciliation
  whenever a consignment-bearing coin is CONFIRMED but absent from `list_allocations`. The
  reconciliation path is preferable — it also repairs the crash window.

**E2E (sdk71):** A→B→A round trip; assert A's balance recovers and A can re-send. Kill the sender
process between `execute` and `mark_spent`; assert reconciliation restores the allocation.

### Stage 2 — WCH itself — lockbox-only

* `clients/libs/rust-sdk/src/tokens.rs` — replace the refusal at :558-565 with a **fallback**, not
  a fastpath:

  ```rust
  if TOKEN_PIECE_SATS + fee_reserve >= carrier_sats {
      if token_amount == carrier_amount {
          return self.colored_whole_transfer(
              asset_id, receiver_address, carrier, carrier_id, carrier_sats, latch,
          ).await;
      }
      return Err(anyhow!("carrier coin too small ({carrier_sats} sats) for a token split"));
  }
  ```

  **Placement is load-bearing.** As a *fastpath* placed before the guard, WCH would pre-empt the
  split whenever `token_amount == carrier_amount` regardless of carrier size — and `issue_token`
  funds the issuance carrier with 10,000 sats and binds the full supply to it, so
  `transfer_tokens(asset, bob, supply)` would hand Bob the entire 10,000-sat carrier instead of a
  1,500-sat piece: a silent 6.7× sats give-away, with `total_sats` 1,500 → 10,000 and `used_split`
  true → false. As a *fallback* it fires only when the split is impossible, existing behaviour is
  byte-identical, and the test blast radius is zero — `used_split == true` is asserted in sdk02,
  sdk04, sdk12, sdk16, sdk29, sdk32 and sdk52 and all keep passing.
* New `async fn colored_whole_transfer(...)`, ~70 lines: require an envelope (else a typed refusal —
  an issuance-origin carrier has no consignment to forward and there is no export primitive; those
  carriers are large enough for the split anyway); run guards (a), (a′), (b), (c), (d), (e); the
  existing latch match bound to `carrier_id`; `transfer_sender::execute(...)`; deferred
  `mark_spent`; return `ColoredTransferOut { used_split: false, piece_id: carrier_id, .. }`.
  Explicitly do NOT call `set_spend_budget`, `take_derived_tokens`, `create_colored_split_tx`,
  `register_split_subcoins`, or touch `branch-<id>` / `parents-<id>`.
* `clients/libs/rust-sdk/src/transfer.rs` — add `min_backup_output(fee_rate)` next to
  `min_split_output` so guard (a) shares one definition.
* `clients/libs/rust-sdk/src/tokens.rs` — `accept_incoming_tokens`: **no code change**, one comment
  naming the whole-carrier lane so a future reader does not "fix" its split-agnosticism away.

**E2E (sdk72):** Alice (3,000-sat carrier) → Bob (1,500-sat piece) → Carol → Dave. Assert balances
move; `used_split == false` for hops 2–3; the conveyed branch txid set and the consignment bytes are
**identical** at every hop; `num_sigs == backups.len()` holds; Dave can materialise the branch. Then
hop until guard (b) fires with a typed error rather than a co-sign failure.
**Also add to sdk32:** the existing assertion at :220-224 (partial re-send of 100 from a 250-token
piece fails) stays TRUE and must NOT be relaxed. Add the positive assertion next to it: Bob
forwarding all 250 to Carol must SUCCEED.

### Stage 3 — deadline correctness — lockbox-only

`deposit_anchored_deadline` is deliberately `k`-unaware (open audit item [17]) and is therefore LATE
by `k · interval` blocks. At `k = 1` that is 10 blocks and ignorable; at `k = 85` it is 850 blocks —
most of the epoch — so `auto_exit_due` would fire after the root is already sweepable. Make the
deadline for any coin with more than one backup row
`min(deposit_anchored_deadline(h_deposit, initlock), lowest_stored_backup_locktime)` and re-derive
`auto_exit_margin_blocks` (documented as covering only `k <= 14`). Correct the comment claiming
`h_deposit + initlock` is "a safe (early) bound over all ancestors" — it is false from WCH hop 2 for
a same-session mint.

**E2E (sdk73):** drive a carrier to `k ≈ 50`; assert `exit_deadline_block` tracks the lowest backup
locktime and that `auto_exit_due` fires before the earliest predecessor maturity.

### Stage 4 — denominations (optional, small) — lockbox-only

Extend the split path to carve K denominated pieces in one tx by passing a K+1-element `splits` vec
to `create_colored_split_tx` and `register_split_subcoins_n` (both already N-ary), writing one
`ConsignmentEnvelope` per piece. Gate on
`carrier_sats >= K*TOKEN_PIECE_SATS + split_fee_reserve(carrier_sats)`.

**E2E (sdk74):** carve 4 denominations from a 10,000-sat carrier; pay three different amounts by
subset-sum using whole-carrier handovers only, with zero further splits.

### Stage 5 — colored settle (RECOMMENDED, separate PR) — lockbox-only

Wire `create_colored_backup_tx(..., is_withdrawal = true)` (currently exercised only by the rgb09
test) as an SDK `settle_carrier(statechain_id)`: materialise the branch, then broadcast a colored,
**un-timelocked** withdrawal paying the owner's own on-chain address and carrying the opret. Being
un-timelocked it pre-empts every stale predecessor backup — it is the only counter-move that
preserves the allocation, and without it the honest terminal state of a deep WCH carrier is "asset
burned by whichever predecessor patches their client first". Caveats for the PR: it needs the blind
SE (so it is not exit-in-the-absence-of-the-coordinator), it is untested at SDK level, and it must
NOT join `unilateral_exit`'s default sweep, which correctly refuses carriers.

**E2E (sdk75):** at `k ≈ 50`, settle the carrier colored; assert the allocation lands on-chain valid
and that a predecessor's matured backup can no longer confirm.

### Stage 6 — CTES-R, only if §6's experiments pass

Not planned here. Gate on E1/E2 (§6) and re-cost from scratch; land the `payload_vout` migration of
`verify_bundle_ex` / `verify_child_bundle` as its own commit with adversarial tests **before** any
colouring is wired, because an off-by-one there is silent fund loss, not a red test.

---

## 5. Rejected alternatives

### 5.1 WCH as a *fastpath* (before the split guard) — REJECTED

Kills the 10,000-sat issuance carrier: `transfer_tokens(asset, bob, full_supply)` would hand over
the whole carrier instead of a 1,500-sat piece (6.7× silent sats give-away), and flips `used_split`
in seven existing tests. Shipped as a *fallback* instead (§4, stage 2).

### 5.2 "Convey a colored split that is built but left un-broadcast" — REFUTED

The split is **already** un-broadcast. Broadcast was never the cost; sats are — each sub-coin needs
a real funding output ≥ `min_split_output` plus a burned `fee_reserve`. The idea buys nothing.

### 5.3 Colour the per-hop transfer backup instead of leaving it uncolored — REJECTED

Puts a fresh RGB transition on every hop: the consignment grows per hop, a rival witness is created
per hop, and it reintroduces exactly the double-spend surface WCH's whole value lies in avoiding.
It also cannot share a consignment with the ladder exit route — `consign` builds one backward walk
per terminal witness — so it costs a second consignment per hop.

### 5.4 CTES-R option (b): suppress the flat backup (`backup_transactions: Vec::new()`) to get
unbounded hops — REJECTED, FATAL

`flat_backups` is `transfer_msg.backup_transactions.len()` — attacker-supplied, and sound only
because `validate_backup_chain_v2` structurally validates the same vector. Conveying an empty vector
either under-counts `expected` (fail-closed brick) or forces a declared baseline integer, which is
verbatim the padding attack the census was hardened against. Worse, the depositor's uncolored tx0
then has no rival: at `h_dep + 1000` they broadcast it (RBF-able), the holder's frozen-fee v3
trigger cannot outbid it, and the allocation is **irrecoverably destroyed** while the depositor
recovers sats they were owed anyway. Free grief.

### 5.5 CKL — colored keystone `K` over `F` with sponsored colored state tiers — REJECTED, FATAL

Two independent kills.

* **The sponsor input re-imports [B1].** `CSP` takes a second "fee bank" input on a different
  statechain node. If the bank is laddered, its un-timelocked `T_bank` voids `CSP` forever. If it is
  un-laddered and on-chain-funded, it carries a `create_tx1` backup maturing at `h_dep + 1000`,
  while `CSP` is not broadcastable until `K` confirms plus CSV `d0 = 1440`. **1440 > 1000**: the
  sponsor's own backup matures ~440 blocks *before* the recipient's `CSP` is even relayable. Sweep
  it and the recipient's seal never exists; the sender's retained superseded `CS` — which pays the
  sender and carries a valid transition — becomes the unique spend of `K.out[j]`. That is **theft**,
  at zero cost, at depth 1. No census can see it: the bank's backup lives on a different
  statechain_id and is *baseline*, not superseded. The repo already documents this exact shape at
  `clients/libs/rust-sdk/src/transfer.rs:686-700`.
* **INV-4 is locktime-only.** `validate_branch` checks `lock_time > tip` and nothing else; BIP68
  relative timelocks are invisible to it and to `tx.verify`. CKL puts CSV-encumbered txs *inside*
  the branch, so a receiver books a coin CONFIRMED that it cannot broadcast for ~10 days — precisely
  the loss INV-4 exists to prevent.

Also: `SEAL_SATS = 570` cannot fund a broadcastable tier (`tier_out_value(570, 2.0) = 82 < 330`), the
0-fee-plus-P2A workaround needs package relay that does not exist anywhere in the tree
(`exit_pass` broadcasts tx-by-tx via Electrum), `K` is v2/frozen-fee and cannot be CPFP'd by its v3
children under TRUC, and the shared bank serialises the tree so branch depth grows per *split*
against `PROTOCOL.md`'s depth cap of 8.

### 5.6 Carrier Float — rented 1:N carriers + blinded allocation-only payments — REJECTED, FATAL

**Seal-lifecycle collision.** The carrier is simultaneously the receive seal and the spend input, and
a payment rotates it. Alice issues `recipient_id` bound to carrier A; before Bob's payment lands she
makes a payment of her own, whose witness spends A and cannot consume Bob's not-yet-existent
assignment (Bob's witness is un-broadcast — the design's own premise). Bob then assigns to
`SecretSeal(A)`, which is already closed and terminal. **Permanent, silent burn**, undetectable by
both parties: Bob's consignment validates, and he cannot check the chain because A's spend is
off-chain. rgb-lib already encodes the contrary invariant — `get_input_unspents` refuses a UTXO with
`pending_blinded > 0` — and the design bypasses it by hand-building the PSBT. Mercury's
`blind_receive` wrapper passes `expiration_timestamp = None`, so invoices never expire and there is
no horizon after which rotation is safe.

Secondary kills: `max_allocations_per_utxo: 1` in mercury's RgbWallet makes 1:N multiplexing
impossible today and bounded (not unbounded) after a bump; the blind-receive lifecycle is never
closed so `pending_blinded` monotonically poisons the carrier; the payee's only pre-signed spends of
a *rented* carrier are plain backups that burn everything on it, so unilateral exit fails at hop 1;
and the proposed exact census `se_finalized == conveyed + 1` is not computable —
`count_finalized_signatures` is a lifetime count with no linkage to any transaction, and recovering
the signed message would land inside the enclave.

### 5.7 Literal shared/pooled UTXO (many holders on one seal) — REJECTED

A Mercury seal is a statechain coin with exactly one owner key-share, every co-sign authenticated
against that single owner's auth key. No co-holder can exit independently (constraint 4 dies), and
the single owner constructs the transition that reassigns everyone's allocations with nothing
stopping them taking a co-holder's (constraint 1 dies). Safe versions need an owner-signature
covenant plus a data-availability layer; neither exists.

### 5.8 `refresh_rgb_anchor_self_transfer`'s `beneficiary` mode as a payment — REJECTED

Zero callers, and unsound: it routes through `get_unsigned_backup_psbt`'s decrementing
deposit-anchored locktime and leaves the parent **non-terminal**, so the sender can afterwards
co-sign a lower-locktime backup committing a different transition and revoke the payee's allocation.
It also does not reset anything — it derives the new locktime by decrementing, i.e. it *consumes* a
rung. Delete or hard-gate it.

---

## 6. Open questions — decide or experiment before starting

1. **[EXPERIMENT, blocks stage 1] Does D1 reproduce?** Build the A→B→A round trip against the pinned
   fork and confirm the allocation disappears. Everything in stage 1 is cheap; confirming the defect
   is cheaper than arguing about it.
2. **[DECISION, blocks stage 2] `WCH_SAFETY_MARGIN`.** 144 blocks is the recommendation. It costs
   ~14 hops of the ~99 available. Smaller values hand receivers a shorter window to act; larger
   values cost hops. Needs a product call, not an engineering one.
3. **[DECISION, blocks stage 2] Fee-rate band policy.** The two-sided ±5 sat/vB band anchored to the
   *mint* rate (§3.6 (a′)) tightens as a carrier's wall-clock life lengthens — which is exactly what
   WCH is for. Options: (i) refuse hops outside the band (safe, surprising to users); (ii) raise
   `fee_rate_tolerance` for carrier wallets (widens an existing security check — needs review);
   (iii) accept and document. Recommend (i) for now, revisit with data.
4. **[DECISION] `TOKEN_PIECE_SATS`.** The true floor is `min_split_output` = 442 at 1 sat/vB, but
   1,500 is what buys the ~10.4 sat/vB headroom in guard (a). Halving to 700 drops that headroom to
   ~3.3 sat/vB. Recommend leaving 1,500 alone and treating this as an explicit trade-off, not a
   tuning knob.
5. **[EXPERIMENT E1, gates stage 6 / CTES-R] v3 colouring smoke test.** Build one tier tx (P2TR
   payload + 240-sat P2A, nVersion 3, nSequence = CSV) and push it through `RgbWallet::color`.
   Assert the opret lands at index 0 (the fork sets `opreturn_first = true` whenever any output is
   P2TR), that `Psbt::from_unsigned_tx` accepts nVersion 3, and that the opret is a stable 43 vB.
   Note `create_colored_split_tx` derives output vouts by filtering `!is_op_return`, which a P2A
   output would break — so it is not a thin wrapper. ~2 days.
6. **[EXPERIMENT E2, gates stage 6 — the one that decides CTES-R] The rival-tier test.** Colour `T`
   over `F`, then `X_0` over `T:1`, then `X_1` over `T:1` with the **same** blinding, then consign a
   state over `X_1:1`. Prediction from the dependency source: the consignment embeds `X_0`'s
   witness, because `RevealedValue` carries no blinding and the seal commits to the vout only, so
   the two transitions are byte-identical, share a `BundleId`, and `select_valid_witness` keeps the
   first-inserted on a Tentative tie. If confirmed, CTES-R needs unique per-tier seal blinding
   (`H(statechain_id ‖ tier_role ‖ index)`) and/or `ColoringInfo.nonce` plumbed through the bridge —
   a design rule, not something to discover at hop two. ~2 days.
7. **[DONE — REPRODUCED, then fixed in the fork] The `update_witnesses` self-destruct.**
   `WitnessStatus::Unresolved → Archived` plus the recursive `set_bundles_as_invalid` means one call
   to `update_witnesses` with the plain blockchain resolver permanently destroys a
   permanently-Tentative ladder. **Ran against the live stack (E7): reproduced —
   `succeeded=2, failed={}`, no error, both rungs dead.** See
   [`CTESR-GATE.md` §2.3](CTESR-GATE.md) for the full chain of evidence and for two corrections to
   the prediction above: it does **not** zero the balance (`get_asset_balance` keeps reporting
   `settled=future=spendable=1000` before and after — the *stock* dies while the sqlite-derived
   balance is blind to it, so a monitoring alarm on the balance would never fire), and the only
   repair rgb-lib offered was the on-chain unwind, bottom-up, one rung at a time.
   The invariant is now **code-enforced rather than conventional**:
   * fork — every `update_witnesses` call wraps the resolver in `TentativeStashResolver`
     (`utexo-rgb-lib/src/utils.rs`): a witness whose stored ord is `Tentative` and which the indexer
     reports `Unresolved` is served from the stash as `Resolved(tx, Tentative)`, or skipped
     (reported in `UpdateRes::failed`) — never archived. Only an explicit `force_witnesses` entry
     can archive an un-broadcast witness;
   * fork — `Wallet::revalidate_offchain_bundles(txids)` repairs an already-archived off-chain
     branch **without broadcasting anything**, plus `backup_invalid_bundles` /
     `restore_invalid_bundles` around any risky operation;
   * mercury — `scripts/ci/deny-rgb-witness-apis.sh` greps `update_witnesses|upsert_witness` across
     `clients/` and `lib/` and is run by `ci-guards/tests/deny_rgb_witness_apis.rs`, so a call site
     is a red test rather than a review catch.
8. **[OPEN, no owner] The colored re-anchor.** The epoch bound (§2.3) is only escapable by an
   on-chain colored re-anchor, which does not exist: `refresh` refuses carriers and
   `refresh_rgb_anchor_self_transfer` is dead code that consumes rather than restores a rung. This
   is the single largest remaining gap in RGB support and it is not scheduled. It should be.
9. **[OPEN] SSP replay surface.** WCH makes `(consignment, funding outpoint)` a **stable** identifier
   across hops — under the split lane every payment presents a fresh pair. Any SSP-side
   caching/dedup keyed on outpoint or consignment becomes a replay surface, and
   `validate_pending_token` alone can no longer distinguish "this coin is being paid to me now" from
   "I have seen this coin before". Gate on `(statechain_id, num_sigs)` or backup depth instead.

---

## 7. Cost table

Per **transfer**, for a minimal received piece (1,500 sats, 250 tokens) forwarding its whole
allocation, at 1 sat/vB unless noted.

| | sats LOCKED (new) | sats SPENT (burned) | new statechain slots | on-chain txs | hop bound |
|---|---|---|---|---|---|
| **Today (always-split)** | 1,500 per recipient | 300–2,000 `fee_reserve` per hop | 2 derived | 0 (until exit) | **1, structurally** — refused at hop 2 for any constant |
| **WCH (recommended)** | **0** | **0** | **0** | 0 (until exit) | `min(≈85 rungs after the 144-block margin, ~1,000-block root epoch, \|rate − mint_rate\| ≤ 5 sat/vB)` |
| **WCH + denominations (stage 4)** | K × 1,500 once, at carve time | one `fee_reserve` once | K derived once | 0 | as WCH, per denomination; arbitrary amounts by subset-sum |
| **CTES-R (colored ladder)** | 0 whole-allocation; 1,478/child on split | 0 whole-allocation; ~334 per split | 0 | 0 | 36 state rungs → free off-chain renew → unlimited, within the same ~1,000-block root epoch |
| **CKL** | 570/seal | ~900/split | derived | 0 | — **FATAL**, see §5.5 |
| **Carrier Float** | 0 | ~155/hop (sender) | 0 | 0 | — **FATAL**, see §5.6 |

Notes:

* "sats SPENT" for WCH is genuinely zero: no new sub-coin is funded and no reserve is carved. The
  112-vB miner fee is recomputed from the coin's funding amount every hop (`create_tx_out`), so it
  does **not** compound — `B_1` and `B_50` have the same output value — and it is only ever actually
  paid if the branch is broadcast, which the SDK never does for a carrier.
* Bandwidth per WCH hop is **flat**: zero new consignment bundles, zero new branch txs, zero new
  terminal ancestors; the message grows by one 112-vB row while the large payloads are re-shipped at
  constant size. `validate_consignment_offchain_chain` has no depth limit, so the frozen chain
  validates at constant cost forever. A split-per-hop design would instead wall out at the 25-tx
  mempool ancestor limit.
* **CTES-R's split floor is a regression, not an improvement**: the smallest colored carrier that can
  do a 2-way in-ladder split is ≈4,600–4,800 sats at 2 sat/vB, versus
  `TOKEN_PIECE_SATS + fee_reserve = 1,800` today, and off-chain colored combine (sdk31/sdk05/rgb08)
  does not survive the migration — a combine over un-broadcast tier outputs is not a tier
  (`cosign_tier_request` rejects >1 input) and a combine over the `F` outpoints is a rival for every
  coin's already-co-signed trigger. So CTES-R trades *better whole-allocation forwarding and a real
  unilateral exit* for *worse granularity*. Cost it on that basis.
* Every row shares the same wall: **~1,000 blocks ≈ 6.9 days from the original deposit**, then one
  on-chain transaction per carrier tree. That number is not improved by any design here.
