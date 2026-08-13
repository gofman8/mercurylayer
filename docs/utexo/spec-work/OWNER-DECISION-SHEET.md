# Mercury Utexo — Owner Decision Sheet and Path to Specification

Prepared against HEAD `f30bf71` (feat/spark). Where a proposal claimed `50c2d36` as HEAD, two commits had landed since — `77807e1` (D14 supersession margin, in code) and `f30bf71` — and every Rust line number above `tesr.rs:~10450` in those proposals is stale by roughly 70–190 lines. Line numbers below are given as verified by the adversarial pass where the two disagreed.

---

## Part 1 — The decision sheet

Ordered by what a wrong answer costs, not by index.

---

### 1. B1 — key-share retention: what does "non-custodial" actually mean? (idx 0)

**THE DECISION.** Whether to publish B1 as an unbounded, custodial-equivalent exposure against a retaining operator, and whether to ship any of the three candidate mitigations (re-anchor on receipt, compiled value threshold, TEE rollback protection) — or ship only the two operator-independent remedies that actually work.

**EXPOSURE.** Any coin with ≥1 prior owner: 100% of value, taken by an offline single-sig key-path spend of F that no census, attestation, budget, rate limit, or audit log can see, because no SE session occurs. Aggregate is the entire transferred float, enumerable by the operator from `statechain_data.aggregate_xonly` plus the chain.

**BOUND.** **No value bound is constructible. UNBOUNDED, not unevaluated** — per-victim (sum over their coins), per-coin (no cap on coin size), aggregate (whole transferred float). The three mitigations the corpus currently publishes all fail: blindness is refuted by the operator's own migration-0009 column; "≥144 blocks of notice" is false because F is key-path P2TR with `merkle_root = None` and the attack never broadcasts T; "pre-hack untouched coins are unconditionally safe" is circular — its proof is T-SE-1 restated. A fourth false mitigation the first pass missed also stands at SPEC-FOUNDATION:59: "collusion leaves publicly-queryable evidence".

One **duration** bound is real and the first pass missed it entirely: **B1 exposure ends when the holder broadcasts T** — already co-signed, `lock_time 0`, `TRIGGER_SEQUENCE 0xFFFFFFFD`, 125 vB, no SE, no counterparty, no wait. Once T confirms, every historical `(o_i, e_i)` is dead. It bounds time and adversary class, not value, and it is the only remedy the adversary cannot decline.

**THE FIRST PASS SAID** value-gated re-anchor at a compiled θ. **That is wrong for three reasons.** (a) `refresh`/`reanchor` is *cooperative* — `refresh.rs:14-16` says so in its own doc, and `reanchor` routes through `withdraw`, needing one fresh SE co-signature. The B1 adversary *is* the operator. The recommendation was "ask the operator to sign the transaction that destroys its own attack," and refusal is already blessed as non-theft (PROTOCOL.md:672). (b) θ caps a *coin*; a victim holds *n* coins, so per-victim exposure is n·θ. (c) `amount` is chosen by the **sender** — a hostile sender conveys a large payment as many sub-θ pieces and the gate never fires. Also: `reanchor` computes `amount_out = amount - fee_sats`, silently breaking REQ-2's amount-exactness; a CATS-B payee holds a two-rung child, so re-anchoring costs full branch materialization (up to 23 tx / 9,383 blocks), not 112 vB; and `colored_reanchor` (CR-D) mints **no fresh SE keypair**, so the "zero prior owners afterwards" claim is unestablished on that lane.

**OPTIONS.**

| Option | Real cost, including what users lose |
|---|---|
| **(A) Publish corrected, defend nothing** | Zero engineering. "Non-custodial" narrows to "non-custodial against a point-in-time snapshot; custodial-equivalent against a retaining operator." REQ-3 cannot remain MET. G7 becomes a corollary of T-SE-1, deleting the strongest marketing sentence. Users get no new defence. |
| **(B) Re-anchor on every receipt** | 112 vB/hop (224 sat @2, 1,120 @10, 3,360 @30). Deletes REQ-1's headline. Arithmetically impossible for the sub-economic tail: at 10 sat/vB the 1,120-sat floor sits above the 1,500-sat TOKEN_PIECE margin — charges everyone to protect nobody at the bottom. Cooperative, so the adversary can decline it. |
| **(C) Compiled θ threshold** | All of (B)'s problems plus: caps a coin not a victim; gated on a sender-chosen field; θ = 112·r/p is a fee-tolerance ratio with no adversary in it; delivered amount shrinks by ceil(112·r); fails open on derived-slot exhaustion; the re-anchor is a public announcement that a valuable coin is escaping, inviting a fee war whose stake is the coin. |
| **(D) TEE with per-coin rollback protection** | Requires real SGX; production runs lockbox (plain C++, host-held seed). Does not eliminate B1 — reduces it to "vendor honest AND no rollback," and rollback is SGX's known weak point. With no client-side attestation, swaps an unverifiable operator promise for an unverifiable hardware promise. |
| **(E) Ship the unilateral-T remedy + structural prior-owner count** | Zero new cryptography. Surface "sever from F": broadcast the already-signed T, 125 vB, no SE. Costs the coin its off-chain life and starts the CSV walk; undoing it needs the SE. Users gain the only operator-independent action in the row. |

**RECOMMENDED: (A) + (E), unconditionally — publish corrected and expanded, ship the unilateral-T remedy, and surface the *structural* prior-owner count `k = (h_deposit + initlock − min_locktime)/interval`; do NOT ship compiled θ.** Only (E) gives a quantity the adversary cannot switch off, and (A) is mandatory regardless because two of the four false mitigations are refutable in one grep each.

**Do not ship `prior_owners = flat_backups.len() − 1`.** It is forgeable downward: `validate_backup_chain_v2` (lib/src/transfer/receiver.rs:431-468) enforces only *pairwise* decrement plus an inequality locktime test; nothing anchors the head of the chain to `h_deposit + initlock`. A sender truncates the high end, buys one extra co-sign per dropped rung, discloses it in `superseded_states`, and the census stays balanced. A receiver can be shown a 1-element chain and conclude "structurally B1-immune." Fix the truncation hole (one comparison: max conveyed nLockTime must equal `h_deposit + initlock` exactly) and publish `k`, whose terms are on-chain or inside a signed transaction. Mainnet cap is **k ≤ 100** (not 101 — that is the max chain *length*), and its sign is adversarial: k is the number of *independent chances* the operator has to find a cooperative ex-owner, not a comfort figure.

Keep (D) on the roadmap for CO-3/SM-5. Keep re-anchor-on-receipt available as an *explicit user choice*, priced honestly, failing closed on SE refusal and slot exhaustion, with the spec stating it is unavailable precisely against the adversary it defends against.

---

### 2. CO-1 — pin the SE identity, or admit admission is issuer-trusted (idx 7)

**THE DECISION.** Whether receiver admission is a cryptographic property or an issuer-trust property — and if the former, whether to ship pinned SE identity + derived shares + confidential t2 + a lockbox host split as one unit.

**EXPOSURE.** The coordinator, acting **alone** (it runs a wallet; it mints its own deposit tokens; it owns the `confirmed`/`spent` columns), funds F to a key D whose discrete log it knows, writes `user_public_key`/`server_public_key` summing to D, signs the entire ladder under d with no enclave, and serves whatever `num_sigs` balances the census. Loss per victim is the full consideration paid; aggregate is unbounded in mint/sell/spend-back cycles at recovered capital.

**BOUND.** **None constructible; UNBOUNDED.** Coin value is a tautology, not a protocol bound; attacker capital is recovered every cycle; deposit rate is the operator's own row.

**THE FIRST PASS SAID** "no proof of knowledge can close CO-1" and proposed writing that impossibility theorem into DECISIONS.md. **That is false and must not enter the corpus.** D34's PoP fails for a *contingent* reason: the operator recovers the scalar `o2 = o1 + x1 − t2` because **t2 arrives in plaintext** (server/src/endpoints/transfer_receiver.rs:555-563, forwarded :681-687), with `o1` held by the colluding sender and `x1` generated by the coordinator itself. Encrypt t2 to the SE and the coordinator learns only the *point* t2·G; D34's premise becomes true and the PoP holds. The first pass listed t2-encryption as a demoted "follow-on" inside its own option 2 — it is the load-bearing item.

Two further corrections. The first pass claimed sender-alone is blocked only by an *ordering* property; in fact `verify_transfer_signature` (lib/src/transfer/receiver.rs:214-233) demands a Schnorr signature under the declared `user_public_key`, so the rogue-U decomposition `U := D − E` against an honestly generated E yields a U whose discrete log the attacker does not know — the block is cryptographic, and only the `E := D − U` variant (coordinator-generated enclave key) is constructible. And the proposed replacement check (`E_sid = M + H(…‖sid‖U0)·G`, `U0 + E_sid == F`'s internal key) is a **deposit-time invariant** that says nothing about whether the keyupdate happened, happened with the real x1, or happened twice — it *replaces* a current-aggregate check with a static provenance check and silently drops the property D34's PoP certifies.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) t2 confidential to the SE** | ECIES layer on the claim path; the auth binding (receiver signs `sha256(t2_hex)`, verified server-side) must be re-specified or the claim path loses authentication. Without it, o2 is recoverable by anyone who ever held o1 — a standing theft of *honestly-minted* coins, forever, outside CO-1's stated scope. |
| **(B) Pinned SE identity + derived shares** | Lockbox derivation change, `/identity` route, compiled per-network table, rewritten receiver check. Users lose: SE key rotation now requires a client release; a stale pinned M refuses every new coin (hard liveness cliff); every pre-change coin must be exited and re-deposited; per-enclave-index pinning publishes the operator's sharding topology. |
| **(C) D34 claim-time PoP** | New enclave endpoint, per-hop round trip, one more coordinator-mediated failure mode. **Vacuous before (A) and (B)**; sound after. It is the only check that certifies the *current* share. |
| **(D) Lockbox host split** | A second production trust domain to run, provision, key-manage; incident recovery slows (the party who can restore the coordinator cannot restore the SE). Without it, (A)–(C) are all forgeable by the seed-holder. |
| **(E) No fix; name issuer trust in §1** | Zero engineering. The instrument stops being "a coin a receiver can verify" and becomes "a coin whose issuer a receiver must trust." O-1 cannot then be graded "mitigated" in the same document. |

**RECOMMENDED: (A) → (D) → (B) → (C), shipped as one unit in that order.** The custody split is the only move that changes any adversary's cost; t2 must be confidential first because until it is, D34 is forgeable *and* the operator has standing theft of honest coins; pinning is what makes any SE-side proof mean anything, because the coordinator picks which enclave answers (`get_random_enclave_index`, deposit.rs:403); the PoP goes last and is the only check covering the live share.

Rewrite D34 to say the PoP does not close CO-1 **as deployed** (plaintext t2, coordinator-chosen enclave) — not that no proof of knowledge can. A false theorem in a normative document is worse than the open row.

---

### 3. O-1 / CO-3 — what anchors the census RHS (idx 6, idx 11 — one defect, two rows)

**THE DECISION.** Whether `se_num_sigs` is authoritative because the operator says so, and what to do about it. These two rows are one defect: enclave key material and the counter it attests are both held by the party the receiver is being protected from.

**EXPOSURE.** Two routes, ranked by privilege. (iii) *Cheapest*: nodejs/web `getStatechainInfo` sends no nonce and returns `num_sigs` raw; the laddered gate fires on three **sender-declared** fields (`protocol_version >= 2 || tesr_ladder != null || child_tesr_bundle != null`), so a sender declaring version 0 with both fields omitted falls through to a bare equality against an unauthenticated integer — a plain HTTP-response edit, no seed, no DB write. (ii) The lockbox host holds the seed as a `std::vector` captured `[&seed]` into every route and assembles the `utexo/sig_count/v2` preimage in host code, so it signs any (count, budget) pair directly. (i) Mint-time key substitution is **dominated** — it needs strictly more (seed *plus* a colluding sender *plus* malice at deposit) and reaches a strict subset of coins.

**BOUND.** **Per-admission on the plain lane: real** — loss ≤ min(F's value, price paid), forced because the concealed rival spends F and Bitcoin permits that once, *plus* the exact-equality census denying an undisclosed rung a co-sign slot to hide in. **Per-victim: not constructible** (n admissions, n losses). **Coloured lane: not constructible in sats at all** — a plain rival tier *destroys* the allocation, and no attacker-gain figure describes that victim. **Aggregate: unbounded.**

Two claimed bounds die. "Per-victim, capped at one coin" mislabels a per-*admission* figure. "Victim count ≤ floor(F/1310) = 76,335 for a 1 BTC coin, and per-level fan-out capped at 63 by `max_derived_tokens_per_statechain = 64`" is wrong: 64 is a **per-parent lifetime** cap over spent+unspent rows, not a per-level fan-out, so the binding victim count is ~64 — three orders of magnitude below the published 76,335, and published in the alarming direction.

**THE FIRST PASS SAID** anchor the counter head to an outside witness (Merkle root of `(sid, n, h_n)`, receiver requires attested n ≥ last anchored n). **That is self-refuting.** The anchor commits to numbers the operator authors, and the receiver's rule is a *floor* against an adversary who writes the floor. This row's attack is **under**-reporting: anchor n=5 while the true count is 7, serve 5, check 5 ≥ 5, pass. Concealment window is unlimited, not "one block of co-signs." Tightening to equality does not save it — `h_n` is a hash over per-coin session keys no receiver can recompute. Every variant reduces to "the SE was honest about n," which is where option (A)'s named assumption already lands. It also carries three unadmitted costs: a per-coin activity oracle (privacy regression in a design whose SE deliberately sees no values), an anchor that must *confirm* before admission and so cannot be sent at the committed 2.0 sat/vB during congestion, and nothing at all against the census's *other* soft term, `superseded`.

**A LIVE HOLE, needing no DB write and no rollback.** `verify_conveyed_child` fetches the attested payload into `p_info` (tesr.rs:7487) and then takes terminality from `crate::lightning_latch::get_spend_budget` (tesr.rs:7496-7497, and per-ancestor at :7506) — an **unattested** coordinator endpoint computed entirely from the coordinator's own Postgres. A repo-wide grep for `has_sig_budget`/`.sig_budget` in `clients/` returns only `utils.rs:146-151`: the attested budget is verified and then never read by any acceptance decision. Live on both prepay (transfer_receiver.rs:772) and claim (:1204). One unattested `terminal: true` kills a whole parent tree at once.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) State the assumption; consume what is already attested** | ~1 day. Derive terminality from `has_sig_budget && num_sigs >= sig_budget`, delete the unattested read, keep the coordinator answer as a cross-check that refuses on disagreement. Zero user-visible cost; a receiver who would have been robbed gets a refusal. Leaves the entire state half open. |
| **(B) Owner-held receipt chain (D8 option c)** | One column, one hash per co-sign, v3 preimage, client receipt store. Detection needs a *pre-tamper* receipt; for a coin sold to a fresh receiver the only holder is the prior owner — the colluder. Real value against **accidental restore** and operator-alone griefing. Worth nothing at admission. A wallet restored from an old backup false-alarms on its own coins, so the alarm must be advisory, and advisory alarms get clicked through. |
| **(C) External anchor** | New subsystem. Refuted above: a floor check cannot bound under-reporting. |
| **(D) Second, independently administered SE write domain** | The only construction whose logic works — it removes the single party who authors the number. Requires either splitting the MuSig2 share n ways (deep change to blind signing and every rotation path) or an attest-only witness *on* the co-sign path. Users lose availability and latency on every deposit, transfer, split and latch. Meaningless if the second administrator is the same legal entity, which is the only deployment that exists. |

**RECOMMENDED: (A) unconditionally and now, plus close the JS/web lane, plus name (D) as the only real closure and gate it on a second legal entity.** Build (B) if wanted for the accidental-restore column *only*, labelled as such and never as closure. Record (C) as rejected with the floor-check argument written down so it is not re-proposed.

Retract the monotonicity premise where it is load-bearing — G6 at SPEC-FOUNDATION.md:96. **Leave :41 alone**: it sits in the malicious-*receiver* row's "provably cannot" column, and against a receiver acting alone it is true. :45 is sloppy adjectival phrasing, not an assertion. Demanding all three be retracted hands the author an easy rebuttal on two of three. Add **G2** (no undisclosed spending path) to the blocked list — it is the goal this defeats, and its own text names the attested count; G6 (reversal of a completed claim) is the wrong goal for an admission defect.

Also note: `sig_budget` is **absolute on both sides** at HEAD (`deposit.rs:228-245` computes `count_finalized_signatures + remaining`; `lightning_latch.rs:307/:360` mirrors that same value). The first pass read a fixed bug's post-mortem comment as current behaviour and hard-coded a weaker booleans-only cross-check around it. The raw quantities *are* comparable, and the stronger check is available.

---

### 4. R-2 / T-1 — whole laddered coins carry an unwatched calendar deadline (idx 12)

**THE DECISION.** INV-27 ("nothing ages while a coin is un-triggered") is already retracted by D36. The decision now is *what the client must do* — and the answer is smaller than the first pass thought.

**EXPOSURE.** Every received whole laddered coin (i.e. every fresh root coin) has a hostile height `L_{k−1} = coin.locktime + interval`, at which a **prior owner acting alone** broadcasts a 112-vB transaction already signed on their disk. No SE, no coordinator, no collusion, no race, no fee spike. Worst admissible case: `L_{k−1} ≥ tip + 101` — 16.8 hours — and every check passes silently.

**BOUND.** **Per-victim value, plain lane: real** — loss ≤ the coin's sats; attacker gain = sats − 112 vB. Forced by the exact-equality census (denying an undisclosed rung a hiding place) *together with* `payment_outputs == 1`. State the census as load-bearing; the output check is the second half. **Coloured/carrier lane: not constructible.** D35 caps the *attacker's gain* at carrier sats but says nothing about the victim's loss, which is carrier sats plus an allocation that is **burned**. An attacker-gain cap is not a victim bound. **Deadline height: exactly derivable** from `coin.locktime + interval`, both already typed and compiled-in.

**The claimed time bound is wrong and the proposal contradicts itself on it.** "The victim's exclusive window is exactly interval = 100 blocks" understates the defence by up to two orders of magnitude: T is un-timelocked, so the window is `[receipt, L_{k−1})` — minimum 101 blocks, typically the epoch remainder, up to ~10,000 blocks. The 100-block figure is only the width during which the victim's own nLockTime'd rung is live and no hostile rung is.

**THE FIRST PASS SAID** "the design's own offline mechanism structurally cannot cover this clock," enumerated four sites, and proposed building a new second-rung derivation, arming the tower with the flat backup, and gating whole-coin admission. **It missed the fifth site, which is built for exactly this clock.** `refresh.rs:514-516`: `coin_near_final(c, tip, margin)` tests `c.locktime − tip ≤ margin`, and for a received laddered coin `coin.locktime` **is** `L_k` (set at transfer_receiver.rs:1718-1720, stored :1764). `auto_refresh_due` filters by it with **no branch gate and no ladder gate** and re-anchors each hit — the 112-vB spend that discharges the absolute clock off-chain. Default margin 144, so it fires at `L_k − 144 = L_{k−1} − 244`, about 40.7 hours before theft. The defect is therefore not "no coverage" but **"the correct pass exists, is correctly keyed, and is not scheduled"** — `background_auto_refresh: false` on both presets, so it runs only via `auto_refresh_before_spend`.

And the miss **inverts which lane is exposed.** `auto_refresh_due` explicitly excludes carriers (`refresh.rs:433`) because a plain re-anchor destroys the allocation, and never falls back to `colored_reanchor`, which no automatic pass calls. With `colored_ladder: false` shipping, D35's plain-ladder arm makes a carrier's flat backups plain too — so T, the owner's own rung, and auto-refresh **all** burn the allocation. The token carrier is the lane with zero automatic coverage and no cheap non-destructive discharge. The proposal reported it as the lane where the bound is tightest.

Two further corrections. Forcing "its own flat backup or refresh" instead of T is refuted by the sentence immediately above the one the proposal quotes: receiver.rs:700-702 — "once T confirms, F is spent and every flat backup is dead forever — so a holder who force-exits the instant the deadline nears is safe with a single block of headroom." 2,160 blocks is the time to get the *value* out, not the time to be *safe*. And the flat backup is v2 with no P2A anchor, so it can be fee-bumped by nobody; T is v3/TRUC with a 240-sat anchor and is the only instrument on this clock anyone can bump. Re-anchor cost is **112 × 5.256 ≈ 590 vB/coin-year**, not the ~1,900 stated.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) Schedule the pass that already exists** | Make the deadline-safety half of `auto_refresh_due` unconditional in `start_background`, separating "routinely re-anchor to stay transferable" (may stay opt-in) from "re-anchor a coin approaching a rung a prior owner can broadcast" (theft prevention, not an economics preference). Users lose the "0 vB idle rent" headline — replaced by ~590 vB/coin-year — and gain a countdown in the UI. |
| **(B) Give the tower the predicate it lacks** | `build_watch_bundle` already ships `push_txs: tb.exit_tiers()`, and `exit_tiers()[0]` **is** the trigger. The tower already holds the discharging transaction; `deadline_block: u32::MAX` is the only thing stopping it acting. Set `deadline_block = coin.locktime + interval − margin`. Do **not** ship `backup_tx`: it grants a tower power to force an unbumpable on-chain sweep. |
| **(C) Cover the carrier** | Route carriers to `colored_reanchor` from the automatic pass, or state normatively that a plain-laddered carrier has no non-destructive discharge and gate its admission on that. |
| **(D) Whole-coin admission gate (H)** | Refuted. receiver.rs:700-702 declines to generalize the leaf's gate to a shape with an un-timelocked escape, and SOUNDNESS-HUNT already ruled the same way. With H = 288 the last ~3 hops of every 100-hop ladder die by construction. If any floor is wanted, derive H from one spend of F needing one confirmation plus fee tolerance — single-digit blocks — not from the auto-exit margin. |
| **(E) Re-anchor at every claim** | Makes INV-27 true as written and deletes the product: every transfer becomes on-chain. Must be written into the spec as the option *not* chosen, or the retained flat ladder reads as an oversight rather than as the price of the free hop. |

**RECOMMENDED: (A) + (B) + (C), with (D) dropped and (E) named-and-rejected.** The forced action is **T**, not the flat backup — un-timelocked, unilateral, SE-free, allocation-preserving on a coloured ladder, and the only fee-bumpable instrument on this clock.

Do **not** build the "second-lowest nLockTime" sibling: it is the same number by a longer route and breaks at k = 0, where "fewer than two entries is an ERROR" collides with "None must mean k = 0" — reintroducing the error-vs-absence conflation the `[C1]` tri-state comment records as previously fatal. Distinguish k = 0 by `backups.len() == 1`.

Also do **not** adopt "for an RGB carrier, refuse to export by name": the refusal it points at `Err`s out of the *whole* bundle, so one coloured carrier would leave every other coin in that wallet unwatched.

---

### 5. R-1 / R3 / D31 — the defence is fee-frozen at 2.0 sat/vB, and two of three lanes cannot bump (idx 1, 8, 9 — one capability, three faces)

**THE DECISION.** Whether to wire the existing D31 bump path into the lanes that lack it, whether to raise `committed_fee_rate`, and whether to fund the P2A anchor as a rescue bounty.

**EXPOSURE.** `fee_bump: None` on both shipped presets, and `FeeBumpConfig` demands a Bitcoin Core RPC endpoint plus a hot fee key plus a float split into one confirmed UTXO per simultaneous rescue. `exit_child_pass` (tesr.rs:7097), `watch_child_pass_seen` (:9908), `exit_spine_tip_pass` (:1341) and `watch_spine_tip_pass_seen` (:1282) take **no** `BumpCapability` and call bare `transaction_broadcast_raw`, while the SDK calls them unconditionally (wallet.rs:2758, :2818, :3548, :3578) three lines from two sibling arms that *do* consult `fee_bump_parts()` under a comment naming D31. A CATS-B payee — the party with the most motive — cannot be rescued at any configuration. `cosign_detrigger` (tesr.rs:9161-9173) and `cosign_colored_detrigger` pass `bundle.fee_rate` and take no live-rate argument.

**BOUND.** **Post-D14, the contested lane carries a real bound and the first pass denied it.** `77807e1` landed `RivalKind::margin` (tesr.rs:10440-10453) returning δ = δ_E = **36 blocks** on mainnet, keyed *structurally* off the live rival's tree position, enforced at :10686-10687. **The quantity the adversary cannot exceed is 36 blocks of CSV maturity, and what forces it is that maturity is not purchasable** — a fee bump moves confirmation priority, never a relative timelock. A prior owner with unlimited budget and a Core node still loses by 36 blocks. Per-victim loss in the contested lane is **zero** absent a >36-block reorg.

**Three published bounds are fake.**

- *"The deadline lane is a loss channel under a fee spike."* Refuted by its own REAL #1: `max_fee_rate: 1.0` is hard-coded at `client_config.rs:70` in the SDK's only constructor, and the **prior owner's rung is built by the same code under the same cap**. At floor ≤ 1.0 both relay and the holder's anchored 250-sat T wins; at 1.0 < floor ≤ 2.0 the holder's T relays and the attacker's backup does not; above 2.0 neither relays. There is no floor at which the cap hands the coin to the prior owner. The cap is a **symmetric disarmament** that disarms the attacker strictly more.
- *"Damage:cost ratio ≈ 0.4 — griefing is economically losing"* (PROTOCOL.md:401). A vbyte ratio, and the r→∞ asymptote of the SE-alive case. In sats at r ≤ 2.0 the attacker pays 0 and the victim pays 980; the sign flips at ~8 sat/vB, and above 2.0 the de-trigger cannot be built at all. There is no rate at which 0.4 is true.
- *"Anyone can spend the anchor"* as any kind of rescue permission. The child's change must clear `CHILD_CHANGE_DUST = 330` (p2a_fee_child.rs:51, enforced :167), so an anchor-only child (95 vB, package 220 vB) is profitable only at r ≤ (A−80)/220 = **0.727 sat/vB** at A = 240 — below the committed 2.0. **The profitable band is empty at every rate at which a rescue is needed.** This is strictly stronger than both the discredited "~900×" and the proposed 47-sat window. And the only builder in the tree hardcodes two inputs, so an anchor-only child cannot be built with repo code at all.

**THE FIRST PASS RECOMMENDED (idx 1) a scaled P2A rescue bounty, A = clamp(V/100, 240, 10000). That is refuted, not merely costly.** The anchor is adversary-agnostic: in this scenario there are two stuck conflicting tiers over the same outpoint carrying the **same** bounty, and a bot cannot tell them apart. It (i) hands the attacker a free rescue, destroying the analysis's own attacker-cost floor; (ii) subsidises the attacker by 2A directly, because the anchor is *recovered* in the child's change (p2a_fee_child.rs:166) — at r=100, A=10,000 the attacker's two-package outlay falls 54,620 → 35,100; (iii) is pure downside above ~45 sat/vB, where no stranger can profit but the subsidy still applies; (iv) costs ~20.6% of the coin on a depth-10 exit (23 anchors at ~1%, compounding) for every coin ≤ 1,000,000 sat, landing the haircut on exactly the split-child path CATS-B exists to make safe; (v) turns a free grief into one that burns A sats per broadcast tier; (vi) requires a rescuer client that does not exist.

**A second first-pass overstatement.** "§5.8's entire mitigation cannot confirm above 2 sat/vB" is a wiring gap dressed as an impossibility. `build_detrigger` routes through `build_tier_tx`, so **the de-trigger carries a 240-sat P2A anchor** — `broadcast_tier` locates it structurally and can lift it to a live package rate with no fresh SE signature and no census change. The only genuinely unbumpable window is while T is still unconfirmed (TRUC caps the cluster at two), and in that window no rival's CSV has started. The real defect is one un-wired call site: `refresh.rs:168` broadcasts the de-trigger with a bare `transaction_broadcast_raw`.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) Wire the existing bump path into all lanes** | Thread `BumpCapability` through the four passes, consult `fee_bump_parts()` at the four SDK sites, route the de-trigger and re-anchor through `broadcast_tier`, fix the inverted flat CPFP (`broadcast_backup_tx.rs:70` broadcasts the parent under `?`, making the child unreachable in exactly the case CPFP exists for). Costs users who decline: nothing — `fee_bump` stays default-None. Requires a zero-CSV successor rule: on the spine, `SPINE_CSV = 0` lets consecutive tiers co-exist unconfirmed and a fee child would consume the single TRUC slot. |
| **(B) `committed_fee_rate` 2.0 → 3 or 4 or 10** | The only lever that shrinks the stall band without handing the attacker anything, because it moves both ladders identically. **Its cost is not what was claimed:** `tokens.rs:6252-6259` and `:6285-6294` *fail* on the change — keeping `PIECE_FEE_RATE_HEADROOM` forces TOKEN_PIECE_SATS 3,066 → 5,082, stranding every piece in circulation; dropping the headroom leaves every piece at exactly the floor with zero margin on a coloured tier vsize that has already drifted 167→168. At c=10, `min_child_value` 1,310 → 3,310 and a 10,000-sat piece reserves 29.8% of itself. **c = 3** covers the regime the only measurement in the corpus was taken in (Core's default floor is 1 sat/vB; WP1 had to *force* 3) at a 26% floor rise. |
| **(C) Live-rate de-trigger + superseded-de-trigger census term** | Small in code, large in care: each attempt burns an irreversible SE co-sign, so the census term must land in the same commit or a failed rescue bricks conveyability. Hardening, not the live gap. |
| **(D) Promote the funded tower to normative** | Supplies what a keyless tower structurally lacks — a key and a confirmed UTXO — without subsidising the rival, because the float is spent selectively. Exposure already bounded: the fee key "is a fee key, never a coin key." Note the Core RPC is *not* the irreducible barrier (opportunistic 1P1C relay since Core 28 makes `submitpackage` a convenience); the barrier is a key plus a confirmed UTXO. |
| **(E) Scaled bounty anchor** | Refuted above. |
| **(F) Re-anchor every h hops to delete the rival stock** | At h=1 rows 1–3 of the race table cease to exist, δ stops being safety-critical, and the retained-flat-backup calendar disappears. Costs ~660 vB/coin-year at 5.26 transfers — 11% of the pre-TES-R rent, not 0 vB. Makes every h-th transfer SE-dependent, which fails precisely under the dead-SE premise. |

**RECOMMENDED: (A) + (D) + (B at c = 3) + (C), with (E) rejected on the record and (F) declined.** (A) is the free fix and makes D31's own recorded decision true; (D) is the only capability that helps a delegated offline holder without arming the rival; (B) at 3 buys headroom over the *mempool floor* (the actual trigger variable) at a 26% floor rise rather than 153%.

**Sell (A) as plumbing parity, not as closing a theft channel.** Post-D14 the un-rescuable lanes cost *latency*, not coins. Overclaiming here is what the next reviewer catches.

**Delete, don't repair, TRUST-MODEL's "the current owner racing with a fee-bump wins in practice."** T-SE-1's adversary is SE + old owner, whose rival is a fresh un-timelocked spend at market rate, facing no floor. No fee-bump capability, funded tower, or bounty makes that sentence true.

---

### 6. C-1 / VE-1 — pricing a piece's exit against a future fee rate, and the split-tree cadence (idx 3, idx 10)

**THE DECISION.** Whether to raise `committed_fee_rate`, add a stress-priced viability gate, add a minimum-slack admission margin, or publish the band and build nothing — and separately, what footprint numbers §7 may publish.

**EXPOSURE.** A piece admitted at `min_child_value = 1,310 sat` has no flat backup of its own (`CHILD_V2_BASELINE = 0`); its only route to Bitcoin is a 3+2d tier walk. `check_exit_headroom` admits at slack == 0 and a test pins that case, while `exit_wait_blocks` charges each tier exactly one block per parent confirmation — its own doc calls that "the theoretical minimum, not a budget." The slack the gate returns is the piece's *entire* tolerance for confirmation variance, fee stall and reorg across the whole walk, and **the sender chooses it by choosing when to convey**.

**BOUND.** **Per-admission: real** — loss ≤ the piece's **face** value (the sats SP.out[j] carries), not V − 2·rung. The 2·rung is the portion going to miners rather than the attacker: a distributional fact, not a reduction in loss. And the wallet books face value anyway. **Per-tree: real and tighter than claimed** — total loss ≤ F less the sender's change, victim count ≤ **min(floor(F/1310), 64 per parent)**. **Aggregate: constructible and UNEVALUATED, not unbounded** — the loss set is `Σ V_i · 1[V_i < V_min(d_i,r)]`, monotone in r and **saturating** at the piece lane's total value. The first pass had this backwards on the one bound meant to be novel, after demanding the distinction in its own framing.

**Three model errors inflate every alarming cell.** (i) The TOPUP formula evaluated *at* c gives 330, not zero, contradicting the proposal's own capitalised "below c the top-up is ZERO BY CONSTRUCTION" — the object is a **step**, and "continuous break-even r* = 2.00001" reads a step function as a curve. (ii) `r` is the **mempool floor**, not the fee estimate: a tier committing 2.0 sat/vB relays at any mempool minimum ≤ 2.0, and Core's default is 1 sat/vB — WP1 had to force 3. "Under water between 5 and 10 sat/vB" means *floor* sustained there across a 2,885-block walk, not "when fees are 5–10." (iii) The formula charges a CPFP child to all N(d) tiers at one constant r across a walk spanning 2,885–9,383 blocks. Priced at the margin, a floor piece rescuing the one stuck tier pays `ceil(278·(2+ε)) − 490 = 67 sat` against 330 reachable — it can afford four.

**Also on VE-1 (idx 10): the claimed footprint bound has a fake denominator and a fake rate.** "639 vB per resulting coin per epoch" is a whole-tree total divided by 11; a depth-1 payee materialising alone pays the entire 668-vB shared prefix plus ~111 to re-deposit. And nothing forces one tree per epoch — a depth-1 walk completes in 2,885 blocks, so a coin can be rebuilt ~3.4 times inside a 10,000-block epoch. The epoch bounds tree *depth*, not rebuild *frequency*. This is a cost/capacity figure, never an adversary bound.

**And the "5,840 vs 0 is an epoch-dial artifact" claim is refuted by the corpus one screen away.** PROTOCOL.md:123 publishes the baseline's "unilateral exit wait ≤ ~7 d" — in the pre-TES-R absolute ladder, `initlock` **is** the exit wait. Setting the baseline to 10,000 gives a 69-day exit wait: a different, unusable instrument. TES-R's un-timelocked T is what decouples exit latency from epoch length and licenses initlock = 10,000, at a cost the corpus also publishes (root exit wait rises to 2,163 blocks ≈ 15 d). The calendar-axis win is **real and ~10×**, not zero.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) Publish the band; build nothing** | Zero engineering. The partial-payment lane stops being sold as a bearer instrument below a value that depends on depth and assumed floor. Leaves the wallet's booking and the child-lane bump gap standing underneath the honest text. |
| **(B) Raise `committed_fee_rate`** | See decision 5 — same lever, same costs. c = 10 quintuples every floor to insure a 67-sat marginal exposure. |
| **(C) Stress-priced viability gate** | Adds `stress_fee_rate`, a number nobody can derive, to the trust base. At stress = 20, depth 10 the minimum admissible piece is ~125,330 sat, killing the entire simulated payment range. Prices a rescue that on the child lane no shipped code performs. |
| **(D) Minimum-slack admission margin** | Refuse a conveyed child whose `check_exit_headroom` slack is below a margin proportional to its own walk. **Every input is already receiver-derived** — no new exogenous constant, which is exactly what disqualifies (C). Closes the sharper clock and removes the sender's free conveyance-timing lever. Cost: refuses late-in-epoch conveyances, forcing re-anchoring — a liveness cost, in the direction ADMISSION-INPUTS already names as safe. |

**RECOMMENDED: (D) + (A)'s normative sentence.** The proposal's own ranking calls the stall clock "new and sharper" and then recommends the one option that admits it "does not touch the stall clock" — the menu was incomplete, and the missing option is the cheapest and the only one with no guessed constant in it.

Ride mandatorily: **"a piece below V_min(d, r) is cooperatively redeemable, not unilaterally exitable."** This is the sentence the analysis supports and its own "no adversary is required" framing contradicts — while the operator is live the piece transfers at full face value with no on-chain walk and no fee exposure at all.

**Unconditional code fixes, two of three.** Wire `exit_child_pass_with_bump`/`watch_child_pass_with_bump` (the most valuable item in the row). Export the shipped 153-vB child vsize as a named constant and recompute every published figure from it — *after* correcting the step-vs-curve model. **Reject the third as written:** re-booking `coin.amount` from `sp_out.value` to `cb.child_state.out_value` would make the child lane the only lane in the wallet reporting net-of-unilateral-exit value (whole coins book `new_key_info.amount` without deducting ~1,470 sat of rungs; deposits book the deposit amount), and would under-report by 980 sat exactly the value a *cooperative* transfer moves — the piece's primary redemption path under the recommended normative sentence. Fix the inconsistency by making the peek path report face value, and add net-of-exit as a separate field for all lanes.

---

### 7. D10 — the census's distinctness premise (idx 4)

**THE DECISION.** Whether to publish A11 as an assumption, prove it as a theorem, build SE-side co-sign indexing, or add a distinctness check.

**EXPOSURE.** As stated in D10 (blinding commitments unverified on the laddered lane): **zero**. The census needs a *cardinality*, not a slot index, and `verify_blinded_musig_scheme` establishes only which session signed which transaction.

**BOUND.** **Per-victim at HEAD: zero sats, and it is real** — but forced by a **field contradiction**, not by a parameter. Backup vs live tier is refused **five** ways: nVersion (tiers `version: 3`, backups must be 2), nLockTime (tiers 0, backups a height above tip), nSequence (backups exactly `Sequence(0)`, trigger `0xFFFF_FFFD`), output count (every live tier carries a P2A anchor beside its payload; a backup must have exactly one non-OP_RETURN output), and sighash amount (`verify_transaction_signature` builds `Prevouts::All` from `tx0.output[vout]`). Backup vs superseded tier is refused **three** ways, and the strongest is **parameter-free**: `live_by_outpoint` is seeded by `for i in 1..txs.len()`, excluding the trigger, so the funding outpoint a backup spends is never a key — the backup is neither out-raced nor transitively dead and is refused as an orphan (tesr.rs:10726-10731). No CSV, no schedule, no fee rate enters that decision.

**THE FIRST PASS SAID** the backup-vs-superseded pair is "blocked ONCE, by `csv < lo`," that `SPINE_CSV = 0` puts this "one parameter away" from a full-coin theft by a malicious sender alone, and that the fix is to seed `seen_txids` with the flat-backup txids. **All three are wrong.** (a) It missed the parameter-free orphan refusal forty lines further down the same function — widen the superseded band to admit 0 and nothing changes. (b) The parameter it calls fragile is itself bound: `cap_schedule` (tesr.rs:6574-6592) compares d0/δ/d_floor/e0/δ_E/e_floor/m_max field-by-field against the receiver's own preset and refuses, on both acceptance paths *before* the census. It read A11's row and never read A8's — whose stated residual ("`cb.parent.params` is STILL a sender-declared serde field") is itself stale for the same reason. (c) The proposed check **cannot fire**: `Sequence(0)` for a backup vs `csv >= d_floor = 144` for a superseded tier is one field with two mutually exclusive required values. Its own adversarial test is unwriteable against a genuine dual-role transaction and can only pin the description — the exact failure shape this repo has shipped before. (d) Billing a maintenance-conditioned hypothetical as "a malicious sender acting alone, the cheapest adversary in the model, loss = full coin per hop" is a mis-attribution that discredits the section.

Also: the ℓ = 1 concurrency property is **real** (single `sealed_secnonce` column, overwrite on reissue, read-and-NULL inside one `SELECT … FOR UPDATE`, NULL refused) — but "so it holds at the ~2^128 generic level" is fabricated. Sequential blind-Schnorr one-more-unforgeability reduces to ROS₁ hardness, itself an assumption. **UNEVALUATED-BY-ASSUMPTION, not a computed security level.**

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) Accept A11 as a named assumption** | Zero engineering. Understates the design — it is a theorem with three enforced premises. |
| **(B) SE-side tx_n ↔ co-sign indexing** | Destroys blindness. `secp256k1_blinded_musig_partial_sign_without_keyaggcoeff` exists precisely so the SE cannot learn a session's role, and a client-declared role is attacker-supplied. Highest price for the least. The roadmap's "excluded scope" justification is stale (D22); the merits objection is not. |
| **(C) Distinctness as a new `seen_txids` check** | Touches four signatures, three call sites and the child twin to install a refusal no transaction can reach, plus a test that pins a description. |
| **(D) Prove A11; fix the ordering; spec the separators as shape obligations** | One small ordering change plus documentation. The claim path feeds the census `transfer_msg.backup_transactions.len()` **raw** (transfer_receiver.rs:1441) and validates the chain only afterwards (:1488); the pre-pay path already does it right and says so. |

**RECOMMENDED: (D).** The one real defect is an ordering asymmetry, not a missing set: move `validate_backup_chain_v2` ahead of `verify_bundle_bound` on the claim path and feed the census the validated group, so the term is "earned before spent" on both doors.

Promote A11 to a **CENSUS COMPLETENESS** theorem with four premises (A3; no CO-1; blind-signing concurrency = 1 per key; pairwise distinctness). Discharge premise four not by a new check but by a **shape obligation** in §3 — a flat backup is nVersion 2 / nSequence 0 / height nLockTime above tip / exactly one non-OP_RETURN output; a tier is nVersion 3 / nLockTime 0 / exactly one 240-sat anchor / CSV in its bound band / provably non-confirmable if superseded — stating that these carry a **census** obligation and not only a relay/race one. That is where a future change would actually break it, and it costs nothing.

Keep the lockbox ℓ=1 comment and CI guard (genuinely good), adding the two premises the first pass skipped: `statechain_id` is **not** UNIQUE in `generated_public_key` (the constraint is on `public_key`) yet `load_and_consume_secnonce` reads `result[0]`; and the KEYSTONE cache returns a signature without an increment on a client-chosen key.

**Grade D10 CLOSED, conditional on A3 and on the absence of CO-1**, with both names printed beside the census in §6.

---

### 8. Coloured lane — seal derivation and renewal scope (idx 5)

**THE DECISION.** Whether P2's per-output seal blinding stays, is made symmetric, or is reverted — and how to state coloured renewal.

**EXPOSURE.** The off-by-one is **real**: the bridge keys `output_blinding` by the pre-colouring vout (`rust-rgb/src/lib.rs:509`, `:630`) while the fork shifts before lookup (`if opreturn_first { vout += 1 }`, then `output_blinding.get(&vout)`), and `opreturn_first` is true whenever any output is P2TR. Keys {0..n−1}, lookups {1..n}.

**BOUND.** **Fake as stated.** "Exactly one stranded allocation per coloured split" quantifies stranding on a path where **the receiver never consults a blinding to book**. Both production accept paths call `extract_received_assignments(consignment, witness_id, Some(vout), known_concealed = None)`; with `None` the `ConfidentialSeal` arm can never match, and the only arm that fires matches `Assign::Revealed` **by vout, blinding not inspected** — which is why `accept_offchain_amount` has no blinding parameter at all. The named failure site cannot fire either: `assigned` and `received` come from the *same* function, *same* consignment, *same* (witness, vout), both blinding-independent. The legacy/default lane's receiver reaches only `accept_offchain_amount`; the proposal conflated `accept_transfer` (the blind-receive flow) with the legacy split receive path.

What survives is a **derivation asymmetry**, whose cost to spendability is **UNEVALUATED** — one sdk75/sdk77 run away. And the privacy sub-bound is mis-attributed: `known_concealed: None` at every Mercury site with no conceal step anywhere in the fork's `rust_only.rs` suggests consignments carry Revealed assignments for every payload, in which case per-output blinding buys **zero bits for everyone**, not just for the holder of the highest vout.

**THE FIRST PASS SAID** this is a live default-lane regression and therefore the urgent arm. **It is not; nothing is bleeding.** Three of four arms sit behind `colored_ladder = false`, and so does the fourth to the extent it bites.

**THE MOST USEFUL CONTENT IN THIS ROW SURVIVES INTACT.** CR-D landed (`build_colored_detrigger` tesr.rs:2147, `cosign_colored_detrigger` :2929), so D30:515's "no builder exists" is stale. But `colored_reanchor` pays `get_user_backup_address` — a **single-sig P2TR off the statechain** — so it is a coloured **exit**, not a renewal; re-entry needs a fresh deposit. Nothing calls it (`auto_refresh_due` skips carriers by design). It broadcasts **two** transactions (trigger then de-trigger), so a coloured coin's idle rent is **336 vB per 10,000-block epoch ≈ 1,767 vB/coloured-coin-year**, roughly 3× a plain coin's, against a 0-vB-rent headline.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) Symmetric per-output derivation** | Three files plus the fork. Only worth doing if per-output blinding conceals anything — and the evidence says it may conceal nothing. Building it now ships an assertion of a property with no test behind it, guarded by source pins in a repo that has already shipped a pin matching a string literal in its own source. |
| **(B) Revert P2; re-open the K>1 privacy prerequisite** | Strict deletion of two lines. Restores sender/receiver agreement on `base` immediately. Costs users nothing they have. Forces honest bookkeeping: un-tick the seal-privacy prerequisite in D30 and §4.7, and resolve the self-contradiction between `refuse_colored_multi_payee`'s doc comment (pre-P2 story) and its own refusal string (claims the gate is closed). |
| **(C) CR-D as real renewal, and drive it** | Destination becomes a fresh statechain 2-of-2 returning a `new_statechain_id`; `auto_refresh_due` dispatches carriers. Makes the rent visible: 336 vB per 69.4 days on an architecture sold on 0 vB. |
| **(D) Accept, flag off, publish as-is** | Rejected — but for the honest reason (RGB is the differentiator and the flip is gated on unscheduled work), not the proposal's reason, which rested on the refuted default-lane claim. |

**RECOMMENDED: (B) now as a same-session deletion, with (C) sequenced.** The correct *first action* is not a code change at all — **run sdk75, sdk77 and sdk29 against HEAD.** They are the only things that distinguish "P2 is a harmless no-op," "P2 breaks coloured-ladder spendability," and "P2 buys no privacy," and `25a4037`'s "685 green" is a unit count touching none of them. Ship (B) in the same session so the tree is not left in the asymmetric middle.

Doc corrections all stand: D30:515 stale on CR-D; D30's prerequisite table stale on two of three rows; COLOURED-SPINE:647 quotes the **regtest** 1,000-block figure as "deployed" (deployed is 10,000 ≈ 69.4 d); :668-671 stale.

---

### 9. R13 — conveyed child CSVs: admitted band or derived value (idx 2)

**THE DECISION.** Whether the receiver's `[144,720] × [144,1440]` band on a conveyed child's CSVs is replaced by a derived equality, a grid law, a raised floor, or a value gate.

**EXPOSURE.** A malicious **sender alone** — no SE, no coordinator collusion — picks the payee's CSVs anywhere inside the band the payee's own verifier admits. Every check passes; nothing surfaces the budget.

**BOUND.** **Real, but far smaller than stated, and it is the remedies that force it.** The first pass claimed a d_c = 144 leaf is "born terminal, refusable by its own wallet the first time it is spent." Refuted by the very error string it quotes: `renew_child` (tesr.rs:22560) exists, is E2E-tested (sdk84), does a **full state reset to 1440** for zero on-chain bytes and zero depth, and `sdk84:597` asserts the floor refusal *names* `renew_child`. Second remedy: `child_in_ladder_split` has one CSV precondition (`old_csv <= SPINE_CSV`) and mints grandchildren at a fresh 576-hop schedule for one depth level and ~980 sat, given V ≥ ~2,290 sat.

So: d_c floored costs **36 of 576 hops = 6.25%**, undone free. The honest bound is per-victim loss ≤ min(V, ~980 sat + the option value of one depth level of 10) for V ≥ ~2,290, and ≤ V only in a **triple-conjunction window** [1,310, ~2,290) where the piece is simultaneously below the self-split threshold, at m_max or off-grid on the extension, and at the state floor.

**And the honest baseline is 576 hops, not 36 — PROTOCOL.md's own constants table two rows above the line the proposal quotes says "Hop budget per depth level | 36 × 16 = 576 transfers."** The proposal demanded that line be amended because "no document states it."

**THE FIRST PASS AIMED AT THE WRONG PARAMETER.** The sharpest lever is not the band's lower edge — it is the **extension grid**. `child_renewal_epoch` derives m = (e0 − e_c)/δ_E with **exact divisibility required**, and refuses an off-grid rung by name and `csv <= e_floor` forever. The receiver's band has **no grid check**. So a sender mints e_c = 719 — one block below the head, cosmetically generous, admitted everywhere — and the leaf can **never** renew: 36 hops instead of 576, a **93.75%** loss. The proposal's framing misses the attack that actually costs 540 of 576 hops. (Its renewal formula is also off by one at every point: remaining = (e_c − 180)/36, not floor((e_c − 144)/36).)

**And the headline rationale is already implemented.** "The bands read `cb.parent.params`, the conveyed schedule — the one place the forged-yardstick discipline was not applied" is false: `verify_conveyed_child` calls `cap_schedule` at :7464, which hard-errors on any field differing from the receiver's preset, and `verify_child_bundle` is called at :7514, *after* it. By the time the bands read `cb.parent.params` that struct is provably field-for-field the authority preset. The quoted `cap_schedule` error text is a stale to-do, not a live hole.

**Option A as written also misfires on a legitimately renewed leaf.** Its discriminator is "the child half discloses superseded states," but `renew_child` adds +2 superseded entries while resetting csv_d to 1440 — so a renewed-but-never-re-transferred leaf is classified as a re-transfer and must satisfy `d_c == lowest rival − δ`, over an outpoint whose rival set is **empty**. Undefined for every renewed leaf. "Takes away nothing that exists" is false.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) Extension grid law, consumer-side** | Run `child_renewal_epoch`'s law (or its pure core) inside `verify_child_bundle`'s `[F4]` extension block on **every** conveyance, no branch. Recovers 540 of 576 hops. Cannot be dodged on a re-transfer, which Option (B) can. |
| **(B) Fresh-mint equality via census discrimination** | Require m == 0 and d_c == d0 on a fresh mint, discriminated by the **exact-equality census count** (a fresh child expects 2; a renewed leaf has superseded *extensions*; a re-transferred leaf has superseded *states*) — exact, because a sender cannot omit a superseded tier without failing the census. |
| **(C) Minimum d_c on re-transfer** | Rejected. `child_supersede_csv` lets an honest final hop convey d_c = 144, and that leaf is not dead — both remedies take it. A floor strands honest coins for a remediable harm. |
| **(D) Declare-and-verify remaining budget** | Hands the payee a knob with no principled setting — the shape that made D19's polling interval a per-implementation guess. |
| **(E) Price the piece against its budget** | Deletes the sub-economic lane the design was selected to open, for pieces the sender deliberately crippled. Wrong party pays. |

**RECOMMENDED: (A) then (B), with (C)/(D)/(E) rejected, plus surfacing the budget at claim.** `child_needs_renewal`, `child_needs_rollover` and `child_renewal_epoch` are public and called from **no production path** (only sdk84). Returning `(hops_remaining, renewals_remaining)` and refusing a bundle whose epoch is **underivable** converts this from silent to visible without inventing the policy knob that sinks (D).

Keep two doc corrections: re-title PARTIAL-PAYMENT-ECONOMICS' "payee's total on-chain warning" table as walk latency (its own correction block establishes the splitter's retained backup steals with **zero** on-chain predecessor), and retract DECISIONS.md:174's "Implementation is WP3c" for D14 — live at tesr.rs:10686-10687. Do **not** amend PROTOCOL.md's decrement line; amend the item, whose "36 onward hops" is 16× low. Do **not** publish the D19 "180 blocks" figure — it has no derivation, and the smallest CSV in any CATS-B bundle is `SPINE_CSV = 0` regardless.

---

### 10. TRUC slot contention — who may take a tier's single child slot (idx 13)

**THE DECISION.** Whether to key the P2A anchor, pre-occupy the slot, raise the committed rate, or accept the squat and make the rescue conflict-aware.

**EXPOSURE.** Every tier carries exactly one anyone-can-spend 240-sat anchor; TRUC gives an unconfirmed v3 transaction exactly one unconfirmed descendant (measured: `descendantcount == 2`). Anyone — no keys, no coin, no relationship — takes the slot. The cheapest legal squat is **65–66 vB** (Core enforces `MIN_STANDARD_TX_NONWITNESS_SIZE = 65`, so the proposal's 61 vB is non-standard), self-financed entirely by the victim's anchor at a ceiling of **240/65 = 3.69 sat/vB**, not 3.93.

**BOUND.** **The proposed delay bound is fake, three ways.** (a) It compares a whole-chain aggregate against a per-outpoint budget: δ is enforced inside `verify_superseded_segment` under `if let Some(live) = live_by_outpoint.get(&sup.prevout)` — the separation between **one** live tier and **one** rival over the **same** prevout, not a head start for the walk. Because the CSV is *relative*, a squat delaying rung k−1 delays the live tier and its rival **equally** (the rival is also a TRUC child of the same unconfirmed parent, blocked by the same occupied slot). Upstream delay is race-**neutral**. "36 ≥ 23 with 13 blocks to spare" is two unrelated numbers. (b) It measures the wrong harm: the squat removes the **CPFP handle**, and converting that to blocks needs to know how long a 2.58 sat/vB package sits unmined — a fee-market quantity. (c) The shipped loop broadcasts each tier and continues on `Plain`, so on the CSV-0 spine SP_i and SP_{i+1} are in the mempool together and the attacker usually *loses* the slot; where the attacker reliably wins is the **terminal tier and every CSV-separated rung**, whose successor is 144+ blocks away — exactly the hops the proposal declares undelayable. The harm profile is close to the inverse.

**Real bounds, all forced.** (i) Attacker outlay is exactly **0 sat** below a 3.69 sat/vB floor; above it, the squat costs the same package price as the honest rescue, removing the asymmetry. (ii) **The pin is one-shot**: RBF may only *raise* a fee, so an attacker pinning below the next-block threshold cannot lower it when the market falls — one dip confirms the tier. Forced by BIP-125 rule 3. (iii) Per-victim ceiling is the full coin, and it is **captured, not burned** — a maturing superseded tier pays the prior owner (tesr.rs:10670, "could mature first and steal"). The proposal's "never a forfeiture to the operator, every tier pays only the owner" is true of the operator and false of the adversary it itself names as sharpest. The **composed attack** is one actor: squat the live tier's anchor (removing the honest CPFP), wait out δ at that rung above a 2 sat/vB floor, then CPFP your own rival, whose anchor nobody squatted.

Bound 2 (rescue surcharge) is structurally sound — a pin above the next-block threshold gets mined and confirms the tier it was pinning — but arithmetically wrong (the ceiling is on the **package**: ~1125r − 97, not 1000r + 153) and it bounds the price of a fix that `fee_bump: None` cannot buy and `build_p2a_fee_child` cannot compute.

**A finding needing no adversary, more likely than the squat:** the greedy `continue` puts SP_i and SP_{i+1} in the mempool together, and that cluster is un-bumpable in **both** directions — SP_i's slot is taken by SP_{i+1}, and a child of SP_{i+1} would have two unconfirmed ancestors. If the floor rises after both relay, the owner has stranded their own walk with nobody attacking.

**OPTIONS.**

| Option | Real cost |
|---|---|
| **(A) Keyed rescue anchor** | +90 sat/tier (330 dust vs 240 standardness): +450 on a 5-tier flat exit, +2,070 at the 23-tier cap, signed into every tier forever, raising V_min across the whole sub-economic table. **The disqualifying cost:** in a split tree the leaf owners are strangers who share ancestor tiers — today any of them can CPFP a shared ancestor they all need; a keyed anchor locks every sibling out unless the key becomes a 1-of-N over all leaf owners, a new key-distribution problem inside the split protocol. |
| **(B) Owner pre-occupies its own slot** | Makes Core RPC a precondition for a *normal* exit — electrum-only wallets, which is every wallet the SDK ships defaults for, lose protected exits. Protects only tiers the **owner** broadcasts, which is not the contested case. |
| **(C) Raise committed rate above 3.69** | Doubles the committed fee on every tier forever (500/rung, 11,500 for a 23-tier exit) to defeat only the zero-cost variant; 219 sat buys past it. D31 already rejected this lever. |
| **(D) Accept; make the rescue conflict-aware** | One extra Core RPC read on a path already requiring Core RPC; float sized ~3.6× larger per simultaneous rescue, making an honest tower look less solvent than `tower_float.rs` reports. Users lose nothing they have — but the spec must now say a contested exit during a spike requires an online, funded owner running Bitcoin Core. |

**RECOMMENDED: (D), with two changes to the proposal.**

**Delete its code change #7 outright.** `min(δ, δ_E) >= max_exit_txs(profile)` is a false invariant comparing a per-outpoint propagation budget against a whole-chain transaction count — it would boot-panic regtest (3 vs 139) for a hazard it does not describe while stamping mainnet safe on a coincidence of units. Writing it beside G3/D7's panic would make it load-bearing and unfalsifiable. Write nothing rather than write this.

**Add the missing change, more urgent than the squat:** stop greedily broadcasting SP_{i+1} the moment SP_i relays. Either hold the successor until the parent confirms, or submit `{SP_i, SP_{i+1}}` deliberately as a package with the decision documented at the site.

**Restate the residual honestly.** On the default install a tier whose anchor is taken cannot be bumped at all, and if the floor stays above ~2.6 sat/vB for δ = 36 blocks past that rung's maturity, the prior owner's rival matures and **takes the coin**. Full-value capture, not delay, with no bound in blocks on how long a floor stays up.

**Tighten the DECISIONS.md correction.** Squatting an *unconfirmed* anchor yields the attacker **zero** — all 240 sats go to the miner. The attack has no profit motive; it has a **zero-cost** motive. The honest sentence: "anyone-can-spend is a permission, not a revenue source — but denying the slot is free, and free is enough." Drop "harvesting turns a profit below 1.57 sat/vB": a single 240-sat anchor cannot fund a change output above the 330-sat dust limit, so harvesting requires batching two or more (the ~5.85 sat/vB figure already assumes that). And note that DECISIONS.md:568's "900×" rests on WP1's 180,330-sat child, a deliberately oversized test fee (177 vB at ~1,018 sat/vB) when the honest rescue needed ~754 sats — the corpus's central argument for "no stranger will bump" is propped up by a measurement artifact.

---

## Part 2 — What each answer implies in work

| Decision | Code changes | Tests | Spec sections unblocked |
|---|---|---|---|
| **1. B1** (A+E) | Surface "sever from F" (broadcast pre-signed T, 125 vB, no SE) as a first-class action; add head anchor to `validate_backup_chain_v2` (max conveyed nLockTime == `h_deposit + initlock`); return structural `k = (h_dep + initlock − min_locktime)/interval` from `epoch_deadline_from_flat_backups`; expose re-anchor-on-receipt as explicit user choice, failing closed on SE refusal and slot exhaustion with named errors. **Do not** compile θ; **do not** ship `flat_backups.len() − 1`. | Chain with top rung removed → refused; re-anchored coin has k == 0; T-broadcast retires all historical pairs; negative test on truncated chain. | SPEC-FOUNDATION §1 B1 row (:55-59) and prior-owner row (:23); G7 (:97) as corollary; G8 (:98) with named B1 exception; G9 (:99); PROTOCOL REQ-3 (:668-677) and :670, :672; TRUST-MODEL T-SE-1 (:184-200, delete :194); new **SE-STORAGE-NON-RETENTION** assumption; named-limitations opening; coloured chapter (D35 rationale must state B1 gives the same ancestor unlimited signing). |
| **2. CO-1** | Encrypt t2 to the pinned enclave key (re-specify the `sha256(t2_hex)` auth binding); lockbox host split (seed to a sign-only process); `generate_new_keypair` → derived `E_sid = M + H(tag‖M‖sid‖U0)·G` with sid+U threaded through; `/identity` route (ops only, never a verification input); compiled per-network SE identity table beside `utils.rs:41-61`; serve original depositor U0; rewrite `validate_tx0_output_pubkey` (and fix its three `unwrap()`s on coordinator data); demote `se_aggregate_pubkey` to cross-check; extend to child slots; **keep D34 PoP** for the live share. Delete `PermanentLicence::LegacyNoAggregate` and the NULL-aggregate arm. | Undecryptable-t2 fails closed; coordinator serving mismatched M refused; PoP forgery attempt under recovered o2 fails once t2 is confidential; keyupdate skipped/double-rotated → PoP refuses. | §7 census/receiver verification; §17 trust model (+ coordinator row in B-table); §1 scope and limitations; §3 crypto constructions (state that key-agg coefficients would **not** have helped); §18 SE interface; §19 coordinator API (served keys become cross-checkable); §20 deposit; §6/§11 child aggregate; §25 migration. |
| **3. O-1/CO-3** | Derive terminality on both admission paths from attested `has_sig_budget && num_sigs >= sig_budget`, delete `get_spend_budget` reads at tesr.rs:7496-7497 and :7506, keep coordinator answer as a **raw-quantity** cross-check refusing on disagreement; port attested reader + compiled `flat_ladder_params` to nodejs/web **or** make the fail-closed gate structural rather than keyed on sender-declared fields; comment ℓ=1 invariant at db_manager.cpp:80-81/:357 + CI guard; drop the four unread blinding fields (hygiene). | Malicious coordinator answering `terminal: true` for a non-terminal parent → refused (**no such test exists today**); two-store disagreement → refused; JS/web sender declaring `protocol_version = 0` with ladder fields omitted → refused; CI guard on secnonce cardinality. | §6 census (name A3 and CO-1 at the point of claim); **G2** (not G6) in the goals table; §14 SE-is-blind (record the blind-oracle + serialisation argument; reject roadmap option (b)); §3 assumptions (A11 folded, monotonicity retracted at :96 only); §5.11 R4′/R5′; §12 wire format; §7/TRUST-MODEL pre-pay census; DECISIONS D8 row + D8-SIGCOUNT-AUTH note + D28 narrowed to transport. |
| **4. R-2/T-1** | Make the deadline-safety half of `auto_refresh_due` unconditional in `start_background` (split it from the routine-refresh flag); forced action is **T**, never the flat backup; `build_watch_bundle` sets `deadline_block = coin.locktime + interval − margin` (ship no `backup_tx`); route carriers to `colored_reanchor` from the automatic pass or state normatively that a plain-laddered carrier has no non-destructive discharge; `auto_exit_due` must not skip laddered coins on `b.is_empty()`; `estimate_exit_cost` distinguishes k=0 by `backups.len() == 1`. | **Replace sdk30(a)** — it idles alice's own k=0 deposit and cannot witness the case where INV-27 is false; new: coin transferred ≥1×, idled past L_{k−1}, prior owner's rung broadcast, tower discharged; **token-carrier variant asserting the allocation outcome explicitly**. | §Invariants (INV-27 retraction per D36 across 11+ sites); §Liveness/watchtower (D19 — needs both clocks); §Refresh economics (publish **590 vB/coin-year**, not 1,900; invert SPEC.md:555 — refresh **is** the deadline reset); §Trust model A6 + B4; §Named limitations R-2, D12. |
| **5. R-1/R3/D31** | Thread `BumpCapability` through `exit_child_pass`, `watch_child_pass_seen`, `exit_spine_tip_pass`, `watch_spine_tip_pass_seen`; consult `fee_bump_parts()` at wallet.rs:2758/:2818/:3548/:3578; route de-trigger + re-anchor through `broadcast_tier` (refresh.rs:164-172); add zero-CSV successor guard; fix inverted flat CPFP (broadcast_backup_tx.rs:70); `cosign_detrigger`/`cosign_colored_detrigger` take live `fee_rate` floored at `bundle.fee_rate` **plus superseded-de-trigger census term in the same commit**; `committed_fee_rate` 2.0 → 3.0 with `tokens.rs:6252-6294` derivations updated; ship a fee-float story for the default presets or disclose that the default wallet cannot bid. | Regtest with `-minrelaytxfee` above the committed rate: keyless `watch_pass` returns `Stuck` with the D31 limit (not a retryable failure); `watch_pass_with_bump` confirms as a package; child-lane and spine-tip equivalents; **zero-CSV successor refusal on regtest** (d_floor ≥ δ clears with *zero slack* there); fix `live_p2a_package_rescue.rs`'s doc comment, which claims to drive the seam its body never enters. | PROTOCOL §5.13 (delete :634, :814, :860, :915 — falsified by that file's own §5.13); §5.7 race table row 6; §5.8 (both consequence bullets, :393-395's "~111 vB" → 125 vB + 240 anchor, :416-419); §5.9 exit costs in sats; §5.3 committed fee; §6 relay/TRUC; §12 fee model + `max_fee_rate` as a constructor literal; §16 client conformance; DECISIONS D31 measured-facts table; TRUST-MODEL B4 (:411 "SDK default 288" → derived 2,120/860) and :194 **deleted, not repaired**. |
| **6. C-1/VE-1** | Minimum-slack margin beside `check_exit_headroom` + mirror in `split_output_floors`; wire `exit_child_pass_with_bump`/`watch_child_pass_with_bump`; export 153-vB child vsize as a named constant and recompute all figures **after** fixing the step-vs-curve model; make the peek path report face value (do **not** re-book the claim path). | Late-in-epoch conveyance refused with named error; slack==0 admission no longer possible; child-lane bump E2E; assert consistent value semantics across all four booking lanes. | PROTOCOL §7.1 (idle-coin cell, qualified by k ≥ 1), §7.2 (percentages), §2 baseline (keep the ~10× calendar win, drop the "epoch-dial artifact" claim); G11 admission soundness; G12 zero-idle-cost; SPEC-FOUNDATION A1 (r* rewritten on the shipped child), VE-1; SUBECONOMIC-FINALITY §1/§2/§4-C-1/§4-C-2/§6; CHILDREN.md; PARTIAL-PAYMENT-ECONOMICS §4.8/§6. |
| **7. D10** | Move `validate_backup_chain_v2` ahead of `verify_bundle_bound` on the claim path (transfer_receiver.rs:1441 vs :1488); feed the census the validated group. Comment ℓ=1 at db_manager.cpp:80-81/:357 + CI guard. **Do not** seed `seen_txids` with backup txids. | CI guard: schema never admits >1 live secnonce per key; assert the claim path counts only validated backups. (No dual-role adversarial test — it is unconstructible; say so.) | §6 CENSUS COMPLETENESS theorem with four premises; §3 shape obligations (both transaction shapes, stated as carrying a **census** obligation); §14; §5.11 (ordering becomes normative); SPEC-ROADMAP D4 citations + delete option (b); retract SPEC-FOUNDATION A8's stale residual. |
| **8. Coloured lane** | Delete `output_blinding` population at rust-rgb/src/lib.rs:509 and :630; resolve the `refuse_colored_multi_payee` doc-vs-string contradiction; sequence CR-D → fresh statechain 2-of-2 returning `new_statechain_id`; `auto_refresh_due` dispatches coloured carriers. | **First: run sdk75, sdk77, sdk29 against HEAD** — the only things that distinguish no-op / broken-spendability / no-privacy. Then round-trip seal test at n = 1, 2, 3 that actually seals and reveals (never a source pin). | §10 (what `colored_ladder = true` removes — D32 inventory); coloured seal-derivation subsection; renewal/re-anchor section (CR-D exists, lands on the owner's own key, nothing drives it, **336 vB/epoch ≈ 1,767 vB/coloured-coin-year**); named limitations (K>1 row → serial conveyance, not seal concealment); privacy statement; D30:515 + prerequisite table; COLOURED-SPINE:647 (regtest quoted as deployed), :668-671. |
| **9. R13** | Extension **grid** law consumer-side in `verify_child_bundle`'s `[F4]` block on every conveyance; fresh-mint equality (m == 0, d_c == d0) discriminated by the exact-equality census count; return `(hops_remaining, renewals_remaining)` at claim and refuse an underivable epoch. **No** d_c floor on re-transfer. | Off-grid e_c (e.g. 719) refused; e_c ≤ e_floor refused; renewed leaf still passes; honest final hop at d_c = 144 still passes; underivable epoch refused. | §Children CSV law; §Watchtower (D19 **cannot** be sized off this item); §Economics three-clocks separation; DECISIONS D14 row retraction. |
| **10. TRUC slot** | `AnchorConflict` + `conflict` parameter on `build_p2a_fee_child` with RBF rules 3/4 + feerate rule; `anchor_spender()` over `gettxspendingprevout`/`getmempoolentry`; conflict-aware `broadcast_tier` distinguishing fee-floor from lost-RBF; stop plain-first for the terminal tier; **stop greedily broadcasting SP_{i+1}**; `bump_cost_sats` adds the pin ceiling. **Delete proposed change #7.** | Live regtest: third party takes the anchor with a zero-out-of-pocket OP_RETURN child → pre-signed successor refused for sibling eviction, conflict-aware child wins RBF, delay is one confirmation; **plus the no-adversary case** (owner strands own walk by greedy broadcast). | PROTOCOL §5.13; §6 relay/TRUC (in-flight window of 2, single descendant slot, slot serves walk **or** rescue never both, slot is free for anyone to take); §5.3 (a pre-signed tier can never win a sibling eviction); DECISIONS D31 permission-vs-revenue split; SPEC-FOUNDATION §1 watchtower row; SPEC-ROADMAP W1 re-scoped to the pinning sub-clause alone; COLOURED-SPINE §1.6 latency model (un-withdraw the mutual-exclusion paragraph). |

---

## Part 3 — The path to spec-writable

### Stage 0 — Verification that must run before anything is claimed (parallel, days 1–2)

Nothing below this line is trustworthy until these run. All are cheap and all gate honest text.

1. **sdk75, sdk77, sdk29 against HEAD.** Decision 8 cannot be answered without them, and `25a4037`'s "685 green" is a unit count that touches no coloured lane. They distinguish three incompatible worlds: P2 is a harmless no-op / P2 breaks coloured-ladder spendability / P2 buys no privacy.
2. **Re-resolve every code citation in the corpus against `f30bf71`.** Multiple proposals claimed `50c2d36` as HEAD; `77807e1` added ~188 lines to `clients/libs/rust/src/tesr.rs` and every citation above ~10450 has drifted. Republish by **symbol**, not by line. SUBECONOMIC-FINALITY says so of itself already.
3. **Evaluate the two closed forms nobody has run.** `Σ V_i · 1[V_i < V_min(d_i, r)]` over the live fleet (C-1's aggregate — bounded and *unevaluated*, not unbounded), and the fleet-wide footprint curve for §7.2. Both terms are queryable today. Publishing §7 without them repeats the error the pass just caught.
4. **Grep-verify the four "unbuilt" claims that are now false**, and the one that is still true. Built: `build_p2a_fee_child`, `core_rpc::submit_package`, `watch_pass_with_bump`/`exit_pass_with_bump`, `tower_float`. Still true: nothing stops a third party pinning an anyone-can-spend anchor.

**Currently believed done but not verified:** D34 ("closes CO-1" — it does not, as deployed); D28's "what a receiver can check" row (attests a budget no acceptance decision reads); PROTOCOL §7 Phase 0 "DONE: generalized counters {level, m, k, total_sigs}, POST /renew/init" (`total_sigs` has **zero** hits in server/src, lockbox/src, lib/src; no `/renew` route exists); INV-27's evidence pin sdk30(a) (tests k=0, cannot witness the failure); `live_p2a_package_rescue.rs`'s doc comment (claims to drive the seam its body never enters); `relayability_is_checked_before_the_csv_race` (matches a string literal in its own source).

---

### Stage 1 — Decisions that must be TAKEN before a section can be written at all

These block *writing*, not building. None requires code first.

| Order | Decision | Blocks |
|---|---|---|
| 1 | **B1's disposition** (decision 1) — unbounded-and-published, plus the T remedy | §1 named limitations opening, REQ-3, G7/G8/G9, T-SE-1, the whole assumption inventory. Everything downstream inherits its trust framing. |
| 2 | **CO-1's disposition** (decision 2) — cryptographic or issuer-trusted | §7, §17, §3, §18, §19, §20, §25. **And it gates decision 3**: if CO-1 is open, O-1 cannot be graded "mitigated" in the same document, because on a CO-1-minted coin the census degenerates to arithmetic over attacker-chosen numbers. |
| 3 | **O-1/CO-3 disposition** (decision 3) — one merged row, three faces | §6 census (the linchpin section), G2, §14, §3 assumptions, §12. |
| 4 | **C-1's admission philosophy** (decision 6) — time-dependence closed structurally or admitted | G11, G12, §7 economics, SUBECONOMIC-FINALITY, CHILDREN. |
| 5 | **The coloured lane's shape** (decision 8) — but only *after* Stage 0 item 1 | §10, the coloured chapter, the privacy statement. |

Decisions 4, 5, 7, 9, 10 do not block *writing* their sections — they block those sections being **true**, which is Stage 2.

---

### Stage 2 — Code that must LAND because the spec is normative

Ordered by dependency, not by size.

**Tier A — must land before the section describing it can be written honestly:**

1. **Consume the attested budget** (decision 3). One function. Today `verify_conveyed_child` fetches the attested payload and then reads terminality from an unattested coordinator endpoint. §6 and §7 cannot describe an attested census while the live path discards the attestation.
2. **t2 confidential to the SE** (decision 2). Everything in CO-1's chain is vacuous before it, and until it lands the operator has *standing* theft of honestly-minted coins outside CO-1's stated scope.
3. **Lockbox host split** (decision 2). The only move that changes any adversary's cost. Blocks pinning and PoP from meaning anything.
4. **Schedule the deadline-safety pass** (decision 4). §Invariants cannot retract INV-27 and simultaneously describe a client that skips the shape.
5. **Extension grid law consumer-side** (decision 9). §Children's CSV law is a different rule depending on whether it lands.

**Tier B — must land before the section can claim the property, but the section can be drafted around it:**

6. Wire `BumpCapability` into the child/spine lanes and the de-trigger (decision 5). §5.13 currently contradicts itself in four places in one file.
7. Minimum-slack admission margin (decision 6).
8. Conflict-aware rescue pricing + stop greedy successor broadcast (decision 10).
9. Claim-path ordering: validate before counting (decision 7).
10. `committed_fee_rate` 2.0 → 3.0 with the `tokens.rs` derivations updated (decision 5) — and say out loud what it does to pieces in circulation.
11. Pinned SE identity + D34 PoP (decision 2, after 2 and 3).
12. Revert P2 (decision 8, after Stage 0 item 1).

**Ordering constraints that genuinely bind:**

- 2 → 3 → 11. Pinning is meaningless while the coordinator holds the seed; the PoP is forgeable while t2 is plaintext.
- Decision 2 → decision 3's *grading*. O-1 cannot be graded closed while CO-1 is open.
- Stage 0 item 1 → decision 8's disposition and item 12.
- Decision 5's live-rate de-trigger **must** ship with the superseded-de-trigger census term in the **same commit**, or a failed rescue bricks conveyability — the silent-degradation shape this repo has hit three times.
- Decision 1's head anchor on `validate_backup_chain_v2` → publishing `k` at all. Without it the number is forgeable downward and would sit in front of users as a safety signal.
- Decision 4's carrier coverage is *not* blocked by anything and is the sharpest open hole in that row.

---

### Stage 3 — Verification before any completeness claim

- Every negative test named in Part 2, especially the three that **do not exist today**: a malicious coordinator on the terminality path; a fee-stuck tier driven through a *pass* (not through the builder directly); a third-party anchor squat.
- The zero-CSV successor test **on regtest**, where `d_floor ≥ δ` clears with zero slack (6 ≥ 6) while mainnet clears at 144 ≥ 36. A mainnet-only test is the trap D14's own author flagged by hand.
- The carrier variant of every deadline and allocation test — the coloured lane is where three separate bounds turned out to be sat-denominated descriptions of a victim who loses an asset.
- Replace sdk30(a) as INV-27's evidence.
- A CI guard that no client reads `num_sigs` except through the attested reader. Today the single-reader property is a convention.

---

### Stage 4 — Work that proceeds in parallel with drafting

All documentation corrections in Part 4. All citation re-resolution. The two fleet queries. The D32 inventory. The three-clocks separation in PARTIAL-PAYMENT-ECONOMICS. None of these blocks or is blocked.

---

### What is genuinely blocking versus merely unfinished

**Genuinely blocking:** decisions 1, 2, 3, 6 (they determine what §1, §3, §6, §7, §17 *say*); Stage 0 item 1 (decision 8 is unanswerable without it); code items 1–5.

**Merely unfinished:** the bump plumbing (decision 5), the TRUC conflict pricing (decision 10), CR-D's renewal semantics (decision 8), the R13 grid law (decision 9). Each is a real defect with a known fix and a known cost; none prevents a correct section from being written today, provided the section says what is true.

**A single defect filed as three rows.** O-1, CO-1 and CO-3/F5 all name the same anchor. With route (i) subsumed under route (ii), they are not three faces of one anchor — they are **one defect**: enclave key material and the counter it attests are both held by the party the receiver is being protected from. Grading them independently triple-counts it. Publish one row, cite it three times.

---

## Part 4 — What this pass changed

### Fake bounds killed

| Claimed bound | Why it is fake |
|---|---|
| B1: "value-gated re-anchor caps per-victim exposure at θ" | The primitive is **cooperative** and the adversary is the operator; θ caps a *coin* not a victim (n·θ); and the gated field `amount` is **sender-chosen**. |
| B1: "prior-owner count = `flat_backups.len() − 1`, not forgeable downward" | Forgeable downward by head truncation — only *pairwise* decrement is enforced, and the census rebalances via one extra co-sign disclosed in `superseded_states`. A receiver can be shown a 1-element chain. |
| R-1: "damage:cost ratio ≈ 0.4 — griefing is economically losing" | A **vbyte** ratio and the r→∞ asymptote. In sats at r ≤ 2.0 the attacker pays 0 and the victim 980; sign flips at ~8 sat/vB; above 2.0 the de-trigger cannot be built at all. True at no rate. |
| R-1/D31: "anyone can spend the anchor is a rescue permission worth ≤47 sat in (2.0, 2.23]" | The child's change must clear `CHILD_CHANGE_DUST = 330`, so the profitable band is `r ≤ 0.727` — **empty** at every rate a rescue is needed. And no repo builder can construct an anchor-only child (two inputs hardcoded). |
| R3: "attacker-cost floor 2·(ceil(278r) − 250) bounds the exposure" | Bounds the *attacker's threshold from below*, never the loss; forced by the wrong mechanism (a receive-side check that does not bind a party already holding its tiers); off by 240 sat per package because the anchor is recovered as change. |
| R3/R13: "the enforceable edge is ONE BLOCK" | D14 landed at `77807e1`. `RivalKind::margin` enforces δ = δ_E = **36 blocks**, keyed structurally off the live rival. |
| TRUC slot: "delay bounded at 11–23 blocks, absorbed by δ = 36" | Compares a whole-chain aggregate against a per-outpoint budget; upstream delay is race-**neutral** because the rival is blocked by the same slot; and the shipped greedy loop produces the inverse harm profile. |
| TRUC slot: "per-victim loss is never a forfeiture — every tier pays only the owner" | A **superseded** tier pays the prior owner. tesr.rs:10670: "could mature first and steal." |
| VE-1: "639 vB per resulting coin per epoch, and nothing a sender chooses moves the ceiling" | Fake denominator (whole-tree total ÷ 11; a lone payee pays the full shared prefix) and fake rate (a depth-1 walk completes in 2,885 blocks, so ~3.4 rebuilds per epoch; the epoch bounds *depth*, not *frequency*). |
| C-1: "aggregate exposure is UNBOUNDED" | Refuted. Each piece's loss ≤ its own V < V_min, so aggregate **saturates** at the piece lane's total value. Constructible and **unevaluated**. The framing demanded this distinction and then got it backwards. |
| C-1: "victim count ≤ 76,335 for a 1 BTC coin, fan-out capped at 63" | `max_derived_tokens_per_statechain = 64` is a **per-parent lifetime** cap over spent+unspent rows, not a per-level fan-out. Binding count is ~64 — three orders of magnitude low, published in the alarming direction. |
| D10: "backup-vs-superseded is blocked ONCE by `csv < lo`, one parameter away from theft" | Missed the **parameter-free** orphan refusal forty lines later, and the parameter itself is bound field-by-field by `cap_schedule` before the census runs. Five separators on the other pair, not two. |
| D10: "ℓ=1 gives ~2^128 generic security" | Sequential blind-Schnorr one-more-unforgeability reduces to ROS₁, an assumption. A computed level was invented from an assumption swap. |
| Coloured: "exactly one stranded allocation per coloured split" | The receiver **never consults a blinding to book** — `extract_received_assignments` with `known_concealed: None` matches `Assign::Revealed` by vout. The named failure site compares two calls to the same blinding-independent function. |
| Coloured: "D35 caps the loss at carrier sats" | Caps the **attacker's gain**. The victim loses the allocation, which is burned and which no sat figure describes. |
| R-2: "the victim's exclusive window is exactly interval = 100 blocks" | T is un-timelocked; the window is `[receipt, L_{k−1})` — minimum 101 blocks, typically the epoch remainder up to ~10,000. Understates the defence by up to two orders of magnitude; the proposal's own stale-list conceded it. |
| R13: "d_c = 144 means the piece is born terminal" | `renew_child` resets to 1440 for zero on-chain bytes and zero depth, and the refusal string the proposal quotes **names it**. Loss is 6.25%, not 100%. |

### Stale claims caught

- **HEAD was wrong in most proposals.** `f30bf71`, not `50c2d36`. `77807e1` (D14) rewrote `verify_superseded_segment` — the exact function two rows were about — and shifted every `tesr.rs` line above ~10450.
- **PROTOCOL.md contradicts itself in four places in one file** (:634, :814, :860, :915) about the fee-bump path being unbuilt, all falsified by that file's own §5.13. :915 is in the implementation ledger, where a reviewer looks first.
- **PROTOCOL.md §7 Phase 0 marks DONE a mechanism that has zero occurrences in the tree**: `total_sigs`, `POST /renew/init`, the {level, m, k} counter machine. No column, no migration, no route.
- **PROTOCOL.md:316's "enclave single-active-state refusal, the second independent layer" is asserted in at least nine places** — including SPEC.md:184-185 as a **numbered invariant (INV-6)** with a "Verified by sdk40 PART 2" coverage claim. What is actually enforced is one-signature-per-secnonce, a key-leak defence, not a rival defence.
- **DECISIONS.md:168 and D8-SIGCOUNT-AUTH.md publish the opposite of the code** ("sig_count is not authenticated at any hop"; "closure = excluded scope"), with the correction stranded 200 lines downstream at :227/:239-247.
- **DECISIONS.md:174 says D14's "Implementation is WP3c."** It is live at tesr.rs:10686-10687.
- **D30:515 says a coloured re-anchor is "not built — no builder exists."** CR-D landed in `b79b525`. D30's prerequisite table is stale on two of three rows.
- **COLOURED-SPINE-REANCHOR-SCOPE.md:647 labels the regtest 1,000-block clock "deployed."** Deployed is 10,000 ≈ 69.4 days.
- **SUBECONOMIC-FINALITY.md:150/:156-157 and PARTIAL-PAYMENT-ECONOMICS.md:732/:750 cite `lockheight_init = 50000`**, a value on which the coordinator **panics at boot**. But :252 is a correctly-labelled sensitivity table with "10 000 (shipped)" in bold — flagging it would have discredited the two genuine ones.
- **The "~900× the anchor's value" figure is wrong three ways and appears in four normative documents.** 180,330/240 = 751×; the source note computes 180,330/**200**, the ratio to the *parent's fee*; and 180,330 was the funding wallet's chosen overpay against a required 754 sat at the run's own lab floor — a ~239× overpayment. Under a 1,000-vB TRUC child cap it is not derivable from the repo's own fee law at any legal shape.
- **"~5.68 sat/vB" is a 100× mis-conversion** of `0.00567704 BTC/kvB` = 567.7 sat/vB, falsifiable by one division.
- **"min relay fee not met, 200 < 423" is quoted as a protocol property in four documents.** It was produced at a lab-forced 3 sat/vB floor on a synthetic 141-vB transaction paying 1.42 sat/vB — neither a TES-R tier (125 vB) nor the protocol's rate. A real tier reads "250 < 375."
- **TRUST-MODEL.md:411's B4 quotes "SDK default 288"** against a derived 2,120 (mainnet) / 860 (regtest).
- **TRUST-MODEL.md:194's B1 mitigation** ("the current owner racing with a fee-bump wins in practice") names a capability that ships as `None` on both presets — and against T-SE-1's actual adversary (a fresh un-timelocked spend at market rate) no fee-bump capability could ever make it true. Delete, do not repair.
- **SPEC-FOUNDATION A8's residual** ("`cb.parent.params` is STILL a sender-declared serde field — recorded, not fixed") is stale; `cap_schedule` closed it. Leaving it standing is what let one analysis believe its CSV band was soft.
- **PROTOCOL.md's own constants table states "Hop budget per depth level | 36 × 16 = 576 transfers"** two rows above the line a proposal demanded be amended because "no document states it."
- **PROTOCOL.md:123 publishes the pre-TES-R baseline's "unilateral exit wait ≤ ~7 d"** — refuting the "5,840-vs-0 is an epoch-dial artifact" claim from within the same corpus. The calendar-axis win is real and ~10×.
- **`refresh.rs:514-516` `coin_near_final` exists, is correctly keyed on `coin.locktime` = L_k, and covers the whole-coin deadline clock.** A four-site "structurally cannot cover" enumeration missed the one site built for it. The real defect is that it is **not scheduled**.
- **The corpus files CO-3 and SM-5 as two rows at the same severity in the same document.** They are one defect, and SM-5 already prescribes the two-store cross-check CO-3 does not mention.
- **`sig_budget` is absolute on both sides at HEAD.** One proposal read a *fixed bug's* post-mortem comment as current behaviour and hard-coded a weaker booleans-only cross-check around it.

### Adversary mis-attributions corrected

- **B1 route (i) is empty and dominated.** The operator holds the discrete log of the *true* enclave key for every coin, honest-minted or not, because every server keypair is generated in-lockbox from the host seed and stored encrypted under it. It never needs to substitute E. Three routes become two, and the "very different privilege" framing collapses.
- **D10's "malicious sender acting alone, cheapest adversary, full coin per hop" is a maintenance-conditioned hypothetical**, not a capability any adversary has.
- **R-1's "damage" is not symmetric.** The transactions are symmetric; the *agents* are not — the rival's holder is by construction online and motivated, the defender by construction offline.
- **CO-1 is single-party, not collusion.** The operator runs a wallet, mints its own deposit tokens, owns the `confirmed`/`spent` columns, and operates the token server named in its own config.
- **Route (iii) — unattested `num_sigs` on nodejs/web — is the cheapest route by privilege**, and one analysis ranked it last while calling the seed-holding route "the cheap one." Worse, that analysis declared those clients *exempt* ("they refuse laddered coins outright") when the gate fires on three **sender-declared** fields — the identical shape the Rust side already had to fix with `MIN_PREPAY_PROTOCOL_VERSION`. The exempt population is the most exposed one.
- **The coloured carrier is the lane with zero automatic deadline coverage**, not the lane where the bound is tightest. `auto_refresh_due` excludes carriers; `auto_exit_due` skips them; the tower exports `u32::MAX`; and with `colored_ladder: false` every available discharge is plain and therefore destructive.
- **The un-rescuable child/spine lanes cost latency, not coins** — post-D14, fees cannot buy CSV maturity. Selling the fix as closing a theft channel is the overclaim the next reviewer catches.
- **The `max_fee_rate: 1.0` cap is a symmetric disarmament that disarms the attacker more than the victim** — a proposal filed it as a vulnerability and recommended deleting it, which would re-arm every future prior owner's retained rung and close a window the holder currently wins for free.
- **A funded P2A bounty pays whichever tier is stuck**, honest or rival, at the same rate — it cannot create a defender advantage and can only cheapen the race for whoever is already online and funded.
- **A proposed CI invariant** (`min(δ, δ_E) >= max_exit_txs`) compares two quantities with no relation, would boot-panic regtest for a hazard it does not describe, and would stamp mainnet safe on a coincidence of units — a guard pinning a description nobody derived.
- **A proposed adversarial test is unconstructible.** A flat backup must carry `Sequence(0)`; a superseded tier must carry `csv >= 144`. One field, two mutually exclusive required values — before nVersion, nLockTime, the anchor, and non-confirmability. The only way to write the test is to hand the verifier something that is not a valid backup.