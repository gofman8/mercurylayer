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
| `initialize({mnemonic, network, options})` | NEW | wallet create/restore from mnemonic (Mercury key derivation), config = SE URL + electrum + RGB proxy |
| `getIdentityPublicKey` / `getSparkAddress` | NEW | identity key = wallet root; bech32m address encode |
| `getBalance` (BTC + tokenBalances) | NEW | sum coin amounts by status + rgb-lib per-asset balances across registered coins |
| `getSingleUseDepositAddress` | NATIVE | Mercury deposit init → aggregated address |
| `getStaticDepositAddress` / `queryStaticDepositAddresses` | PARTIAL | Mercury addresses are per-coin; SDK re-issues a fresh deposit slot bound to the same key on use + duplicate detection (`check_for_duplicated`) covers reuse. Documented difference. |
| `claimDeposit` / auto-claim | NEW | SDK polling loop: detect UTXO at deposit address → update coin → auto-confirm (Spark's deposit-confirmation polling equivalent) |
| `transfer({receiverSparkAddress, amountSats})` | NATIVE+NEW | Mercury transfer_sender/receiver (key handover, statechain_id rotation) wrapped with **automatic amount-making**: exact-amount coin = auto split/combine of owned coins before transfer |
| `transferTokens({tokenId, receiver, amount})` | PORT+NEW | RGB transfer over statechain: colored split (change stays) / anchor-refresh self-transfer / off-chain consignment + validate_offchain_chain |
| `getTransfers` / `getTransfer` | NEW | activity log (Mercury activities + RGB transfers) |
| `payLightningInvoice` | NATIVE (protocol) | Mercury **lightning latch** (server endpoints on dev): preimage-gated transfer to the LSP counterparty = Spark's preimage swap REASON_SEND |
| `createLightningInvoice` | PARTIAL | latch receive leg: invoice created by LSP with payment_hash bound to a latch transfer; SDK exposes the flow. Single SE holds the preimage gate (Spark splits it across SOs via VSS — N/A with one SE). |
| `withdraw({onchainAddress, exitSpeed})` | NATIVE | Mercury withdraw: SE co-signs direct spend to L1 address (no SSP/connector needed). exitSpeed → fee rate. |
| `unilateralExit` / `checkTimelock` | NATIVE+PORT | flat coin: broadcast backup tx after locktime; tree sub-coin: broadcast the branch (split/combine chain) — RGB anchors settle on broadcast |
| `getWithdrawalFeeQuote` / fee estimates | NEW | electrum fee estimation; simple quote (no SSP pricing) |
| events (`TransferClaimed`, `DepositConfirmed`, balance updates) | NEW | SDK event loop (poll-based; Mercury has no server stream) → PARTIAL: no server push, documented |
| `signMessageWithIdentityKey` / validate | NEW | Schnorr sign/verify with identity key |
| leaf optimization / `optimizeLeaves` / swap service | N/A→NEW | unnecessary as a service: exact amounts via multi-input combine + split (PORT). SDK still auto-consolidates dust coins (combine) opportunistically. |
| HTLC create/claim (`createHTLC`, `claimHTLC`) | PARTIAL | Mercury atomic transfer (tb03) + latch = preimage-gated transfers; generic HTLC API in SDK backlog |
| Spark invoices (`createSatsInvoice`, `fulfillSparkInvoice`) | NEW | invoice fields in the address encoding + SDK fulfil (auto pay to embedded amount/asset) |
| webhooks | N/A | server-side feature; poll/events instead (documented) |

## Issuer SDK (IssuerSparkWallet → `mercury-issuer-sdk`)

| Spark method | Status | RGB mechanism |
|---|---|---|
| `createToken({name, ticker, decimals, maxSupply, isFreezable})` | NEW | `issue_asset_nia` (fixed supply) or `issue_asset_ifa` (inflatable) on a statechain-funded UTXO; metadata = RGB contract fields |
| `mintTokens` | PARTIAL | NIA: full supply at issuance (mint-at-create). IFA: inflate op = true mint. SDK picks by asset schema. |
| `burnTokens` | PARTIAL | IFA burn op; NIA: send-to-provably-unspendable documented |
| `freezeTokens` / `unfreezeTokens` | N/A | RGB has no issuer freeze for fungible assets — client-side validation makes issuer freeze meaningless without consensus. Documented with rationale (this is a *feature* of RGB's trust model). |
| `getIssuerTokenBalances` / metadata / distribution | NEW | rgb-lib balance/metadata per contract; distribution = issued − burned |
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
