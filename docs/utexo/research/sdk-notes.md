# Spark SDK surface + docs sitemap — condensed notes

Distilled from `@buildonspark/spark-sdk`, `@buildonspark/issuer-sdk`, docs.spark.money.

## UtexoWallet public surface (~50 methods) — parity checklist

**Init/config**: `initialize({mnemonicOrSeed?, options})` → `{wallet, mnemonic}`;
`getOrCreateWallet` (singleton per identity); config: network, electrsUrl, optimizationOptions
{auto, multiplicity}, tokenOptimizationOptions {enabled, intervalMs}, events, log.

**Query**: getBalance → `{balance: bigint, tokenBalances: Map<tokenId, {ownedBalance,
availableToSendBalance, tokenMetadata}>}`; getCachedBalance; getLeaves; getIdentityPublicKey;
getUtexoAddress; getTransfers(limit, cursor)/getTransfer(id).

**Deposit**: getSingleUseDepositAddress; getStaticDepositAddress; queryStaticDepositAddresses;
getUnusedDepositAddresses; getUtxosForDepositAddress; claimDeposit(txid);
getClaimStaticDepositQuote(txid) → {creditAmountSats, sspSignature}; claimStaticDeposit;
claimStaticDepositWithMaxFee; refundStaticDeposit; advancedDeposit(txHex).

**Transfer**: transfer({receiverUtexoAddress, amountSats}) → WalletTransfer{id, status, totalValue,
expiryTime, leaves, type, transferDirection}; transferV2({receivers[]}) multi-recipient.

**Lightning**: createLightningInvoice({amountSats, memo, expirySeconds}); createLightningHodlInvoice
({paymentHash}); payLightningInvoice({invoice, maxFeeSats, preferSpark}); getLightningSendFeeEstimate;
getLightningReceive/SendRequest(id).

**Withdraw**: withdraw({onchainAddress, exitSpeed: SLOW|MEDIUM|FAST, amountSats?, feeQuoteId?});
getWithdrawalFeeQuote; getCoopExitRequest(id); checkTimelock(nodeId) → {blocksRemaining, canExit}.

**Tokens**: transferTokens({tokenIdentifier, receiverUtexoAddress, tokenAmount}) → txid;
batchTransferTokens; queryTokenTransactions; getTokenL1Address.

**HTLC**: createHTLC({receiver, amountSats, preimage?, expiryTime}); claimHTLC(preimage);
getHTLCPreimage(transferId).

**Invoices**: createSatsInvoice({amountSats, memo}); createTokensInvoice; fulfillUtexoInvoice
([invoices]); queryUtexoInvoices.

**Signing**: signMessageWithIdentityKey; validateMessageWithIdentityKey.

**Events** (`wallet.on(...)`): BalanceUpdate{available, owned, incoming}; TokenBalanceUpdate;
TransferClaimed(transferId, newBalance); DepositConfirmed(depositId, newBalance);
StreamConnected/Disconnected/Reconnecting.

**Lifecycle**: cleanup(); cleanupConnections().

## Hidden automations (the UX bar to meet)
1. Background stream + **auto-claim of incoming transfers** (event → claimTransfer internally).
2. **Deposit confirmation polling** → auto-claim (user only shares the address).
3. **Leaf optimization**: auto split/consolidate via SSP swaps (ours: combine/split directly).
4. **Timelock renewal**: checkRenewLeaves auto-renews at low locks (ours: N/A in trees; flat coins
   surfaced via activity/warnings).
5. Lightning route choice (Spark-vs-LN legs), idempotency keys, fee quotes before commit,
   singleton instance guard, claim polling fallback (React Native).

## IssuerUtexoWallet adds (~8)
createToken({tokenName, tokenTicker, decimals, isFreezable, maxSupply}) → tokenId (bech32m `btkn1…`);
mintTokens; burnTokens; freezeTokens/unfreezeTokens({tokenId, sparkAddress});
getIssuerTokenBalance(s); getIssuerTokensMetadata; getIssuerTokenDistribution
{totalMinted, burned, frozen, circulating}.

## Address format
bech32m of identity pubkey (+ optional invoice TLVs: version, id, paymentType sats|tokens{tokenId,
amount}, memo, senderPublicKey, expiryTime). HRPs: spark/sparkt/sparkrt/sparks/sparkl.
Ours: hrp `mrc`(main)/`mrct`(test)/`mrcrt`(regtest) or reuse Mercury transfer-address encoding —
decide in P2 (keep TLV invoice fields idea).

## Docs sitemap to mirror (compressed)
**learn/**: tldr · sovereignty · scalability · trust-model · limitations · faq · core-concepts
(SE/operators/SSP, branches+leaves, exit txs — tree diagrams) · technical-definitions (statechains,
Schnorr, timelocks, key tweaking) · frost-signing[→ our blind-musig2 page] · deposits · transfers ·
lightning · withdrawals · unilateral-exit · tokens/{hello-btkn→hello-rgb, core-concepts, minting,
transferring, freezing→why-no-freeze, burning, glossary}.
**build/wallets/**: overview · typescript[→rust+nodejs] · create-wallet · addressing · balances ·
transfer-bitcoin · transfer-tokens · spark-invoices · deposit-from-l1 · deposit-from-lightning ·
withdraw-to-l1 · withdraw-to-lightning · unilateral-exit · estimate-fees · testing-guide · faq.
**build/issuance/**: overview · create-token · mint-tokens · transfer-tokens · burn-tokens ·
[freeze-tokens → N/A page] · testing-guide.
**api-reference/**: wallet-overview (method-by-method) · issuer-overview.
**quickstart/**: create-wallet (6-step CLI) · launch-token (5-step CLI).
Style: TypeScript-first code blocks with expected output, short concept pages, callout boxes,
progressive complexity, "why" before "how".
