# Spark SDK surface + docs sitemap — condensed notes

Distilled from `@buildonspark/spark-sdk`, `@buildonspark/issuer-sdk`, docs.spark.money.

The method lists, hidden automations, address format and sitemap below are **Spark's** — the parity
target we measured against. Anything marked "ours" describes the **shipped** Mercury Utexo stack, not
a plan; the protocol behind it is [PROTOCOL.md](../PROTOCOL.md) (TES-R),
[CHILDREN.md](../CHILDREN.md), [LIGHTNING.md](../LIGHTNING.md).

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
3. **Leaf optimization**: auto split/consolidate via SSP swaps (ours: no swap counterparty —
   `transfer` mints the exact amount itself via the **in-ladder split**, and an RGB payment spanning
   several carriers is combined internally; see "Non-exact payment" below).
4. **Timelock renewal**: checkRenewLeaves auto-renews at low locks (ours: **no scheduled renewal and
   no deadline to watch** — a laddered coin's timelocks are RELATIVE and its trigger stays
   un-broadcast, so an idle coin never ages; renewal and rollover are off-chain and unbounded; see
   "Ageing" below).
5. Lightning route choice (Spark-vs-LN legs), idempotency keys, fee quotes before commit,
   singleton instance guard, claim polling fallback (React Native).

## Our shipped equivalents — where the two surfaces actually diverge

**Coin shape.** Spark's unit is a tree leaf: a pre-signed node tx + refund tx, relative lock 2000
blocks, ~100 off every transfer, `renew_leaf` once it runs low. Ours is a **laddered coin (TES-R)** —
three pre-signed tiers over one funding outpoint. `claim()` ladders every fresh confirmed ROOT
deposit unconditionally; the old opt-in switches (`deposit_protocol_version`, the
`UTEXO_PROTOCOL_DEFAULT` env) are deleted.

```
F (funding, on-chain)
└─▶ T   TRIGGER    — no timelock, signed once at deposit
    └─▶ X_m EXTENSION — relative CSV E_m (renewal replaces it horizontally)
        └─▶ S_k STATE — relative CSV Δ_k (decrements by δ per transfer)
```

All three tiers are v3/TRUC with a P2A anchor, pre-signed and **un-broadcast**. A transfer co-signs a
fresh state one δ **lower** than the one it replaces (replace-by-lower-timelock), so the new owner's
state always matures first; the superseded state is disclosed and counted by the receiver's census.

**Ageing — the change that reshapes the whole UX.** BIP-68 relative locks only start counting once
the PARENT confirms, and `T` carries no timelock, so **nothing matures until someone broadcasts
`T`**. An idle laddered coin never ages: no calendar deadline, no "exit before your floor", **0 vB of
idle rent**. (Classic Mercury coins — the un-laddered shape below — do age: their backups carry
absolute nLOCKTIMEs. Do not generalise that model to a laddered coin.) Renewal (a lower-CSV extension
replacing `X_m`) and rollover (a fresh level) are **off-chain and unbounded** — a coin can live
off-chain forever (sdk43). `refresh(statechain_id, fee_rate)` is the **re-anchor** primitive: one
on-chain tx that moves the coin to a fresh funding outpoint and mints a new ladder (sdk30), not a
deadline reset. `refresh_sponsored` adds an operator-paid off-chain rebate of
`max(fee + dust, min_child_value)`.

**One protocol, two coin shapes.** Not every coin is laddered, by design — the second shape is
current, not legacy:
* **Laddered** — every plain BTC deposit.
* **Un-laddered** — an **RGB carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation: terminal freeze), and a **split sub-coin** whose funding tx is un-broadcast
  cannot root a trigger. These keep the classic **backup chain**: one pre-signed backup per hop, each
  at an absolute nLOCKTIME one interval below the last, handed over on transfer. `split_coin` /
  `ensure_exact_coin` mint un-laddered sub-coins off-chain on this lane (they refuse a laddered
  parent — that is what `in_ladder_pay` is for). Current and load-bearing for RGB assets, not
  deprecated (sdk52, sdk39).

**Exit** (`unilateral_exit` → `ExitStatus{complete, wait_blocks}`; Spark's `checkTimelock`). Three
lanes, chosen per coin: a laddered coin **walks the pre-signed chain tier by tier**, waiting out each
relative timelock and re-called as the chain advances (sdk50); a received split child walks
`T → X_m → SP → ext_child → state_child` (`exit_child_pass`, keyless — every tier is already signed);
an un-laddered coin broadcasts its exit branch plus the latest backup once its nLOCKTIME is reached.
`defend_ladders` is the owner-run watchtower pass: a **no-op** while `F` is unspent, and a race the
owner wins if someone triggers, because the adopted current state carries the strictly-lowest CSV. A
delegated tower runs the same pass from the exported watch bundle with **no key material**, and a
second tower is idempotent (sdk45).

**Non-exact payment — the in-ladder split** (Spark's reason for leaf swaps). A state tier `SP` spends
`X_m.out[0]` — a **descendant of the trigger**, never a rival for `F` — paying a piece child and a
change child. Admission floor is `min_child_value` = **1306 sat at 2 sat/vB** (a child funds its own
two tiers plus dust). `in_ladder_pay` splits a parent; `child_in_ladder_pay` splits a received child
(a depth-2 ancestors chain). sdk58, sdk59.

**Received children are first-class** (Spark parity for multi-hop). The claim completes the standard
SE key handover, so the receiver co-owns `A_child` — invariant across the rotation, which is what
keeps the pre-signed exit chain valid — and the sender is permanently locked out. A child can be paid
onward off-chain, whole (`transfer()` routes to `child_retransfer`) or split
(`child_in_ladder_pay`): one co-signature and one disclosed superseded state per hop, counted by the
receiver's census. sdk60 (alice→bob→carol, funding outpoint unspent throughout), sdk17 (partial
second hop). [CHILDREN.md](../CHILDREN.md).

**Lightning** (Spark VSS-splits the preimage across operators; we have a single SE). Ours is a
**HODL-invoice latch** through the SSP, working both directions on the ladder: pay sdk63, receive
sdk64, non-exact pay sdk65, non-exact receive sdk67, failure + rollback sdk66/sdk68, adversarial
sdk19/20/24/25, remote SSP over HTTP sdk21. The LN-latched piece is the one case that stays
**terminalized** — it sits unclaimed past the pending-transfer lock's window.
[LIGHTNING.md](../LIGHTNING.md).

## IssuerUtexoWallet adds (~8)
createToken({tokenName, tokenTicker, decimals, isFreezable, maxSupply}) → tokenId (bech32m `btkn1…`);
mintTokens; burnTokens; freezeTokens/unfreezeTokens({tokenId, sparkAddress});
getIssuerTokenBalance(s); getIssuerTokensMetadata; getIssuerTokenDistribution
{totalMinted, burned, frozen, circulating}.

## Address format
bech32m of identity pubkey (+ optional invoice TLVs: version, id, paymentType sats|tokens{tokenId,
amount}, memo, senderPublicKey, expiryTime). HRPs: spark/sparkt/sparkrt/sparks/sparkl.

**Ours (shipped)** — the `mrc*` HRP idea was dropped; we reuse the Mercury transfer-address
encoding and carry the invoice fields in a separate envelope:
* **Receive address** = the wallet's stable statechain address, bech32m with HRP `ml` (mainnet) /
  `tml` (testnet+regtest) — `get_utexo_address()`. This is what `transfer(receiver_address, sats)`
  takes.
* **Invoice** = `utexoinv1<hex(json)>` over `{version, address, amount, asset_id, memo,
  expiry_unix}`; `asset_id: None` ⇒ a sats invoice, `Some(contract_id)` ⇒ an RGB token invoice.
  `create_sats_invoice` / `create_tokens_invoice` / `fulfill_utexo_invoice`, mirroring Spark's
  `createSatsInvoice` / `createTokensInvoice` / `fulfillUtexoInvoice` (sdk11).

## Docs sitemap studied (Spark's, compressed)
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

## What we actually built (use these paths for links)
The mirror landed as a flatter tree — fewer, denser pages, Rust-first code:
* **learn/** (conceptual prose): [tldr](../learn/tldr.md) · [core-concepts](../learn/core-concepts.md)
  · [trust-model](../learn/trust-model.md) · [transfers](../learn/transfers.md) ·
  [lightning](../learn/lightning.md) · [exits](../learn/exits.md) (deposits + cooperative and
  unilateral exit) · [tokens](../learn/tokens.md) (incl. why there is no freeze) ·
  [invalidation](../learn/invalidation.md) + [invalidation-deep-dive](../learn/invalidation-deep-dive.md)
  · [granularity-deep-dive](../learn/granularity-deep-dive.md).
* **build/** (practical, with code): [getting-started](../build/getting-started.md) ·
  [wallet-sdk](../build/wallet-sdk.md) · [issuer-sdk](../build/issuer-sdk.md) ·
  [api-reference](../build/api-reference.md) (`UtexoWallet` method-by-method) ·
  [testing-guide](../build/testing-guide.md).
* **Protocol / normative**: [PROTOCOL.md](../PROTOCOL.md) (TES-R) · [CHILDREN.md](../CHILDREN.md) ·
  [LIGHTNING.md](../LIGHTNING.md) · [SPEC.md](../SPEC.md) · [TRUST-MODEL.md](../TRUST-MODEL.md) ·
  [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md) · [GRANULARITY-SPEC.md](../GRANULARITY-SPEC.md).
* **Status / history**: [PARITY.md](../PARITY.md) · [ARK-SPARK-PARITY.md](../ARK-SPARK-PARITY.md) ·
  [PLAN.md](../PLAN.md) · [PROGRESS.md](../PROGRESS.md) · [REVIEW.md](../REVIEW.md) ·
  [AUDIT-2026-07.md](../AUDIT-2026-07.md) · [RGB-TEST-PARITY.md](../RGB-TEST-PARITY.md) ·
  [history/MIGRATION.md](../history/MIGRATION.md) ·
  [history/SPLIT-FINDINGS.md](../history/SPLIT-FINDINGS.md).

Pages we deliberately did **not** mirror: `frost-signing` (single SE, blind-MuSig2 — covered in
core-concepts + TRUST-MODEL), `freeze-tokens` (no consensus meaning in RGB — answered in
learn/tokens), separate `spark-invoices` / `estimate-fees` / per-direction deposit+withdraw pages
(folded into wallet-sdk + api-reference).

E2E citations in these docs use the live dispatch range `SDK_E2E=1..68` (with gaps where tests were
retired) plus `chaos22`, and the `rgb*`/`ta*`/`tb*` suites — see
[build/testing-guide.md](../build/testing-guide.md) for how to run them.
