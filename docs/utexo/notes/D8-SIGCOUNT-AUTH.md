# D8 — Is `sig_count` authenticated end to end?

Verify-first result for DECISION D8 (docs/utexo/DECISIONS.md). READ-ONLY characterisation.
The census anti-theft test is exact equality `se_num_sigs == flat_backups + tiers + superseded`
(clients/libs/rust/src/tesr.rs:9093-9098). `se_num_sigs` is the coordinator's word.

## 1. Direct answer

**NO. `sig_count` is not authenticated at any hop.** It is a bare integer that rides plain JSON
from the lockbox DB, through the coordinator, to the client. No signature, MAC, attestation,
freshness nonce, or id-echo covers it anywhere. The enclave — the only trusted crypto core — never
reads, writes, or signs the count; it is pure host/DB state. The coordinator is a free substitution
point. **The coordinator's word is the ONLY evidence the receiver has, and its upstream (the lockbox
DB) is itself unauthenticated mutable state.** D8 is a REAL, currently-unmitigated trust assumption.

Chain of custody (every arrow unauthenticated, no integrity check at any node):

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
(`verify_tier_cosigned`, tesr.rs:9044-9051), so `expected` cannot be padded with junk. But the RHS
count is trusted, and the census is EXACT equality — an under-report of exactly the hidden tier's
count balances the equation. The client holds nothing chain-anchored to bind the count: idle TES-R
tiers are never broadcast, so on-chain there is zero evidence of how many co-signs occurred
(only funding output F is proven, transfer_receiver.rs:564-581).

Failure direction: **UNDER-report = THEFT** (hidden co-signed rival state survives the census).
OVER-report = fail-closed REJECT = grief/DoS only.

## 2. D8 enumeration — every coordinator-forwarded admission input

| # | Value (endpoint / field) | If coordinator lies | Class |
|---|---|---|---|
| a | **`num_sigs`** (GET /info/statechain, from lockbox /signature_count) — transfer_receiver.rs:46-83, tesr.rs:9094 | Under-report by k hides k co-signed rival states; census balances exactly; receiver accepts / SSP irreversibly pays a coin the sender can still reclaim | **THEFT** |
| i | **`terminal`** flag (GET /statechain/spend_budget, coordinator-computed `finalized>=budget`) — lightning_latch.rs:323-343; verify_terminal_parents transfer_receiver.rs:1082-1122 | Report `terminal=true` for a non-terminal ancestor → receiver accepts a branch sub-coin whose parent the sender can still double-spend | **THEFT** (branch/sub-coin lane) |
| f | `initlock`, `interval` (GET /info/config) — utils.rs:9-47; validate_backup_chain_v2 / INV-5 transfer_receiver.rs:613-626,653-658 | A lying `interval` changes what counts as a valid ladder decrement — the padding-defence term the census multiplies against num_sigs | **THEFT lever (flag; unproven break)** |
| b | `aggregate_pubkey` (get_aggregate_pubkey, mig 0009) — tesr.rs:8227-8290 | Cross-checked against on-chain funding taproot key + bundle.agg_address; a mismatch fails closed. UNIQUE(aggregate_xonly) blocks decoy reuse | **GRIEF** |
| c | `enclave_public_key` (get_enclave_pubkey) — validate_tx0_output_pubkey, transfer_receiver.rs:1313 | Used only to bind tx0 output = sender_pubkey + enclave_pubkey; wrong value fails the on-chain-anchored check | **GRIEF** |
| d | `statechain_info` rows: server_pubnonce, challenge, tx_n — database/transfer_receiver.rs:6-53 | Perturbs per-tx_n blinded-musig lookup (un-laddered lane); wrong/absent row → refusal | **GRIEF** |
| e | `x1_pub` (get_x1pub) — receiver.rs:1027-1064 | Wrong value → receiver derives unusable t2 → cannot claim; withhold → cannot claim | **GRIEF** |
| h | transfer mailbox (GET /transfer/get_msg_addr) | Ciphertext addressed-to-me is crypto-proven (only our auth key decrypts); can suppress/reorder delivery but not forge. Child census binds cb.child_statechain_id == transfer_msg.statechain_id (transfer_receiver.rs:699) | **GRIEF** |
| g | tip height / blockheight | NOT proxied — read from client's own electrum (transfer_receiver.rs:197-198). Not a coordinator lever | n/a |

Most non-`num_sigs` values are grief because the receiver re-anchors them to on-chain facts (b, c),
its own electrum (g), or cryptography (h). The two that also deserve synthesis attention beyond (a):
the coordinator-computed **`terminal` flag (i, theft)** and the coordinator-supplied
**`initlock`/`interval` (f, theft lever)**.

## 3. Closure path (SE-signed counts) — feasibility from the lockbox reading

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

## 4. Cheapest client-side detection available today

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

**No test stubs a lying coordinator.** sdk54/58/70 hold the count honest and mutate the bundle or
pass num_sigs±1 directly as an argument (test verifier math, not transport). sdk71's canned HTTP
server returns only garbage/404 (sdk71.rs:69-93,503-522), never a well-formed payload with a lowered
num_sigs. No mockito/wiremock/httpmock anywhere in clients/. Zero coverage of a coordinator returning
valid JSON with an under-reported count.

## 5. UNVERIFIED

- Whether the SE actually REFUSES a co-sign once a node's spend budget is exhausted enclave-side, or
  whether budget enforcement is coordinator-side only — decides whether item (i) is a pure
  coordinator break or needs enclave complicity. (enclave/lockbox read-only; flagged.)
- Concrete exploitability of a lying `initlock`/`interval` (f): a padding surviving every other check
  (fee-rate, locktime sanity, tx0 reconstruction) was not constructed — potential lever, not a
  demonstrated break.
- Semantic equivalence of coordinator-DB `finalized` vs lockbox `sig_count` in all in-flight states
  is UNVERIFIED — the §4 cross-check may false-positive during signing; safe operator (== vs <=)
  needs increment-timing traced.
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
  sites; JS clients refuse laddered coins outright but trust num_sigs on the flat lane
  (nodejs/transfer_receive.js:190-196, web:230-236).
