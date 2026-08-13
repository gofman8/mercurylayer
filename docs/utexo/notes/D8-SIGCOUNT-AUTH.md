# D8 — Is `sig_count` authenticated end to end?

Verify-first result for DECISION D8 (docs/utexo/DECISIONS.md). READ-ONLY characterisation.
The census anti-theft test is exact equality `se_num_sigs == flat_backups + tiers + superseded`
(`verify_bundle_ex`, `clients/libs/rust/src/tesr.rs` — cited by symbol, the line moved twice).

> **STATUS AT HEAD (2026-08-13).** §1–§5 are the 2026-08-10 verify-first characterisation and are
> kept as the problem statement. **They no longer describe the code.** `sig_count` IS authenticated:
> the enclave signs `utexo/sig_count/v2` over `(statechain_id, count, has_budget, budget, nonce)` and
> the Rust client REFUSES an unattested count (§6, §7 for row (f), §8 for row (i), D28/D40.2). Read
> §9 for what is still open — the JS/web lane, one un-repointed acceptance path, and the fact that
> the attesting key is held by the party the receiver is being protected from.

## 1. Direct answer (as of 2026-08-10 — SUPERSEDED by §6/§8; kept as the problem statement)

**NO. `sig_count` is not authenticated at any hop.** It is a bare integer that rides plain JSON
from the lockbox DB, through the coordinator, to the client. No signature, MAC, attestation,
freshness nonce, or id-echo covers it anywhere. The enclave — the only trusted crypto core — never
reads, writes, or signs the count; it is pure host/DB state. The coordinator is a free substitution
point. **The coordinator's word is the ONLY evidence the receiver has, and its upstream (the lockbox
DB) is itself unauthenticated mutable state.** D8 is a REAL, currently-unmitigated trust assumption.

Chain of custody **as it stood on 2026-08-10** (every arrow unauthenticated, no integrity check at
any node). Line numbers are as-of that date and have since drifted; the symbols are the durable
handles. This is the "before" half of the comparison §6/§8 close — see §9 for the chain at HEAD:

```
enclave  ── never sees the count ──▶ (nothing)
   │ partial_signature is stateless crypto; count is host state, not enclave state
   ▼
lockbox DB  generated_public_key.sig_count  INTEGER DEFAULT 0
   │  (plain SQL UPDATE by untrusted host wrapper; operator can UPDATE ... = N)
   │  db_manager.cpp:81, :465-474
   ▼  GET /signature_count/<id>  →  {"sig_count": N}   (bare JSON, plain HTTP)
   │  server.cpp:357-371, :430
   ▼  plain reqwest, no TLS pin / no app auth
coordinator  response["sig_count"].as_u64().unwrap()  →  num_sigs as u32
   │  transfer_receiver.rs:46-83   (renames + widens; ADDS no authenticator, STRIPS nothing)
   ▼  GET /info/statechain/<id>  →  StatechainInfoResponsePayload.num_sigs  (bare u32)
client  get_statechain_info → verify_bundle_bound(info.num_sigs)
        utils.rs:64-91 ; census tesr.rs:9093-9098   (no integrity check; trusted as RHS authority)
```

The client verifies only the LHS: each disclosed tier is signature-checked as a genuine co-sign
(`verify_tier_cosigned`, tesr.rs), so `expected` cannot be padded with junk. But the RHS
count is trusted, and the census is EXACT equality — an under-report of exactly the hidden tier's
count balances the equation. The client holds nothing chain-anchored to bind the count: idle TES-R
tiers are never broadcast, so on-chain there is zero evidence of how many co-signs occurred
(only funding output F is proven, transfer_receiver.rs:564-581).

Failure direction: **UNDER-report = THEFT** (hidden co-signed rival state survives the census).
OVER-report = fail-closed REJECT = grief/DoS only.

## 2. D8 enumeration — every coordinator-forwarded admission input

Nine rows, a–i. The "if coordinator lies" column is the analysis that motivated the fixes and is
unchanged; the **Class** column now carries the disposition at HEAD.

| # | Value (endpoint / field) | If coordinator lies | Class |
|---|---|---|---|
| a | **`num_sigs`** (GET /info/statechain, from lockbox /signature_count) — server `statechain_info`, client `verify_bundle_ex` | Under-report by k hides k co-signed rival states; census balances exactly; receiver accepts / SSP irreversibly pays a coin the sender can still reclaim | ~~THEFT~~ → **CLOSED on the Rust client** by the `utexo/sig_count/v2` attestation (§6); STILL OPEN on nodejs/web (§9) |
| i | **`terminal`** flag (GET /statechain/spend_budget, coordinator-computed `finalized>=budget`) — `set_spend_budget` / `get_spend_budget` in lightning_latch.rs; `verify_terminal_parents` transfer_receiver.rs | Report `terminal=true` for a non-terminal ancestor → receiver accepts a branch sub-coin whose parent the sender can still double-spend | ~~THEFT~~ (branch/sub-coin lane) → **CLOSED on the child-bundle lane** (D28 + D40.2, §8); the branch lane's `verify_terminal_parents` is NOT yet repointed (§9) |
| f | `initlock`, `interval` (GET /info/config) — `info_config` in utils.rs; validate_backup_chain_v2 / INV-5 transfer_receiver.rs | A lying `interval` changes what counts as a valid ladder decrement — the padding-defence term the census multiplies against num_sigs | ~~THEFT lever (flag; unproven break)~~ → **CLOSED on the Rust client** by the compiled-in `TesrParams::flat_ladder_params` (D27, §7); STILL OPEN on nodejs/web, which take both values verbatim (§9) |
| b | `aggregate_pubkey` (get_aggregate_pubkey, mig 0009) — tesr.rs:8227-8290 | Cross-checked against on-chain funding taproot key + bundle.agg_address; a mismatch fails closed. UNIQUE(aggregate_xonly) blocks decoy reuse | **GRIEF** |
| c | `enclave_public_key` (get_enclave_pubkey) — validate_tx0_output_pubkey, transfer_receiver.rs:1313 | Used only to bind tx0 output = sender_pubkey + enclave_pubkey; wrong value fails the on-chain-anchored check | **GRIEF** |
| d | `statechain_info` rows: server_pubnonce, challenge, tx_n — database/transfer_receiver.rs:6-53 | Perturbs per-tx_n blinded-musig lookup (un-laddered lane); wrong/absent row → refusal | **GRIEF** |
| e | `x1_pub` (get_x1pub) — receiver.rs:1027-1064 | Wrong value → receiver derives unusable t2 → cannot claim; withhold → cannot claim | **GRIEF** |
| h | transfer mailbox (GET /transfer/get_msg_addr) | Ciphertext addressed-to-me is crypto-proven (only our auth key decrypts); can suppress/reorder delivery but not forge. Child census binds cb.child_statechain_id == transfer_msg.statechain_id (transfer_receiver.rs:699) | **GRIEF** |
| g | tip height / blockheight | NOT proxied — read from client's own electrum (transfer_receiver.rs:197-198). Not a coordinator lever | n/a |

Most non-`num_sigs` values are grief because the receiver re-anchors them to on-chain facts (b, c),
its own electrum (g), or cryptography (h). The two that also deserve synthesis attention beyond (a):
the coordinator-computed **`terminal` flag (i, theft)** and the coordinator-supplied
**`initlock`/`interval` (f, theft lever)**. *(Both were subsequently built — §7 and §8. All three
theft-class rows are closed on the Rust client and none of them is closed on nodejs/web.)*

Row (c)'s GRIEF classification is worth stating precisely, because the attestation now rests on it:
`validate_tx0_output_pubkey` is the point-sum equality `tx0_out == user_public_key + enclave_public_key`
(`lib/src/transfer/receiver.rs`). Against a lying coordinator **alone** the coordinator cannot
substitute `E`: `user_public_key` arrives inside the sender's encrypted transfer message and the tx0
output is on-chain, so `E` is pinned to one value. Against a coordinator that also chooses `U` — the
sender colludes, or the coordinator minted the coin — the equation has two attacker-chosen unknowns
and the anchoring is circular. That is the boundary of what §6's "chain-anchored key" buys.

## 3. Closure path (SE-signed counts) — feasibility from the lockbox reading

*(This was the feasibility argument, written while `lockbox/` was out of scope. **It was built, very
nearly as described** — see §6. Two things the estimate got right and one it got wrong are worth
keeping: the key material and the "mechanically CHEAP" verdict held; the preimage shipped as
`(statechain_id, count, has_budget, budget, nonce)` rather than the triple below, because the budget
had to travel under the same signature (§8); and the last line — "not a recommendation to implement"
— was overtaken by D22.)*

- **Slot in the reply today: NONE.** /signature_count returns only `{"sig_count": int}` — no field
  for a signature, nonce, or id echo (server.cpp:357-371). A closure design must add a field.
- **Key material exists and the client already knows it.** Per-statechain secp256k1 server keypair
  (enclave.cpp:44-85); its pubkey is returned to the client at keygen (server.cpp:33-36) and after
  each keyupdate (server.cpp:219-222) — the receiver already learns `enclave_public_key` during
  transfer. So a client-verifiable attestation is mechanically CHEAP: one new enclave function that
  signs a tagged hash of `(statechain_id, sig_count, client-nonce)` under the server key share, with
  domain separation from MuSig partial sigs.
- **What it buys, what it doesn't.** It defeats a lying COORDINATOR (the D8 threat: lockbox and
  coordinator are distinct containers with separate DBs — LOCKBOX_DATABASE_URL=db_lockbox,
  docker-compose-lockbox.yml:39,84-92). It does NOT defend against the lockbox operator/DB: the
  enclave is stateless (enclave.cpp:139-204) and would faithfully sign whatever count the mutable
  host DB hands it. Operator-resistance needs enclave-sealed monotonic + rollback-protected state,
  which the plain-Docker lockbox fundamentally lacks (seed is host-provided at startup,
  server.cpp:252-280). Sealing also uses ad=NULL (enclave.cpp:19-20,36-37) so sealed blobs aren't
  bound to their statechain_id.
- **Net:** SE-signed counts = cheap, real defence-in-depth against a compromised-coordinator-but-not-
  lockbox adversary; meaningless where coordinator == lockbox operator. Modifying lockbox/enclave is
  out of scope; this is a design note, not a recommendation to implement.
- **Sharper, now that the signature exists.** The operator does not need to substitute the enclave
  key to forge an attestation: every per-coin server keypair is *generated inside the lockbox and
  stored encrypted under the host-provided seed* (`enclave::generate_new_keypair` → `encrypt_data`,
  enclave.cpp), so whoever holds the seed holds the discrete log of the **true** key for every coin
  and can sign any count it likes. The attestation is therefore unforgeable by a coordinator that
  does not hold the lockbox seed, and worth nothing against one that does. `attested_terminal`
  (tesr.rs) says the same thing in the code: *"This does not close CO-1."*

## 4. Cheapest client-side detection available today (NOT BUILT — superseded by §6)

*(Deliberately not built. The attestation landed instead, which is the "only sound fix" this section
names. The cross-check below did survive in one form: `attested_terminal` keeps the coordinator's
`finalized`/`terminal` answer as a cross-check that **refuses on disagreement**, on top of the
attested derivation rather than in place of it. Kept because the reasoning — and the reason two
mutually-consistent lies defeat it — is what justified paying for the attestation.)*

Cross-check `num_sigs` (lockbox-sourced, /info/statechain) against `finalized` (coordinator-DB-
sourced, GET /statechain/spend_budget = `COUNT(*) WHERE partial_sig_issued=true`,
database/deposit.rs:156-163; served lightning_latch.rs:332-340). The client already fetches
spend_budget on the child path and discards `finalized` (get_spend_budget lightning_latch.rs:302-320;
used only for terminality tesr.rs:5877-5887). Require `num_sigs == finalized` (or `<=`) before
trusting the census.

Reuses a field already on the wire, one comparison. **NOT a true fix:** both numbers pass through the
same coordinator, so a coordinator that lies consistently about both defeats it — it merely raises the
bar from forging one integer to forging two consistently. The only sound fix is the SE-signed count
in §3.

**No E2E stubs a lying coordinator, then or now.** sdk54/58/70 hold the count honest and mutate the
bundle or pass num_sigs±1 directly as an argument (test verifier math, not transport). sdk71's canned
HTTP server (`canned_http_server`, sdk71_unconditional_ladder.rs) returns only garbage/404, never a
well-formed payload with a lowered num_sigs. Still no mockito/wiremock/httpmock anywhere in
`clients/`. What *does* now exist is unit-level: ten tests around `verify_sig_count_attestation`
(`lib/src/transfer/receiver.rs`) pin the properties directly — an under-reported count does not
verify, a coordinator signing with its own key is refused, a replayed nonce is refused, an
attestation for another coin is refused, absent-budget and budget-0 are not interchangeable, count
and budget cannot be recombined across attestations, and a v1 attestation no longer verifies. The
gap that remains is transport-level: no test drives a coordinator that returns valid JSON with an
under-reported count over the wire.

## 5. UNVERIFIED

- ~~Whether the SE actually REFUSES a co-sign once a node's spend budget is exhausted enclave-side, or
  whether budget enforcement is coordinator-side only — decides whether item (i) is a pure
  coordinator break or needs enclave complicity.~~ **RESOLVED, and the answer changed underneath the
  question.** It was coordinator-side only; it no longer is. `generate_partial_signature`
  (`lockbox/src/server.cpp`, serving `/get_partial_signature`) reads `db_manager::get_sig_budget` and
  refuses with **410** when `sig_count >= sig_budget` — placed AFTER the
  idempotency cache (so a retry of an already-served session still returns its cached signature) and
  BEFORE the secnonce is consumed (so a refusal costs nothing). It fails CLOSED on an unreadable
  budget. See §8 and D28.
- Concrete exploitability of a lying `initlock`/`interval` (f): a padding surviving every other check
  (fee-rate, locktime sanity, tx0 reconstruction) was not constructed — potential lever, not a
  demonstrated break.
- Semantic equivalence of coordinator-DB `finalized` vs lockbox `sig_count` in all in-flight states
  is UNVERIFIED — the §4 cross-check may false-positive during signing; safe operator (== vs <=)
  needs increment-timing traced. *(Moot for §4, which was not built. The cross-check that DID ship
  compares the two sides' **terminality verdicts**, not their raw counts — and it is sound on the
  quantity it compares because both stores hold the same ABSOLUTE budget. The count-timing question
  is untraced and would return if anyone tried to compare `finalized` against `sig_count` directly.)*
- Whether /statechain/spend_budget is fetched on the FLAT and pre-pay-flat lanes (confirmed only on
  the child path) — enabling the §4 check on the flat lane may need an extra round-trip.
- Whether statechain_id re-creation after unauthenticated DELETE /delete_statechain (server.cpp:413-420;
  does not purge signed_session_cache) is reachable through coordinator flows — lockbox accepts it;
  coordinator behaviour unread. Same for the duplicate-row hazard (no UNIQUE on statechain_id,
  db_manager.cpp:104-108) and the uninitialized-count read (db_manager.cpp:503-514 + server.cpp:360
  returns stack garbage with HTTP 200 for an unknown id).
- No TLS/mutual-auth confirmed on the lockbox→coordinator transport in deployment; code path is a
  bare reqwest client with no pinning. End-to-end conclusion holds regardless (no app-layer
  authenticator exists at any hop).
- Non-Rust/JS clients and lib/out-python/mercurylib.py not exhaustively audited for further census
  sites. ~~JS clients refuse laddered coins outright but trust num_sigs on the flat lane.~~
  **CORRECTED — "refuse laddered coins outright" is not what the code does, and the difference is
  the whole exposure.** The `[S7]` gate in `clients/libs/nodejs/transfer_receive.js` and
  `clients/libs/web/transfer_receive.js` fires on three fields of the decrypted transfer message —
  `protocol_version >= 2`, `tesr_ladder`, `child_tesr_bundle` — all three **sender-declared**. A
  sender that declares version 1 and omits both ladder fields walks a laddered coin straight onto the
  flat `num_sigs == backup_transactions.length` check, and on those clients `num_sigs` is still the
  bare integer (`attestation` appears nowhere in `clients/libs/nodejs` or `clients/libs/web`) and
  `interval` is still taken verbatim from `/info/config` (`getConfig` in their `utils.js`;
  `flat_ladder_params` has no JS or wasm binding). That is the same shape the Rust side had to fix
  with `MIN_PREPAY_PROTOCOL_VERSION` — a gate keyed on what the attacker writes. So the JS/web
  population is not the exempt one, it is the exposed one: rows (a) and (f) are both open there.
  D40.2 item 2 decided the gate must be made **structural**; it has not been built.

---

## 6. Addendum 2026-08-11 — (a) is CLOSED; (f)'s obvious fix is circular

**(a) `num_sigs` — closed.** The SE now attests the count and the client verifies it against the
chain-anchored enclave key (`8bc7f1a`; primitive + 5 tests, wiring into the census sites is the
remaining half). Unblocked by **D22**, which revoked the SE scope rule. This row moves from "stated
trust assumption" to "closed", once the wiring ships.

*(Update at HEAD: the wiring shipped and the row is closed on the Rust client. `get_statechain_info`
is the single `/info/statechain` reader in the Rust tree; it sends a fresh random 32-byte
`attestation_nonce` on every call, verifies `utexo/sig_count/v2` against `enclave_public_key` — the
chain-anchored key, not the served `attestation_pubkey` — and, under D23, **refuses** a response with
no attestation rather than accepting it provisionally. Every census lane therefore consumes an
attested count. The test count is now ten, not five. What did not ship: the same reader on nodejs/web
— see §5 and §9.)*

**(f) `initlock`/`interval` — the naive fix would make things WORSE. Recorded so nobody builds it.**

The lever: INV-5 requires each flat-backup hop to decrement by *exactly* `interval`
(`ladder_decrements_by_interval`, `lib/src/transfer/receiver.rs:371-373`), and that is the defence
against a sender padding the backup vector with duplicates to inflate `flat_backups` and absorb a
hidden co-signed state (the attack its own doc-comment names at `:375-380`). `interval` was fetched
from the coordinator (`clients/libs/rust/src/utils.rs`), so **the coordinator defined the defence.**
*(Fixed 2026-08-11 — see §7. The rest of this section is kept as the reasoning that got there,
including the fix that looked obvious and was wrong.)*

The tempting fix is to derive the interval from the conveyed chain itself, since `L_k = L_0 −
k·interval` looks self-evidencing. **It is circular.** A padded chain with uniform `I/2` decrements
would derive `I/2` and validate against itself — accepting precisely the padding the
coordinator-supplied value at least constrains while the coordinator is honest.

The interval must come from a source **neither the sender nor the coordinator chooses**: a
compiled-in per-network constant, exactly as `TesrParams` already is
(`lib/src/tesr.rs:219-225`) — that asymmetry between the TES-R schedule and the flat-lane parameters
is the whole finding. That needs the normative per-network profile, i.e. **WP4/D7**, so (f) is
blocked on it rather than being the cheap win it looked like.

The "unproven break" rating stands: padding needs backups the SE actually co-signed, and each
co-signature also increments `sig_count`, so both sides of the census move together. Fix it anyway —
it is small once WP4 lands, and it removes the coordinator's ability to define a defence.

---

## 7. Addendum 2026-08-11 (later) — (f) is CLOSED

D25 landed the per-network-constant pattern, which was the blocker named above, so (f) was built as
described: `TesrParams::flat_ladder_params(network)` is now the single source of truth, and the
prediction held — it was small, and it removed the coordinator's ability to define the defence.

What shipped, beyond the one-line constant this note anticipated:

- **The client refuses**, by name, any coordinator whose `/info/config` disagrees
  (`clients/libs/rust/src/utils.rs`). The published values are a cross-check, never the source.
- **The coordinator refuses to boot** on the same disagreement (`server/src/server_config.rs`). This
  is the necessary companion, not belt-and-braces: once clients refuse, a config typo is a
  fleet-wide outage, and the difference between a five-second fix and debugging a fleet is whether
  the process says so at boot with both numbers in the message.
- **The SDK's `auto_exit_margin_blocks` derivation reads the interval through a `const fn`** instead
  of the two literals (`SE_INTERVAL_DEPLOYED` / `SE_INTERVAL_DEFAULT`) it used to transcribe, so the
  margin cannot be sized for a ladder nobody runs.
- **A CI guard parses the real deployment configs**
  (`ci-guards/tests/deny_flat_ladder_config_drift.rs`).

**Writing it turned up the drift the note did not predict.** Three of the four config sources in the
repo disagreed with the table and with each other — `docker-compose-main.yml` at 50000/6,
`docker-compose-test.yml` at 1100/1, `server/Settings.toml` declaring `testnet` at the regtest
1000/10. Only the running regtest stack matched, which is exactly why no E2E caught it. That is the
sharper version of the finding: the coordinator did not merely *define* the defence, the value it
would have defined it with was already inconsistent across the tree.

Verified live in both directions: the running coordinator (1000/10) is accepted as `regtest`, and a
coordinator configured one block off refuses to start. Recorded as **D27** in `DECISIONS.md`.

**Scope, stated because the row above says "CLOSED": the Rust client only.** `flat_ladder_params` has
no JS or wasm binding, and `getConfig` in `clients/libs/nodejs/utils.js` and
`clients/libs/web/utils.js` still returns `initlock` and `interval` exactly as the coordinator served
them, with no cross-check. On those clients the coordinator still defines the defence (§9).

---

## 8. Addendum — (i) `terminal` is NOT client-derivable, and the SE cannot vouch for it *yet*

*(Section renumbered: it was a second `## 7.`. Its "yet" was right — option (1) below is what
shipped, as **D28**. Read it as the argument that produced the design, then the "What shipped"
block at the end for the state at HEAD.)*

Answering the question §2 row (i) left open.

`terminal` is computed by the coordinator as `finalized >= budget`
(`server/src/endpoints/lightning_latch.rs:336`), and **both operands are the coordinator's own
database**:

| operand | source | independently checkable? |
|---|---|---|
| `finalized` | `count_finalized_signatures(pool, sid)` (`:332`) | **Nearly** — it tracks the SE's co-signature count, which §6 just made attestable |
| `budget` | `get_sig_budget(pool, sid)` (`:328`), column added by `server/migrations/0005_spend_budget.sql` | **No** — see below |

**The decisive fact: the lockbox has no notion of a spend budget.** Grepping `lockbox/src` and
`lockbox/include` for `budget` returns nothing; `sig_budget` exists only as a coordinator column.
So the SE *cannot* attest terminality today even though it now attests the count — it does not know
the threshold the count is being compared against. *(True when written; false at HEAD — that grep now
returns the `sig_budget` column, `set_sig_budget`/`get_sig_budget`, the `/sig_budget` route and the
enforcement point. This was the blocker, and removing it was the fix.)*

**What the receiver relies on this for.** `verify_terminal_parents`
(`clients/libs/rust/src/transfer_receiver.rs:1082-1120`) requires every structural ancestor of a
branch-funded sub-coin to be terminal, precisely so the sender cannot hide a non-terminal ancestor it
could still double-spend. A coordinator reporting `terminal=true` for a live ancestor defeats it.
Note the client already fails closed on transport problems (`:1107`) and on a missing/false flag
(`:1114`) — the gap is not sloppiness, it is that a *lying* coordinator is indistinguishable from an
honest one.

**Therefore (i) does not close by client derivation.** The options, now that D22 puts the SE in
scope:

1. **Move or mirror `sig_budget` into the lockbox** and have the SE attest `terminal` the same way it
   now attests `sig_count` — same key, same nonce-bound preimage shape. Strongest, and it makes the SE
   the authority on the property that is actually about the SE's future behaviour ("will it co-sign
   again?"). Cost: a schema and a write path on the SE side, and the budget must be set through the
   SE rather than beside it.
2. **Attest the pair `(finalized, budget)`** and let the client compute the comparison. Weaker in
   form but equivalent in effect, and it keeps the policy decision on the coordinator.
3. **Leave it stated** as an enumerated trust assumption alongside the others.

Recommend (1) or (2); (3) is now a choice rather than a necessity, which is the change D22 bought.
Either way `terminal` is a **second SE-attested field**, not a second bespoke mechanism — the
preimage-with-client-nonce shape from §6 generalises directly.

### What shipped (D28 + D40.2)

Option **(1)**, with one correction to the shape this note proposed: the budget does not get its own
attestation. It rides **inside** the count's signature, because two signatures could be mixed across
time — a fresh count paired with a stale budget — which is the exact confusion an attestation exists
to remove. `has_budget` is an explicit byte: "no budget" (co-signable indefinitely) and "budget 0"
(terminal) are opposite facts and must not share an encoding. v1 is gone rather than retained (D23).

```
sha256("utexo/sig_count/v2" || statechain_id || u32_be(count)
       || u8(has_budget) || u32_be(budget) || nonce32)
```

Built in one definition per side — `lockbox/src/server.cpp`'s `/signature_count` route and
`mercurylib::transfer::receiver::verify_sig_count_attestation` — so signer and verifier cannot drift.

- **The SE enforces what it attests.** `db_manager::set_sig_budget` is a monotone ratchet in one SQL
  statement (`WHERE sig_budget IS NULL OR sig_budget > $1`), so a budget may be created or lowered,
  never raised; a raise is a **409** naming both values, and setting the same budget again is
  idempotent. Without the ratchet an attestation reading "1 of 1, terminal" would be true when issued
  and worthless a block later.
- **The coordinator reports failure when the mirror does not land** (`set_spend_budget`,
  lightning_latch.rs): its own write succeeding while the enclave disagrees produces a coin the
  receiver's verification refuses, so reporting success would hand the caller an unclaimable coin.
- **The mirrored quantity is ABSOLUTE on both sides.** `remaining` is relative ("one more co-signature
  from here"); the coordinator stores `count_finalized_signatures + remaining` and mirrors *that*.
  Mirroring the relative value made the enclave read "never more than 1 co-signature in this coin's
  life", and because both budgets are monotone and the coordinator's write lands first, the coin came
  out terminal at the coordinator and pinned at 1 in the enclave — exit-only, permanently. Measured
  live: coordinator `{"sig_budget":7,"finalized":6}` against enclave `{"sig_budget":1,"sig_count":6}`.
  Pinned by `ci-guards/tests/deny_relative_budget_mirror.rs`.
- **The client derives terminality from the attested pair.** `attested_terminal` (tesr.rs) computes
  `budget exists && num_sigs >= budget` from the signed payload, refuses a missing attested budget
  rather than reading silence as "not terminal" (the permissive direction), and keeps the
  coordinator's own answer as a cross-check that **refuses on disagreement** — both stores hold the
  same absolute quantity, so a mismatch means one was written behind the other's back. Pinned by
  `ci-guards/tests/deny_unattested_terminality.rs`.

---

## 9. State at HEAD (2026-08-13) — what is closed, and the three things that are not

The chain of custody in §1, rewritten for HEAD. Every arrow that carried the census RHS is now
covered by one signature, on the Rust client:

```
enclave  attest_sig_count  — BIP-340 over sha256("utexo/sig_count/v2" || sid || count
   │                         || has_budget || budget || client nonce), signed with THIS COIN's
   │                         server keypair
   ▼  GET /signature_count/<id>?nonce=…  →  {sig_count, has_sig_budget, sig_budget, terminal,
   │                                         attestation, attestation_pubkey, attestation_nonce}
coordinator  forwards the attestation fields verbatim (statechain_info)
   ▼  GET /info/statechain/<id>?attestation_nonce=…
client  get_statechain_info → verify_sig_count_attestation against enclave_public_key
        (the CHAIN-ANCHORED key, not the served attestation_pubkey); NO attestation ⟹ refuse
        → verify_bundle_ex census RHS, and attested_terminal for terminality
```

**Still open, all three verified at HEAD:**

1. **nodejs and web are on the old chain of custody entirely.** No attestation request, no
   verification, no compiled ladder parameters, and a fail-closed gate keyed on three sender-declared
   fields (§5). Under D23 that makes them non-conformant rather than merely behind. **D40.2 item 2
   decided the gate must be structural; not built.**
2. **One acceptance path still reads terminality from the unattested endpoint.**
   `verify_terminal_parents` (`clients/libs/rust/src/transfer_receiver.rs`) queries
   `GET /statechain/spend_budget/<id>` and accepts its bare `terminal` boolean — on the claim path and
   again inside the pre-pay value gate. D40.2 item 1 says to delete the unattested reads on *both*
   acceptance paths; `verify_conveyed_child` was repointed to `attested_terminal` (`52daca6`) and the
   branch/sub-coin lane was not. `deny_unattested_terminality.rs` polices `verify_conveyed_child`
   only, so nothing catches this. **This is row (i) still live on the branch lane** — the decision
   record is right and the code has not caught up.
3. **The attestation does not close CO-1, and cannot.** Every per-coin server keypair is generated
   inside the lockbox and stored encrypted under the host-provided seed, so the seed holder can sign
   any count and any budget (§3). Both stores the attestation covers are plain Postgres columns in
   the same operator's lockbox. The attestation closes the gap between what the enclave signed and
   what the client read; it does not make the enclave a party the receiver need not trust. Only a
   second, independently administered SE write domain — under a second legal entity — would (D40.2).
