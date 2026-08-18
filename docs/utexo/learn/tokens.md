# Tokens on RGB

> How partial token amounts work — colored splits, raw units vs precision, the piece size, exit
> behaviour — is covered end-to-end in the [granularity deep dive](granularity-deep-dive.md).

## Hello RGB

The token standard here is **RGB**, not a server-side token ledger. An RGB asset is a
client-validated contract: issuance and every transfer are cryptographic state transitions committed
inside Bitcoin transactions (tapret/opret) and validated by the *receiving wallet* from a
**consignment** — the transition history together with the witness transactions that seal it. Nobody
— not the SE, not an indexer — is trusted for token state.

On this layer allocations ride **statechain coins and off-chain sub-coins**, so token payments
inherit what sats have: instant off-chain transfers, exact amounts via colored splits,
branch-verified receiving, and an on-chain settlement path that needs nobody's permission
(materialize the branch — see [Exits with tokens](#exits-with-tokens); it is not the plain one-tx
sweep sats get).

## The carrier, and why its ladder is coloured

A coin that holds an RGB allocation is a **carrier**.

`claim()` establishes a TES-R exit ladder — `T` trigger → `X_m` extension → `S_k` state, relative
CSV, all un-broadcast — for every fresh confirmed **root** coin. A carrier is not exempt from that
any more, but it may never be given a **plain** ladder: a plain tier is a *sats-only* spend of the
carrier's funding output `F`, so broadcasting one destroys the allocation
([`../spec/PROTOCOL.md`](../spec/PROTOCOL.md) §5.10 rule 1, terminal freeze;
[`../spec/SPEC.md`](../spec/SPEC.md) INV-29). What it gets instead is a **coloured** ladder — every
tier a coloured tier carrying its own RGB state transition (CTES-R) — so that the same walk which
exits a plain coin *moves* the allocation rather than sweeping it away.

The decision site in `claim()` is `match (self.inner.config.colored_ladder, one)`
(`UtexoWallet::claim`, `clients/libs/rust-sdk/src/wallet.rs`), where `one` is the single booked
allocation on the carrier's outpoint. With `colored_ladder` set *and* exactly one allocation resolved,
the carrier reaches `mercuryrustlib::tesr::build_colored_ladder_auto` + `cosign_colored_ladder` and
**is** laddered. Every other case records `LadderSkipReason::RgbCarrier` and leaves the coin flat.

**What decides `colored_ladder` is the enclave pin, not a preference.** It is no longer a bool the
constructors state; both presets compute it as
`TesrParams::attestation_identity_const(network).is_some()`
(`clients/libs/rust-sdk/src/config.rs`). The coupling is the point: colouring a carrier retires the
legacy coloured-split lane for it, and the CTES-R lane that replaces that lane cannot establish
anything without an identity to verify the enclave's attestation against — so `true` with no pin is
not a bolder default, it is a wallet whose token lane refuses forever while claiming to be retryable.
Reading the pin makes the two unable to disagree.

- **regtest is pinned** (`TesrParams::REGTEST_ATTESTATION_IDENTITY`, derived from the dev seed this
  repository commits, so the key is a fact about the source rather than about a running server), and
  therefore ships **on**.
- **mainnet, testnet and signet evaluate false**, for one reason: **no enclave is provisioned there
  yet**, so there is no identity to pin. Inventing one would be worse than leaving it absent — a wrong
  pin refuses every attestation, and the obvious "fix" is to trust the key the coordinator serves,
  which is exactly the hole the pin exists to close. Publishing a real enclave identity and pinning it
  turns this true with no other change. [`../spec/SPEC.md`](../spec/SPEC.md) §0.4 rows V-1 and V-6.

So the two lanes below are not "shipped vs experimental" — they are "wherever an enclave is
provisioned" vs "not yet there":

| | Coloured ladder (an enclave identity is pinned) | Flat carrier (none is) |
|---|---|---|
| Exit material | `T → X_m → S_0`, coloured; a received piece walks five tiers `T → X_m → SP → ext_child → state_child` | the signed-once backup chain ([SPEC §2.4](../spec/SPEC.md)) plus the un-broadcast colored split/combine branch |
| Calendar | the retained flat chain still carries a calendar; the tier walk itself is relative-CSV | an **absolute**-locktime deadline anchored at `deposit_height + initlock` |
| Payment shape | coloured in-ladder split (`colored_in_ladder_pay`) | flat colored split over `F` (`create_colored_split_tx`) |
| Sends per carrier | **1** — the change is a depth-1 coloured child that three named guards refuse to split again | `LEGACY_CARRIER_SEND_DEPTH` = **5** chained splits |
| Multi-payee batch | refused by name (`refuse_colored_multi_payee`) | supported (`batch_transfer_tokens`, `sdk09`) |
| Re-anchor | `colored_reanchor` (coloured de-trigger) | none — `refresh` refuses a carrier |
| Unilateral exit | the coloured walk moves the allocation to the owner's own key (`sdk74`, `sdk75`) | none — every pre-signed spend of `F` this wallet holds is RGB-unaware |

The coloured lane is built and exercised end to end (`sdk74` establish, `sdk75` exit, `sdk77`
coloured in-ladder split, `sdk87` / `sdk88` deadline and headroom).

**Read the right-hand column's payment rows against the licence retirement, because it moved them.**
That column describes what a carrier still *is* where no identity is pinned, and its exit and
calendar rows are unchanged. Its *send* rows are not. The licence that used to permit a flat
carrier's conveyance — `licence_rgb_carrier`, on the reason spelling `rgb-carrier` — is **retired**
(`clients/libs/rust/src/transfer_sender.rs`), because "this coin is an RGB carrier" has stopped being
a reason a coin may move without a ladder; and `licence_funding_not_onchain` went with it, which is
the one that covered the *piece* a flat colored split mints over un-broadcast funding.
`assert_flat_conveyance_is_legitimate` holds exactly one `Ok` and it is reachable only from a proven
licence, so with both gone the flat lane's hand-over refuses rather than degrading — the five chained
sends are what the carrier is still *sized* for (below), not what it can complete today. The reasons
are still *recorded* so the state is visible; they simply no longer open the gate. That is the sharp
end of "waiting on an enclave": on an unpinned network a flat carrier is holding and settlement, not
payment.

## Where the sats come from — the carrier is sized, not rounded

Two constants in `clients/libs/rust-sdk/src/tokens.rs` fix a carrier's economics, and both are
derived from the protocol committed fee rate `TesrParams::committed_fee_rate` = **3.0 sat/vB**
(`TIER_COMMITTED_FEE_RATE`), not chosen:

* **`TOKEN_PIECE_SATS` = 4 074** — the sats a token piece carries. It is the coloured ROOT floor
  `colored_ladder_floor` computed at *double* the committed rate (`PIECE_FEE_RATE_HEADROOM` = 2.0):
  `3 · (⌈168 · 6⌉ + 240) + 330`. A piece is a coin like any other — its receiver claims it, and if
  the carrier is coloured that claim ladders it — so the piece must clear the floor it will be
  measured against, with head-room for a rate that moves after it is carved.
  `token_piece_sats_is_the_coloured_root_floor` recomputes the floors from the real `tesr` functions
  and fails if the constant ever drops below them.
* **`TOKEN_CARRIER_SATS` = 22 536** — what a freshly-issued carrier is funded with, and the *larger*
  of the two lanes' requirements, because the lane is chosen per spend and a wallet may flip the flag
  between issuing a carrier and spending it. Flat lane: `5 · (4 074 + 300) + 666`
  (`legacy_carrier_sats`). Coloured lane: 8 253 (`ctesr_carrier_sats`). Over-sizing parks sats in a
  change output; under-sizing is refused at spend time with the carrier already terminalized, so the
  max is the fail-closed choice.

Two more floors bound a split: `split_fee_reserve(parent) = clamp(parent/100, 300, 2000)` and
`min_split_output(rate) = 330 + ⌈112 · rate⌉` — every output must fund its own backup, else
`create_tx1` rejects it as `FeeTooLow` *after* the carrier is terminal. `transfer_tokens` checks
both up front and refuses with the carrier untouched.

## Issuance, mint, burn

| Spark BTKN | Here |
|---|---|
| `createToken` | `issue_token(ticker, name, precision, supply)` — RGB NIA, full supply at issuance / `issue_inflatable_token(..., inflation_amounts)` — IFA |
| `mintTokens` | `mint_tokens(asset_id, inflation_amounts)` — IFA on-chain inflate, the newly-minted allocation bound to a fresh statechain coin (NIA supply stays fixed); [SPEC §7](../spec/SPEC.md) REQ-20, `sdk09` |
| `transferTokens` | `transfer_tokens(asset_id, receiver_address, amount)` — colored off-chain split + handover |
| `batchTransferTokens` | `batch_transfer_tokens(asset_id, &[(address, amount)])` — one colored split, N pieces + change (`sdk09`) |
| `freezeTokens` / `unfreezeTokens` | **N/A by design** — see below |
| `burnTokens` | `burn_tokens(asset_id, amount)` — burns engine-held free balance on-chain; statechain-bound supply must be exited first |
| token identifier (`btkn1…`) | RGB contract id (`rgb:…`) |

`issue_token` does three things:

1. issues the NIA contract in the wallet's RGB engine (`create_utxos` then `issue_nia`),
2. deposits the full supply onto a **fresh statechain coin** of `TOKEN_CARRIER_SATS` in one colored
   on-chain transaction (`bind_engine_supply`) — the only on-chain tx a token needs until settlement,
3. registers that coin as the asset carrier.

`issue_inflatable_token` creates one colorable UTXO per allocation (the fungible supply plus each
inflation-right) before issuing, so binding the supply never consumes the reserve
([SPEC §7](../spec/SPEC.md) REQ-19, INV-12). `mint_tokens` snapshots `list_allocations` *before*
inflating and binds only the difference, so a mint never re-binds already-bound supply (REQ-20).
Each of `issue_token`, `issue_inflatable_token` and `mint_tokens` has a `_sized` sibling that takes
the carrier's sats explicitly; the default is `TOKEN_CARRIER_SATS`.

From then on the supply moves off-chain.

## Transfer lifecycle

```
alice: 1000 TKN on carrier C (22 536 sats)
alice.transfer_tokens(TKN, bob, 250)
  → colored split (off-chain, un-broadcast): C → [piece: 250 TKN + 4 074 sats][change: 750 TKN + rest]
  → consignment for the 250-TKN assignment rides the transfer message as
    BackupTx.rgb_consignment, a ConsignmentEnvelope{c, a, s}
  → the piece is handed over (key handover, exit branch included)
bob's watcher:
  → validates the branch (consensus) and the consignment (RGB, off-chain resolver)
  → books what the CONSIGNMENT assigns to his own witness outpoint
    (accept_offchain_amount), under the consignment's VERIFIED contract id
balances: alice 750 / bob 250 — zero on-chain footprint
```

Two receiver rules do the work, and both are normative
([SPEC §7](../spec/SPEC.md) REQ-21/REQ-22): the amount comes from the consignment, with the
envelope's `a` field treated only as a cross-checked hint (a mismatch rejects, ERR-8); and the asset
is booked under the cryptographically verified `contract_id`, never a sender-claimed one. The single
predicate `verify_consignment_assignment` serves both the claim path and the SSP's pre-payment gate,
so the two can never drift apart.

**Multiple carriers.** If no single carrier holds the amount, `transfer_tokens` spans several, and
the two lanes span them differently.

On the flat lane it COMBINEs — N carriers → exact piece + change in one SE-co-signed colored
combine tx (`colored_combine_transfer`, over `mercuryrustlib::rgb::create_colored_combine_tx`). Every
combined carrier is made terminal first, and the receiver validates the multi-input branch requiring
**all N** input carriers to be terminal: `required_terminal_ancestors` counts one per structural
*input* across the branch (Σ inputs), not one per hop, which is what closes the multi-carrier
double-spend hole. Conservation holds across the whole input set (INV-13).

On the coloured lane that shape cannot exist — each carrier's `F` is already spent by its own trigger
`T`, and `SP` spends exactly one `X_m`, so there is no multi-parent coloured tier. `colored_multi_carrier_transfer`
pays it as a multi-PIECE payment instead: one in-ladder split per carrier, each conveying a coloured
child to the same recipient, who books them as separate allocations summing to the amount. Terminality
is then one terminal parent per leg rather than N per transaction, and the legs are sequential and
**not atomic** — a failure on leg `k > 0` leaves legs `0..k` already conveyed, so the recipient is
short-paid rather than unpaid, and the error names every piece already handed over. `sdk31` drives
this lane: two carriers, two coloured children, each child's `SP` spending its parent's `X_m` payload
output and never `F`, each source carrier terminal at the SE, and a read-only stock probe binding each
child's exact share to its own exit output.

**Many recipients.** `batch_transfer_tokens` carves one piece per recipient plus change in a single
colored split; each piece ships with its own consignment envelope and each receiver validates its
own amount (`sdk09`). On the coloured lane this is refused by name
(`refuse_colored_multi_payee`) — that lane conveys serially after the carrier is already terminal
and journals no recipient address, so a failure part-way through would strand the remaining pieces.
Pay two recipients from two carriers there.

## Why there is no freeze

Spark tokens can be issuer-frozen because operators enforce token state. RGB is
**client-validated**: token state has no central enforcement point, so an issuer freeze list has no
consensus meaning — a "frozen" holder's transfers still validate for any receiver that does not
honour the list. This is treated as a property of the trust model (tokens behave like bearer
instruments) and is documented rather than faked. Issuers who need freeze semantics should not issue
on client-validated rails.

## Exits with tokens

A carrier refuses the plain exit operations, and each refusal has a reason:

* **`withdraw`** excludes carriers from the withdraw-everything default and hard-errors when a
  carrier is named — an RGB-unaware L1 sweep destroys the allocation
  ([SPEC §9.1](../spec/SPEC.md)). `sdk02` asserts exactly this: a cooperative sweep of a wallet
  holding a received token coin sweeps **nothing**, and the balance survives intact. (That test runs
  with `colored_ladder` set, but the refusal is lane-independent — `withdraw` never consults the flag.)
* **`unilateral_exit`** likewise excludes carriers, with one opening — and on a network whose
  enclave identity is pinned that opening is now the normal case: a carrier whose ladder **is
  coloured** may walk it, because every tier is then a valid RGB state transition and the walk moves
  the allocation to the owner's own key. Where no such ladder exists the call refuses, and it refuses
  *by name* rather than returning `complete` on a walk it did not perform — naming the two routes
  that do exist, `materialise_carrier` and `transfer_tokens`.
* **`refresh`** (the cooperative on-chain re-anchor) refuses carriers outright — it routes through
  `withdraw`, which is RGB-unaware, so a plain re-anchor would move the sats and destroy the
  allocation. The coloured counterpart is `colored_reanchor` (broadcast `T`, then co-sign and
  broadcast a coloured de-trigger — two transactions, zero CSV wait, no SE change), and it needs a
  **coloured ladder** to build from. Where there is none, the refusal names that too: move the asset
  off the coin first.

Token settlement instead means **materializing the coin's branch on-chain**: broadcasting the stored
`branch-<id>` rows, which for a carrier *are* the un-broadcast coloured split/combine transactions —
the RGB witnesses that carved the allocation. Landing them root-first settles the allocation on a
confirmed outpoint and spends the shared root. `sdk39` drives this at depth 2: two successive
transfers build a piece whose branch is `[split1, split2]`, the recipient broadcasts both root-first,
and the 250 units settle on-chain with no SE involved. Onward movement of the *sats* still needs the
SE: materializing settles the asset, it does not exit the coin. `unilateral_exit` says exactly that
in its refusal for this class — the sats stay on the 2-of-2 outpoint, and the two routes that do
exist are named (`materialise_carrier` to settle the allocation, `transfer_tokens` to move it onward)
— rather than returning an `ExitStatus` with `complete` set, which would be a false green on an
escape hatch.

`materialise_carrier(statechain_id)` is the named call for a carrier for which no coloured ladder can
ever be built. It is gated on `carrier_is_permanently_flat` — the same shared definition
`unilateral_exit` refuses against, so the two can never disagree about which coins they mean — and it
verifies against the chain before returning: an unreachable backend is an `Err`, never a quiet
success.

## Tokens over time — holding, and doing nothing

*"If I issue or receive tokens and then do nothing for a year, do I lose them?"* No. Tokens are never
lost by inactivity. What differs is what still works, and that depends on how you got them.

**Tokens you issued or minted — a root carrier, funded on-chain.** Terminal freeze keeps it off the
*plain* T/X/S tiers for good; whether it gets *coloured* ones depends on the enclave pin above. Either
way it has no ancestor, so there is no clawback risk. What it always keeps is its signed-once deposit
backup, whose absolute locktime matures at `deposit_height + initlock` —
`TesrParams::flat_ladder_params` compiles that in per network: 10 000 blocks head start with 100 per
hop on mainnet, testnet and signet; 1 000 / 10 on regtest. That maturity does not stop you sending: a
token transfer is a colored *split*, and each sub-coin it mints gets its **own fresh** backup at the
full initial locktime (`create_tx1(.., tx_n = 1)`). A carrier travelling flat has no unilateral path
off it at all, and since the licence retirement no flat hand-over either, so its tokens stay put
until the carrier can be laddered — which is the enclave pin again, not a decision; a coloured
one can walk its own ladder instead.

**Tokens you received — a sub-coin carrier**, funded by an output of an un-broadcast split. That
funding is why it can never root a **trigger** of its own — and colouring a tier does not change it,
because nothing can broadcast a funding output that was never meant to be broadcast. Its exit
material reaches back through its parent instead. Also not lost, and here there is an **SE-free**
option: the exit branch is locktime-free, so broadcasting it materializes the allocation on-chain at
any time — as long as the shared root is still unspent.

That last clause is the whole risk, and it is handled automatically. Past the root deadline the
*sender's* own retained deposit backup matures. It is an RGB-unaware spend of the very funding output
`F` the receiver's material roots at: on the flat lane broadcasting it is a clawback (the tokens
return to her), on the coloured lane it is a burn. Either way the receiver loses the allocation, and
the answer in both cases is the same — **spend `F` first**.

`auto_exit_due(margin_blocks)` does it, in a carrier loop disjoint from the plain-sub-coin one and
gated on a *verified* branch read, because only that can tell "this coin has none" from "I could not
look":

* a carrier whose branch reads **verified-empty** is an issued/flat carrier — no ancestor, nothing
  to race it — and is skipped, and only then;
* a flat carrier that **has** a branch and is inside its margin emits
  `WalletEvent::TokenCarrierMaterialized` and is materialized with `broadcast_branch_if_any` —
  **branch only**, never the sats-sweeping backup ([SPEC §9.5](../spec/SPEC.md) REQ-33);
* a received **split child** has no `branch-` row at all — its exit material is the five-tier chain in
  its `ctesr-` bundle — so a third loop drives `unilateral_exit` for it instead, resuming the walk on
  later passes as each relative timelock matures, and taking its deadline head start (`Σ csv`,
  `exit_wait_blocks` over the bundle's own chain) rather than guessing it. That loop covers **both**
  lanes — a plain leaf has the identical exposure and no other runtime deadline defence — plus the
  sender's own coloured change (a `spinetip-` row), and it emits `TokenCarrierMaterialized` for a
  coloured row and `LeafExitForced` for a plain one. A leaf that its own partial-payment split has
  already terminalized is **skipped**, never driven: `journal_open_splits` is the durable evidence,
  written before the co-signature, and an unreadable journal counts as blindness over every child.

`sdk34` drives the coloured variant end to end and asserts the clawback is defeated: mine past the
deadline and the sender's matured backup fails to broadcast, because `F` has already been spent.

The pass runs every poll of the background watcher (`start_background`, gated on
`SdkConfig::auto_exit`, which ships **true** in both constructors), and it fails **closed and loud**:
any failure to read the chain tip, the wallet record, the carrier enumeration, or the split journal
emits `WalletEvent::WatchtowerBlind`, retains a `WatchtowerFault` readable through
`watchtower_faults`, and returns `Err`. It never proceeds on a defaulted-empty carrier set — that
would skip every carrier's protection while reporting success.

The margin itself is derived, not chosen: `auto_exit_margin_blocks_for(k_max, interval, child_depth)`
= `k_max·interval + tesr_exit_txs(d)·144`, i.e. **860** blocks on regtest and **2 120** on mainnet,
because the exit walk lands its transactions one after another and each must confirm before the next
tier's relative lock starts counting.

**Delegation.** The duty is also delegable, keyless, to any external tower
([SPEC §9.5](../spec/SPEC.md) REQ-34). `sdk45` pins the property on the ladder bundle itself: it
carries **zero** key material (a serialized bundle is scanned for every secret-key spelling), a
second independent tower over the same bundle is harmlessly idempotent, and a keyless tower drives an
offline owner's whole exit off nothing but `tesr::watch_pass`.

`export_watch_bundle` is the SDK's export of that duty across a whole wallet, and it is where the
carrier rule lives: a `WatchEntry` with `token_carrier` set carries **no** `backup_tx` at all,
so a delegated tower can only ever materialize the branch, never sweep and destroy the allocation.
That omission is structural — the field is `None`, not a policy the tower is trusted to follow — and
the unit test `bundle_roundtrip_and_carrier_has_no_backup` is what holds it. The export also fails
**closed**: a token wallet whose carriers cannot be enumerated exports nothing rather than
mis-exporting a carrier as plain, and adopted split children and spine tips are read from their own
rows so a leaf is never silently absent from the bundle.

**One limit worth naming.** A lone received piece carries `TOKEN_PIECE_SATS` = 4 074 sats, and
splitting it again needs `TOKEN_PIECE_SATS + split_fee_reserve` *below* the parent's value — which
4 074 is not. So a single piece cannot be re-sent on its own: hold it, combine it with another piece
of the same asset, or materialize it.

**Summary.** Never lost. Cooperative operations work throughout. A received token's unilateral
materialization works indefinitely while the shared root is unspent — and the watchtower keeps it
unspent for you, so even an offline receiver is protected. The asymmetry with sats is narrower than it
used to be, and worth stating exactly. Every coin, plain or coloured, has its CSV clock stopped by an
un-broadcast trigger and still keeps the retained flat chain's `min(L_k)`, so on the *calendar* the
two are alike. What still differs is the escape hatch: a plain coin's walk is always available,
whereas a carrier's is available only if its ladder is **coloured** — and where no enclave identity is
pinned it is not, so materializing the branch is the whole of that carrier's SE-free story. That is
the residual price of anchoring RGB in signed-once transactions, and it shrinks to nothing on a
network with a provisioned enclave.

---

Normative sources: [`../spec/SPEC.md`](../spec/SPEC.md) §7 (tokens), §6.2 (branch split & combine),
§9.5 (watchtower); [`../spec/PROTOCOL.md`](../spec/PROTOCOL.md) §5.10 (RGB integration and terminal
freeze). Tokens over Lightning: [lightning.md](lightning.md).
