# BLOCKERS

Things that are genuinely undecidable here, externally blocked, or out of bounds. One line of
context each so they can be picked up without re-deriving. **A blocked item is not a reason to stop —
it is an entry in this file.**

## Escalated — needs the product owner, cannot be decided in-repo

* **SE seed + Vault root token are committed and the repo is PUBLIC.**
  `docker-compose-main.yml` carries both literals; `github.com/gofman8/mercurylayer` returns 200 to
  an anonymous API call. Rotation requires generating new secrets and deploying them to
  infrastructure holding real funds — outside what can be done from here. The audit
  (`docs/utexo/AUDIT-2026-07-30.md`, Tier 0) rates this ahead of every code fix, because it makes the
  other guarantees vacuous.

* **CATS-B's liveness trade is a product decision, not an engineering one.**
  CATS is symmetric — zero-CSV accelerates the honest exit and the theft identically — so the payee's
  on-chain warning drops from ~4 years to ~15.7 days. That is a normal L2 assumption, but it becomes
  a **per-payee obligation regardless of payment size**, and a sub-economic payee will rationally
  stop watching. Documented in `PARTIAL-PAYMENT-ECONOMICS.md` §4.8. Building CATS-B accepts this.

* **The lean-leaf variant (§4.6) is a separate decision.** −71% per payment instead of −45%, but it
  breaks the other constraint — every leaf pre-funding a private exit. Not in the current queue.

## Out of bounds by standing constraint

* **`.github/` workflows.** The `ci-guards` deny-list runs under `cargo test` but **no CI job invokes
  cargo**, so the guard never fires in CI. Wiring it needs a one-line `cargo +stable test -p ci-guards`
  step in a job with a Rust toolchain. CI config changes are out of bounds; this is the owner's call.

* **`enclave/` and `lockbox/`.** The SE is blind and must stay so. If something appears to require the
  SE to distinguish coloured from plain, that is the expensive path with a proven history of landing
  on the tested lockbox lane and missing the shipped SGX one — stop and report rather than proceed.

## Found by adversarial review, fixed, but the class is not exhausted

* **The split child's tier chain had no value-conservation check** (fixed 2026-08-03). A sender could
  fund a payee's slot with 198 530 sat and have the child's extension forward only 1 000, skimming
  the rest to a second output; `verify_child_bundle` returned `Ok(())` and the receiver booked the
  FUNDING value. Proven by running it against the real verifier, not argued.
  **The general lesson, which is not yet swept:** `verify_tier_cosigned` binds a co-sign to the
  tier's INPUT amount and says nothing about how that amount is split across outputs — and the SE is
  blind, so it signs any distribution by design. Every place that reasons about a tier's value needs
  to ask whether it is reading a parsed output or assuming conservation. Two hops are now bound; the
  ancestor-segment hops and the split state's Σ-payload conservation have **not** been audited with
  this question in mind.

## Open, unexplained, needs its own investigation

* **rgb-lib refuses to colour a legacy-lane carrier.** Four independent reproductions: a piece above
  the coloured root floor, holding exactly one booked allocation, with confirmed funding, still gets
  `Invalid coloring info` over its own funding output — stable across 40 claim passes. Suspected E7
  class (a stash whose witness was resolved with the plain resolver while un-broadcast), but the fork
  guard should already prevent that, so either the guard has a gap or the cause is different. This is
  why the migration hatch keys on **evidence** (can a ladder actually be built) rather than on size.
  If the answer is "every legacy-lane piece is permanently un-colourable", that is a migration fact
  the owner needs stated, not a footnote.

* **`execute_ex` opens a conveyance on a coin whose exit has already begun.** If `F` is spent / `T`
  confirmed, the receiver refuses (`transfer_receiver.rs` "tx0 output is spent or not confirmed"), so
  the sender burns two SE co-signs and leaves the coin `IN_TRANSFER` on a transfer nobody can
  complete. Found while building `sdk79` part C; the test documents and works around it.

* **`TIER_VBYTES` understatement was systemic.** The tier witness item is 65 bytes, not 64
  (`SIGHASH_ALL` serialises the explicit `0x01`; 64 is only right for `SIGHASH_DEFAULT`). Corrected to
  125 / 168, but every uncoloured tier had been marginally underpaying its own standalone relay —
  worth checking whether any other vsize constant in the tree carries the same assumption.

## Deferred, tracked, not blocking

* **Porting `verify_bundle` to wasm/JS and Kotlin.** nodejs and web receivers THROW on any laddered
  coin ("this client cannot verify the exit ladder"), so every coin laddered is one they cannot
  receive. This — not CTES-R — is the real gate on retiring the flat lane entirely.
* **The ~69-day root epoch.** An unspent on-chain root plus the depositor's maturing absolute-locktime
  backup. On-chain state; only an on-chain colored re-anchor moves it, and that primitive does not
  exist. No design in `PARTIAL-PAYMENT-ECONOMICS.md` claims to remove it.
