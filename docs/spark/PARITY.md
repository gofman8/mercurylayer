# Spark ↔ Mercury+RGB feature parity matrix

Target: every user-visible Spark feature (docs.spark.money + `@buildonspark/spark-sdk` +
`@buildonspark/issuer-sdk`) implemented on Mercury Layer with a **single SE** (blind-MuSig2 2-of-2,
lockbox) instead of FROST multi-operator, and **RGB** (UTEXO-Protocol/rgb-lib) instead of the
BTKN/LRC-20 token standard.

Status legend: **NATIVE** = Mercury dev already has it · **PORT** = exists on `feat/rgb-statechain`,
ported here · **NEW** = built in this effort · **PARTIAL** = works with a documented mechanism
difference · **N/A** = not applicable to the single-SE / RGB design (rationale given).

## Architecture mapping

| Spark concept | This implementation |
|---|---|
| Spark Entity (SE) = t-of-n FROST Spark Operators | Mercury SE: one server + lockbox enclave, blind MuSig2 2-of-2 (owner+SE). Trust: Spark = 1-of-n operators honest; here = the one SE honest. Both: user can always exit unilaterally without the SE. |
| FROST 2-round threshold signing, consensus engine, gossip | N/A — replaced by Mercury's sign_first/sign_second blind-MuSig2 exchange. No consensus/2-phase commit needed with one signer. |
| Leaf (TreeNode, P2TR user+SO key) | Statechain coin (P2TR aggregated owner+SE key). |
| Tree: root deposit → branches → leaves | Deposit coin → off-chain split/combine DAG (un-broadcast SE-co-signed txs; sub-coins = witness outputs). One tree per deposit → forest. |
| Timelock decrement (2000 → −100/transfer, renew ≤300) | NATIVE for flat coins: Mercury decrementing-nLockTime backup txs per transfer (tb05). Tree sub-coins instead use **SE single-use per node** (no race possible: child spends parent) + epoch deadline. PARTIAL: mechanism differs, guarantee (current owner exits first / bounded lifetime) preserved. |
| Leaf renewal (`renew_leaf`) | PARTIAL: Mercury coins have a bounded lifetime (locktime floor); renewal ≈ self-transfer refresh (re-anchor, decrements) or exit+redeposit. Documented limitation. |
| SSP (Spark Service Provider): swaps, LN gateway, coop-exit connector | No SSP needed for BTC coop exit (SE co-signs a direct spend). For Lightning, the latch counterparty (any LSP running LND) plays the SSP role. Leaf-denomination swaps are unnecessary: multi-input combine + split makes exact amounts natively (Spark needs SSP swaps because leaves are fixed denominations). |
| Spark address (bech32m `spark1…`, identity pubkey + invoice fields) | NEW: bech32m statechain address encoding (existing Mercury transfer address) + invoice fields for the SDK. |
| BTKN token (CREATE/MINT/TRANSFER/FREEZE, TTXOs) | RGB assets (NIA/UDA/CFA/IFA) on statechain coins; allocations ride the same coins/sub-coins. See token section. |

## Wallet SDK (SparkWallet → `mercury-spark-sdk`)

| Spark method | Status | Mechanism |
|---|---|---|
| `initialize({mnemonic, network, options})` | **DONE** | `SparkWallet::initialize(SdkConfig, Option<mnemonic>)` |
| `getIdentityPublicKey` / `getSparkAddress` | **DONE** | stable `ml1…/tml1…` statechain address |
| `getBalance` (BTC + tokenBalances) | **DONE** | available/pending/in-transfer sats + per-asset RGB balances |
| `getSingleUseDepositAddress` | **DONE** | `get_deposit_address(amount)` |
| `getStaticDepositAddress` / `queryStaticDepositAddresses` | PARTIAL | Mercury addresses are per-coin; SDK re-issues a fresh deposit slot bound to the same key on use + duplicate detection (`check_for_duplicated`) covers reuse. Documented difference. |
| `claimDeposit` / auto-claim | **DONE** | `claim()` + `start_background()` watcher, `DepositConfirmed` events |
| `transfer({receiverSparkAddress, amountSats})` | **DONE** | exact-subset selection OR off-chain split minting the exact amount; **branch-carrying transfers** let receivers verify un-broadcast sub-coins (consensus-validated back to an on-chain root) |
| `transferTokens({tokenId, receiver, amount})` | **DONE** | colored off-chain split + handover; consignment rides the transfer message; receiver books under the consignment's VERIFIED contract id (sdk02 green) |
| `getTransfers` / `getTransfer` | NEW | activity log (Mercury activities + RGB transfers) |
| `payLightningInvoice` | **DONE (legs)** | `start_lightning_swap` / `get_swap_payment_hash` / `settle_lightning_swap` on the Mercury latch (sdk03 green); BOLT11 orchestration stays in the LSP's node |
| `createLightningInvoice` | PARTIAL | latch receive leg: invoice created by LSP with payment_hash bound to a latch transfer; SDK exposes the flow. Single SE holds the preimage gate (Spark splits it across SOs via VSS — N/A with one SE). |
| `withdraw({onchainAddress, exitSpeed})` | **DONE** | SE co-signed direct spend; sub-coin branches auto-materialize; fee_rate param = exitSpeed |
| `unilateralExit` / `checkTimelock` | **DONE** | branch (no locktime) + stored pre-signed backup (locktime-gated); coin locktimes visible on the record |
| `getWithdrawalFeeQuote` / fee estimates | NEW | electrum fee estimation; simple quote (no SSP pricing) |
| events (`TransferClaimed`, `DepositConfirmed`, balance updates) | **DONE (poll)** | broadcast-channel events from the watcher; + `TokenTransferClaimed`; no server push (documented) |
| `signMessageWithIdentityKey` / validate | NEW | Schnorr sign/verify with identity key |
| leaf optimization / `optimizeLeaves` / swap service | N/A (superseded) | exact amounts are native (off-chain split); no SSP swap pools needed. Opportunistic dust consolidation = backlog. |
| HTLC create/claim (`createHTLC`, `claimHTLC`) | PARTIAL | Mercury atomic transfer (tb03) + latch = preimage-gated transfers; generic HTLC API in SDK backlog |
| Spark invoices (`createSatsInvoice`, `fulfillSparkInvoice`) | NEW | invoice fields in the address encoding + SDK fulfil (auto pay to embedded amount/asset) |
| webhooks | N/A | server-side feature; poll/events instead (documented) |

## Issuer SDK (IssuerSparkWallet → `mercury-issuer-sdk`)

| Spark method | Status | RGB mechanism |
|---|---|---|
| `createToken({name, ticker, decimals, maxSupply, isFreezable})` | **DONE** | `issue_token` — NIA issued + deposited onto a statechain coin in one colored tx (sdk02 green) |
| `mintTokens` | PARTIAL | NIA: full supply at issuance (mint-at-create). IFA: inflate op = true mint. SDK picks by asset schema. |
| `burnTokens` | PARTIAL | IFA burn op; NIA: send-to-provably-unspendable documented |
| `freezeTokens` / `unfreezeTokens` | N/A | RGB has no issuer freeze for fungible assets — client-side validation makes issuer freeze meaningless without consensus. Documented with rationale (this is a *feature* of RGB's trust model). |
| `getIssuerTokenBalances` / metadata / distribution | **DONE** | `get_token_balances` (settled/total per asset) |
| token identifier (bech32m `btkn1…`) | NEW | RGB contract id (already string-encoded) exposed as the token identifier |

## Test parity (tracks)

1. **Spark-mirror integration tests** — deposit (single + static-reuse + double-claim), transfer
   (basic, multi-leaf, interrupt/recovery, double-claim refusal), split/combine (exact amounts),
   coop exit, unilateral exit (timelock + branch), latch/LN legs, adversarial (duplicate leaf,
   conflicting spend → SE refusal, wrong preimage).
2. **rgb-lib-mirror** — the rgb-lib on-chain suite (issue/send/receive/witness/blinded/fail cases)
   re-expressed over statechain coins.
3. **rgb-lib e2e over spark-layer** — issue → deposit → off-chain DAG (split/combine/transfer) →
   exit → on-chain spend round-trips.

## Docs parity (sitemap)

`docs/spark/learn/`: tldr, trust-model, core-concepts (coins/DAG ↔ trees/leaves), technical-definitions,
deposits, transfers, lightning, withdrawals+unilateral-exit, tokens-on-rgb (hello-rgb, minting,
transferring, burning, why-no-freeze), limitations, faq.
`docs/spark/build/`: getting-started (quickstart), wallet-sdk (create-wallet, addressing, balances,
transfer-bitcoin, transfer-tokens, deposit-from-l1, withdraw-to-l1, unilateral-exit, lightning,
testing-guide), issuer-sdk (create-token, mint, burn, distribute), api-reference (wallet + issuer),
local-dev-stack.
