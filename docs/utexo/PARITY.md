# Spark ↔ Mercury+RGB feature parity matrix

> **Verification (final):** full suite green on regtest — the live SDK_E2E set (wallet flow, tokens,
> TES-R ladder lifecycle/renewal/off-chain rollover, unilateral exit, watchtower, in-ladder split and
> first-class children, Lightning HODL latch in both directions, parity methods, adversarial/chaos,
> re-anchor/refresh, token granularity + combine), RGB_E2E=1..14 (off-chain DAG primitives + blinded
> transfer, history, UDA/CFA/IFA issuance), and the complete upstream Mercury suite (tb01–tb05, tm01,
> ta01–ta03, tv01). See the CI matrix for the authoritative test list and pass count rather than a
> frozen number here — the SDK_E2E suite is not a contiguous range, and every claim below cites the
> test that still covers it.

Target: every user-visible Spark feature (docs.spark.money + `@buildonspark/spark-sdk` +
`@buildonspark/issuer-sdk`) implemented on Mercury Layer with a **single SE** (blind-MuSig2 2-of-2,
lockbox) instead of FROST multi-operator, and **RGB** (UTEXO-Protocol/rgb-lib) instead of the
BTKN/LRC-20 token standard.

Status legend: **NATIVE** = Mercury dev already has it · **PORT** = exists on `feat/rgb-statechain`,
ported here · **NEW** = built in this effort · **PARTIAL** = works with a documented mechanism
difference · **N/A** = not applicable to the single-SE / RGB design (rationale given).

## Architecture mapping

> **One protocol, two coin shapes.** There is a single protocol. `claim()` establishes a TES-R ladder —
> trigger `T` → extension `X_m` → owner state `S`, relative-CSV tiers, all **un-broadcast** — for every
> fresh confirmed ROOT coin, unconditionally. Underneath it live two coin *shapes*, both current:
> **laddered** (every plain BTC deposit), and **un-laddered** — an RGB **carrier** is deliberately never
> laddered, because a plain tier spend would destroy the allocation (terminal-freeze, PROTOCOL.md §5.10;
> proven by sdk52), and a split sub-coin whose funding is un-broadcast cannot root a trigger [B0].
> Un-laddered coins keep the signed-once backup and transfer by backup-chain handover; that path is
> load-bearing for RGB tokens, not legacy. Rows below say which shape they describe.

| Spark concept | This implementation |
|---|---|
| Spark Entity (SE) = t-of-n FROST Spark Operators | Mercury SE: one server + lockbox enclave, blind MuSig2 2-of-2 (owner+SE). Trust: Spark = 1-of-n operators honest; here = the one SE honest. Both: user can always exit unilaterally without the SE. |
| FROST 2-round threshold signing, consensus engine, gossip | N/A — replaced by Mercury's sign_first/sign_second blind-MuSig2 exchange. No consensus/2-phase commit needed with one signer. |
| Leaf (TreeNode, P2TR user+SO key) | Statechain coin (P2TR aggregated owner+SE key). |
| Tree: root deposit → branches → leaves | Deposit coin → TES-R ladder + off-chain split/combine DAG (un-broadcast SE-co-signed txs; sub-coins = witness outputs). A non-exact payment from a laddered coin is an **in-ladder split**: a state tier spends the extension and pays a child statechain coin (sdk58 accepts a real child bundle across 11 adversarial cases; sdk59 the end-to-end payment). One tree per deposit → forest. |
| Timelock decrement (2000 → −100/transfer, renew ≤300) | **NEW (TES-R)** for laddered coins: instead of decrementing an absolute nLockTime, each transfer co-signs a fresh owner state at a strictly **lower relative CSV**, so the current owner always out-races every superseded state; the funding outpoint is never spent, so an idle coin never ages and pays 0 vB of rent. Each hop discloses exactly one superseded state, which the receiver's census counts against the enclave sig-count and proves out-raced (sdk40 PART 2 — a stale state dies at consensus; sdk46/sdk47 — the R' census; sdk54 — verify_bundle adversarial). Un-laddered coins (RGB carriers, split sub-coins) keep Mercury's decrementing-nLockTime backup chain (tb05) + **SE single-use per node** (no race possible: child spends parent, rgb04) + epoch deadline. Mechanism differs from Spark's; the guarantee (current owner exits first) is preserved on both shapes. |
| Leaf renewal (`renew_leaf`) | **NEW (TES-R)** for laddered coins — and stronger than Spark's bounded renewal: renewal replaces the extension horizontally, and when the extension-CSV budget is exhausted the ladder **rolls over off-chain** (the current state becomes a self-split, a fresh level hangs off it), giving unbounded off-chain state transitions at zero on-chain bytes (sdk43 renews, rolls over, renews again, then exits through the whole deep chain). There is no locktime floor to approach. `refresh` is therefore no longer a deadline reset but the **re-anchor** escape hatch — one cooperative on-chain spend of `F` mints a new coin with a fresh ladder and kills every exit right rooted at the old `F` (user-pays and operator-sponsored modes, sdk30). A sponsored rebate is paid by an in-ladder split of the sponsor's coin, so it is sized `max(fee + dust, min_child_value)` — sizing it below that floor made sponsored refresh fail *after* the user had paid the on-chain fee; fixed. Un-laddered RGB carriers renew by that same re-anchor. |
| SSP (Spark Service Provider): swaps, LN gateway, coop-exit connector | No SSP needed for BTC coop exit (SE co-signs a direct spend). For Lightning, the HODL-latch counterparty (any LSP running LND) plays the SSP role, and runs a **pre-pay census** (`verify_bundle` / `verify_conveyed_child`) over the conveyed ladder before it parts with money. Leaf-denomination swaps are unnecessary: multi-input combine + in-ladder split make exact amounts natively (Spark needs SSP swaps because leaves are fixed denominations). |
| Spark address (bech32m `spark1…`, identity pubkey + invoice fields) | NEW: bech32m statechain address encoding (existing Mercury transfer address) + invoice fields for the SDK. |
| BTKN token (CREATE/MINT/TRANSFER/FREEZE, TTXOs) | RGB assets (NIA/UDA/CFA/IFA) on statechain coins; allocations ride the same coins/sub-coins. See token section. |

## Wallet SDK (UtexoWallet → `mercury-utexo-sdk`)

| Spark method | Status | Mechanism |
|---|---|---|
| `initialize({mnemonic, network, options})` | **DONE** | `UtexoWallet::initialize(SdkConfig, Option<mnemonic>)` |
| `getIdentityPublicKey` / `getUtexoAddress` | **DONE** | stable `ml1…/tml1…` statechain address |
| `getBalance` (BTC + tokenBalances) | **DONE** | available/pending/in-transfer sats + per-asset RGB balances |
| `getSingleUseDepositAddress` | **DONE** | `get_deposit_address(amount)` |
| `getStaticDepositAddress` / `queryStaticDepositAddresses` | PARTIAL | Mercury addresses are per-coin; SDK re-issues a fresh deposit slot bound to the same key on use + duplicate detection (`check_for_duplicated`) covers reuse. Documented difference. |
| `claimDeposit` / auto-claim | **DONE** | `claim()` + `start_background()` watcher, `DepositConfirmed` events. `claim()` also auto-establishes and persists the TES-R ladder for every fresh confirmed root coin — no manual `establish` call, idempotent on re-claim (sdk48) |
| `transfer({receiverUtexoAddress, amountSats})` | **DONE** | exact-subset selection OR an **in-ladder split** minting the exact amount (a state tier pays a child coin; the parent is terminalized and its superseded state disclosed). Receivers never take a coin on trust: the transfer carries the bundle and the receiver's **census** verifies every un-broadcast tier against chain + `/info/statechain` and proves the disclosed states out-raced (sdk54 adversarial, sdk46/sdk47, sdk58) |
| received leaf is fully usable (re-transferable) | **DONE** | a received split CHILD is first-class. `claim()` completes the standard SE key handover — the receiver co-owns `A_child` (invariant across the rotation, so every pre-signed child tier stays valid) and the sender is permanently locked out. A child can then be paid onward WHOLE (`child_retransfer`) or SPLIT (`child_in_ladder_pay`, a depth-2 `ancestors` chain), one co-signature and one disclosed superseded state per hop (docs/utexo/CHILDREN.md; sdk60 alice→bob→carol with the funding outpoint unspent throughout, sdk17 partial second hop) |
| `transferTokens({tokenId, receiver, amount})` | **DONE** | colored off-chain split + handover; consignment rides the transfer message; receiver books the consignment-VERIFIED contract id AND the consignment-derived AMOUNT (envelope amount is only a cross-checked hint — G2, sdk02) |
| `batchTransferTokens` | **DONE** | one colored split -> N recipient pieces + change, per-piece envelopes (G3, sdk09) |
| `getTransfers` / `getTransfer` | **DONE** | `get_transfers()` / `get_transfer(utxo)` over the wallet activity log (sdk11) |
| `transferV2` (multi-recipient sats) | **DONE** | `transfer_many(recipients)` — one off-chain split -> N recipient pieces + change; routes per parent shape, so a laddered coin takes a multi-child in-ladder split (sdk11, sdk69) |
| `list leaves/UTXOs` | **DONE** | `list_coins()` — coin inventory with status + off-chain flag (sdk11) |
| `payLightningInvoice` | **DONE** | one call: `pay_lightning_invoice` for an exact-amount coin (sdk63) and `pay_lightning_invoice_inladder` for an arbitrary invoice, which splits the laddered coin in-ladder and latches the piece (sdk65). The SSP runs its **pre-pay census** over the conveyed ladder/child bundle before `send_payment`. A failed payment is reclaimable once the SE `batch_timeout` elapses — `pay_lightning_invoice_reclaimable` + `reclaim_lightning_payment` (sdk68 exact, sdk66 non-exact rollback). The one-call PAY API previously minted through `ensure_exact_coin` and therefore refused every laddered coin; fixed. The LN-latched piece is the one case that stays terminalized — it sits unclaimed past the pending-transfer lock's window. |
| `createLightningInvoice` | **DONE** | HODL-latch receive: the SSP fronts its own coin under a HODL invoice and the SE releases the preimage only once the payee's coin is claimable, so the SSP can take the LN money only after releasing the coin — no operator trust in the receive direction (sdk64 exact; sdk67 arbitrary amount, where the SSP's large laddered coin is split in-ladder and the piece conveyed). Single SE holds the preimage gate (Spark splits it across SOs via VSS — N/A with one SE). |
| RGB-asset Lightning swaps (colored channel / asset invoice / pay) | **DONE (legs)** | colored statechain latch (`tokens.rs::latch_tokens` for pay, `latch_tokens_se_preimage` for receive) + `SspClient` asset legs (`ssp.rs::create_receive_asset` / `ln_invoice_asset`); the LN half (issue -> colored channel -> asset invoice -> decode+pay over `RlnClient`) is verified end-to-end by sdk23. A full cross-rail swap additionally needs the asset bridge onto a statechain coin (documented follow-up). |
| `withdraw({onchainAddress, exitSpeed})` | **DONE** | SE co-signed direct spend; sub-coin branches auto-materialize; fee_rate param = exitSpeed |
| `unilateralExit` / `checkTimelock` | **DONE** | laddered coin: `unilateral_exit()` walks the TES-R chain (trigger → extension → state) as each **relative** CSV matures, reporting `wait_blocks` between tiers, and the funds land at the wallet's own seed-derived backup address (sdk50; deep multi-level chains after rollover, sdk43). Un-laddered coin: branch (no locktime) + stored pre-signed backup (absolute-locktime-gated). A watchtower can drive the same walk for an offline owner (sdk45 keyless bundle, sdk51 against a hostile trigger). Adversarial coverage of the child/exit bundles: sdk58, sdk54, sdk55. A child routed to a unilateral exit is no longer mis-marked WITHDRAWING (it has no withdrawal tx), so status polling terminates. |
| `getWithdrawalFeeQuote` / fee estimates | **DONE** | `get_withdrawal_fee_quote()` via electrum estimatefee (~111 vB/coin, sdk11); `estimate_exit_cost` for unilateral covers the whole tier chain (sdk29/sdk31/sdk34; the exit itself sdk50). An in-ladder split child funds its OWN two tiers plus dust, so admission is gated on `min_child_value` (1306 sat at 2 sat/vB), not the old 442-sat backup-fee floor — the split fee model is `tier_out_total` / `committed_fee_for_outputs`. Admitting a child below the floor terminalized the parent and only *then* failed, stranding it; fixed. |
| events (`TransferClaimed`, `DepositConfirmed`, balance updates) | **DONE (poll)** | broadcast-channel events from the watcher; + `TokenTransferClaimed`; no server push (documented) |
| `signMessageWithIdentityKey` / validate | **DONE** | `sign_message_with_identity_key` / `validate_message_with_identity_key` — BIP340 Schnorr over a stable identity key at m/1000h/0h/0h (sdk11 + unit) |
| leaf optimization / `optimizeLeaves` / swap service | N/A (superseded) | exact amounts are native (in-ladder split); no SSP swap pools needed. Opportunistic dust consolidation = backlog. |
| HTLC create/claim (`createHTLC`, `claimHTLC`) | PARTIAL | Mercury atomic transfer (tb03) + the HODL latch = preimage-gated transfers (sdk63/sdk64); generic HTLC API in SDK backlog |
| Spark invoices (`createSatsInvoice`, `createTokensInvoice`, `fulfillUtexoInvoice`) | **DONE** | `create_sats_invoice` / `create_tokens_invoice` (encode address+amount+asset+memo+expiry) + `fulfill_utexo_invoice` (decode, expiry-check, auto-pay); sdk11 + unit roundtrip |
| webhooks | N/A | server-side feature; poll/events instead (documented) |

## Issuer SDK (IssuerUtexoWallet → `mercury-issuer-sdk`)

| Spark method | Status | RGB mechanism |
|---|---|---|
| `createToken({name, ticker, decimals, maxSupply, isFreezable})` | **DONE** | `issue_token` — NIA issued + deposited onto a statechain coin in one colored tx (sdk02 green) |
| `mintTokens` (IFA) | **DONE** | `issue_inflatable_token` (IFA) + `mint_tokens` = on-chain inflate in the engine, minted supply bound to a fresh statechain coin (G3, sdk09). NIA supply is fixed at issuance by design. |
| `burnTokens` | **DONE** | `burn_tokens` burns engine-held free balance (on-chain). Statechain-bound supply must be exited first (documented). |
| `freezeTokens` / `unfreezeTokens` | N/A | RGB has no issuer freeze for fungible assets — client-side validation makes issuer freeze meaningless without consensus. Documented with rationale (this is a *feature* of RGB's trust model). |
| `getIssuerTokenBalances` / metadata / distribution | **DONE** | `get_token_balances` (settled/total per asset) |
| `getTokenL1Address` | **DONE** | `get_token_l1_address` (RGB engine funding address) |
| `queryTokenTransactions` | **DONE** | `query_token_transactions(asset)` -> (kind,status,amount,txid) from rgb-lib |
| token identifier (bech32m `btkn1…`) | NEW | RGB contract id (already string-encoded) exposed as the token identifier |

## Test parity (tracks)

1. **Integration tests mirroring Spark** — deposit (single + static-reuse + double-claim), transfer
   (basic, multi-leaf, interrupt/recovery, double-claim refusal), split/combine (exact and non-exact
   amounts), coop exit, unilateral exit (TES-R ladder walk + un-laddered branch/backup), Lightning
   HODL latch in both directions, adversarial (duplicate leaf, conflicting spend → SE refusal, wrong
   preimage).
2. **rgb-lib-mirror** — the rgb-lib on-chain suite (issue/send/receive/witness/blinded/fail cases)
   re-expressed over statechain coins.
3. **rgb-lib e2e over utexo-layer** — issue → deposit → off-chain DAG (split/combine/transfer) →
   exit → on-chain spend round-trips.
4. **TES-R protocol suite** — ladder establishment + consensus (sdk40, sdk48), transfer and lifecycle
   (sdk41, sdk42, sdk49), off-chain renewal and rollover (sdk43, sdk44), watchtower (sdk45 keyless
   bundle, sdk51 hostile trigger), R′ census (sdk46, sdk47, sdk54, sdk55), unilateral exit (sdk50),
   RGB carrier ⊥ ladder (sdk52), in-ladder split and first-class children (sdk58, sdk59, sdk60),
   Lightning (sdk63–sdk68).

## Docs parity (sitemap)

`docs/utexo/learn/`: tldr, trust-model, core-concepts (coins/DAG ↔ trees/leaves), technical-definitions,
deposits, transfers, lightning, withdrawals+unilateral-exit, tokens-on-rgb (hello-rgb, minting,
transferring, burning, why-no-freeze), limitations, faq.
`docs/utexo/build/`: getting-started (quickstart), wallet-sdk (create-wallet, addressing, balances,
transfer-bitcoin, transfer-tokens, deposit-from-l1, withdraw-to-l1, unilateral-exit, lightning,
testing-guide), issuer-sdk (create-token, mint, burn, distribute), api-reference (wallet + issuer),
local-dev-stack.
