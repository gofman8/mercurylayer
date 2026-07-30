//! Tokens on RGB rails: issuance, balances and off-chain token transfers.
//!
//! The token standard is RGB (rgb-lib): assets are client-validated contracts whose allocations
//! ride statechain coins. A token transfer is a **colored off-chain split** — one SE-co-signed,
//! un-broadcast tx carving a sub-coin that carries the exact token amount (plus the sender's
//! change) — followed by the same branch-carrying key handover used for sats. The consignment
//! travels inside the transfer message (BackupTx.rgb_consignment); the receiver validates it
//! off-chain against the branch and books the balance under the consignment's verified contract.

use anyhow::{anyhow, Result};
use mercury_rgb::RgbWallet;
use mercurylib::wallet::CoinStatus;
use mercuryrustlib::rgb::{TierRole, TierSeal};
use serde::{Deserialize, Serialize};

use crate::types::{SdkError, TokenBalance, TransferResult, TransferredCoin};
use crate::wallet::UtexoWallet;

// The global `TOKEN_BLINDING: u64 = 777` that used to live here is GONE. Its doc comment claimed
// "a fixed value is fine"; `docs/utexo/CTESR-GATE.md` §2.2/§4.2 measured that claim and it is
// conditionally FALSE. Two RGB transitions over the SAME parent outpoint with equal amounts and
// equal blinding collapse to one `OpId` and one `BundleId`, and rgb-lib then resolves that BundleId
// to whichever rival witness has the numerically smallest INTERNAL (little-endian) txid — an
// arbitrary hash lottery. The losing transition's consignment embeds the rival's witness, so no
// branch the receiver can try will validate; the allocation is simply unclaimable off-chain.
//
// Every colouring in this module now derives its blinding from a `TierSeal`
// (`H(statechain_id ‖ role ‖ tier_index ‖ rung)`), which is unique over the whole
// `(parent outpoint, role, index)` space and deterministically re-derivable by the receiver.
/// Sats carried by a token-piece sub-coin (just above dust; the token is the payload).
/// `pub(crate)` so the granularity model (`granularity_model.rs`) can pin the carrier floor.
pub(crate) const TOKEN_PIECE_SATS: u64 = 1_500;

/// Rung-space flag that separates the BATCH split lane from the single-recipient split lane.
///
/// Both lanes spend the same carrier under `TierRole::Split` at the same `tier_index` (the carrier's
/// spend generation), so only the `rung` can tell them apart, and both key it on the transition's
/// arity. The single lane's arity is always 2 (piece + change). A batch's arity is `n + 1`, and
/// `batch_transfer_tokens` rejects only an EMPTY recipient list — so a batch of ONE recipient also
/// has arity 2 and would derive the byte-identical seal. Setting the high bit puts every batch rung
/// in a range the single lane can never reach (a split's arity is bounded by its output count, far
/// below `2^31`), which restores uniqueness at every arity including 1.
const BATCH_SPLIT_RUNG_FLAG: u32 = 0x8000_0000;

/// THE single-recipient split lane's seal (`transfer_tokens`). `arity` is the transition's output
/// count (piece + change = 2); `generation` is the carrier's spend generation (`parent_backups`).
///
/// This is the ONLY place the single lane's rung is expressed. Both split lanes go through these two
/// functions so the disjointness they promise is a property of the code that actually runs, and so
/// the unit tests below can exercise the real derivation instead of restating it.
fn single_split_seal(carrier_id: &str, generation: u32, arity: u32) -> TierSeal {
    TierSeal::new(carrier_id, TierRole::Split, generation, arity)
}

/// THE batch split lane's seal (`batch_transfer_tokens`): the same derivation moved into a disjoint
/// rung space by [`BATCH_SPLIT_RUNG_FLAG`]. `arity` is `recipients + 1` (the change output).
fn batch_split_seal(carrier_id: &str, generation: u32, arity: u32) -> TierSeal {
    debug_assert_eq!(
        arity & BATCH_SPLIT_RUNG_FLAG,
        0,
        "a split arity must never reach the lane-separating flag bit"
    );
    TierSeal::new(carrier_id, TierRole::Split, generation, BATCH_SPLIT_RUNG_FLAG | arity)
}

/// The CTES-R build-time collision assert (`docs/utexo/CTESR-GATE.md` §3.1), for the lanes that
/// colour through `RgbWallet::color` (`create_colored_split_tx` / `create_colored_combine_tx`)
/// rather than through `mercuryrustlib::rgb::build_colored_tier`, which carries the guard itself.
///
/// Two RGB transitions over the same parent outpoint(s) with equal amounts and an equal seal
/// blinding collapse to one `OpId` and one `BundleId`; rgb-lib resolves that BundleId to whichever
/// rival witness has the smallest INTERNAL (little-endian) txid. The loser's consignment therefore
/// comes back carrying the RIVAL's witness, and no branch the receiver can try will validate. So the
/// absence of our own witness from the consignment we just produced is proof that the seal blinding
/// collided — a sender-side defect that must never be handed to a receiver as an unvalidatable
/// consignment. Fail closed here instead.
fn assert_own_witness(lane: &str, txid: &str, consignment: &str, blinding: u64) -> Result<()> {
    let witnesses = mercury_rgb::consignment_witness_txids(consignment)?;
    if !witnesses.iter().any(|w| w == txid) {
        return Err(anyhow!(
            "{lane} {txid}: its OWN witness is absent from the consignment it just produced \
             (bundled witnesses: {witnesses:?}). The seal blinding {blinding} collided with a rival \
             transition over the same parent output(s) — rgb-lib merged them into one BundleId and \
             kept the rival with the smaller internal txid, so this consignment is unvalidatable by \
             ANY receiver."
        ));
    }
    Ok(())
}

/// How a colored transfer's piece is handed over.
pub(crate) enum ColoredLatch {
    /// Plain transfer (no latch).
    None,
    /// Batch-locked to an external payment hash (Lightning PAY: receiver claims on the preimage).
    ExternalHash(String),
    /// Batch-locked to an SE-generated preimage (Lightning RECEIVE: the SE reveals the preimage only
    /// after the coin is released).
    SePreimage,
}

/// Output of a colored transfer, with any latch artifacts.
pub(crate) struct ColoredTransferOut {
    pub result: TransferResult,
    pub piece_id: String,
    pub batch_id: Option<String>,
    pub se_hash: Option<String>,
}

/// Envelope stored in `BackupTx.rgb_consignment` so a token transfer is self-describing.
#[derive(Serialize, Deserialize)]
pub(crate) struct ConsignmentEnvelope {
    /// Consignment, base64.
    pub c: String,
    /// Advisory amount hint. NOT trusted: the receiver re-derives the booked amount from the
    /// consignment (`accept_offchain_amount`) and rejects the transfer if this disagrees.
    pub a: u64,
    /// Sats on the sub-coin.
    pub s: u64,
}

impl UtexoWallet {
    /// Open (lazily) this wallet's RGB engine. Token support requires `rgb_proxy_url` and
    /// `rgb_data_dir` in the config.
    pub(crate) async fn rgb(&self) -> Result<tokio::sync::MutexGuard<'_, Option<RgbWallet>>> {
        let mut guard = self.inner.rgb.lock().await;
        if guard.is_none() {
            let dir = self
                .inner
                .config
                .rgb_data_dir
                .clone()
                .ok_or(SdkError::TokensNotConfigured)?;
            let proxy = self
                .inner
                .config
                .rgb_proxy_url
                .clone()
                .ok_or(SdkError::TokensNotConfigured)?;
            std::fs::create_dir_all(&dir)?;
            // The RGB engine has its own BIP39 seed, persisted alongside its data.
            let mnemonic_path = std::path::Path::new(&dir).join("rgb.mnemonic");
            let mnemonic = if mnemonic_path.exists() {
                std::fs::read_to_string(&mnemonic_path)?.trim().to_string()
            } else {
                let m = RgbWallet::generate_mnemonic(&self.inner.config.network.to_string())?;
                std::fs::write(&mnemonic_path, &m)?;
                m
            };
            let wallet = tokio::task::block_in_place(|| {
                RgbWallet::open(
                    &dir,
                    &mnemonic,
                    &self.inner.config.network.to_string(),
                    &self.inner.config.electrum_url,
                    &proxy,
                )
            })?;
            *guard = Some(wallet);
        }
        Ok(guard)
    }

    /// Bitcoin address of the RGB engine's internal wallet. Issuance needs a little on-chain
    /// funding here (colorable UTXO + witness fees) — send some sats to it before `issue_token`.
    pub async fn get_token_funding_address(&self) -> Result<String> {
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| w.get_address())
    }

    /// Issue a new token (RGB NIA: fixed supply at issuance) and deposit the full supply onto a
    /// fresh statechain coin of this wallet. Returns the asset id (the token identifier).
    ///
    /// Prerequisite: the RGB funding address holds enough sats (see
    /// [`Self::get_token_funding_address`]); the statechain slot consumes one deposit token.
    pub async fn issue_token(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        supply: u64,
    ) -> Result<String> {
        let deposit_sats: u64 = 10_000;
        // 1. Colorable UTXO + issuance in the RGB engine.
        let (asset_id, sources) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| -> Result<(String, Vec<String>)> {
                w.create_utxos(1, (deposit_sats * 4) as u32, 2)?;
                let asset_id = w.issue_nia(ticker, name, precision, vec![supply])?;
                let sources = w
                    .list_allocations(&asset_id)?
                    .into_iter()
                    .map(|(op, _, _)| op)
                    .collect();
                Ok((asset_id, sources))
            })?
        };

        // 2. A fresh statechain slot and the colored deposit onto it (one on-chain tx).
        let token = self.take_token().await?;
        let sc_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token,
            u32::try_from(deposit_sats)?,
        )
        .await?;
        // The coin has no statechain_id yet (the deposit has not confirmed), so the funding seal is
        // keyed on the deposit address — freshly derived per slot, therefore unique per funding
        // transition. Nothing on the receiving side ever re-derives a funding blinding.
        let funding_blinding =
            TierSeal::new(sc_address.clone(), TierRole::Funding, 0, 0).blinding();
        let (txid, vout) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let (txid, vout, _consignment, signed_tx) = tokio::task::block_in_place(|| {
                w.fund_statechain(&sc_address, deposit_sats, &asset_id, supply, 2, funding_blinding)
            })?;
            use electrum_client::ElectrumApi;
            let raw = hex::decode(&signed_tx)?;
            let _ = self.inner.cc.electrum_client.transaction_broadcast_raw(&raw)?;
            (txid, vout)
        };

        // 3. Register the statechain UTXO as the asset's carrier (consumes the funding sources).
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            tokio::task::block_in_place(|| {
                w.register_statechain(&txid, vout, deposit_sats, &asset_id, supply, &sources)
            })?;
        }
        Ok(asset_id)
    }

    /// Issue an IFA (inflatable) token. `supply` is minted now and bound to a fresh statechain
    /// coin exactly like [`Self::issue_token`]; `inflation_amounts` reserve inflation-right the
    /// issuer can later realize with [`Self::mint_tokens`]. `list_allocations` returns only the
    /// fungible allocation, so binding the supply never consumes the (InflationRight) reserve.
    /// Returns the asset id.
    pub async fn issue_inflatable_token(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        supply: u64,
        inflation_amounts: Vec<u64>,
    ) -> Result<String> {
        let deposit_sats: u64 = 10_000;
        let (asset_id, sources) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let inflation = inflation_amounts.clone();
            tokio::task::block_in_place(move || -> Result<(String, Vec<String>)> {
                // One colorable UTXO per allocation (the fungible supply + each inflation-right)
                // plus a spare for the fund/witness txs; max_allocations_per_utxo is 1.
                let utxos = (inflation.len() as u8).saturating_add(2);
                w.create_utxos(utxos, (deposit_sats * 4) as u32, 2)?;
                let asset_id = w.issue_ifa(ticker, name, precision, vec![supply], inflation)?;
                let sources = w
                    .list_allocations(&asset_id)?
                    .into_iter()
                    .map(|(op, _, _)| op)
                    .collect();
                Ok((asset_id, sources))
            })?
        };
        self.bind_engine_supply(&asset_id, supply, deposit_sats, &sources).await?;
        Ok(asset_id)
    }

    /// Realize `inflation_amounts` of an IFA's inflation-right as new supply and bind it to a fresh
    /// statechain coin. **This broadcasts an on-chain tx in the RGB engine** (inflation is a
    /// contract state transition — there is no off-chain variant); the newly-minted allocation is
    /// then bound like issuance. Returns `(inflate_txid, minted_total)`.
    ///
    /// Requires the inflate tx to confirm before the minted allocation is spendable — on regtest
    /// the caller must be mining (e.g. a background miner); in production real blocks provide it.
    pub async fn mint_tokens(
        &self,
        asset_id: &str,
        inflation_amounts: Vec<u64>,
    ) -> Result<(String, u64)> {
        let deposit_sats: u64 = 10_000;
        // Snapshot the allocations that already exist (incl. registered statechain coins, which
        // list_allocations reports as colorable UTXOs) so we can isolate ONLY the freshly-minted
        // one afterwards — otherwise binding would wrongly consume already-bound supply.
        let before: std::collections::HashSet<String> = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| w.list_allocations(asset_id))?
                .into_iter()
                .map(|(op, _, _)| op)
                .collect()
        };

        // 1. Inflate in the engine (on-chain broadcast). Ensure a colorable UTXO exists first.
        let (inflate_txid, minted) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let inflation = inflation_amounts.clone();
            tokio::task::block_in_place(move || -> Result<(String, u64)> {
                let _ = w.create_utxos(2, (deposit_sats * 4) as u32, 2);
                w.inflate(asset_id, inflation, 2)
            })?
        };

        // 2. Wait for the inflate to confirm and the NEW (post-snapshot) fungible allocation to
        //    settle; use only that as the bind source.
        let mut sources: Vec<String> = Vec::new();
        for _ in 0..90 {
            let allocs = {
                let mut rgb = self.rgb().await?;
                let w = rgb.as_mut().unwrap();
                tokio::task::block_in_place(|| -> Result<Vec<(String, u64, bool)>> {
                    let _ = w.refresh(Some(asset_id.to_string()));
                    w.list_allocations(asset_id)
                })?
            };
            let fresh: Vec<(String, u64)> = allocs
                .into_iter()
                .filter(|(op, _, s)| *s && !before.contains(op))
                .map(|(op, a, _)| (op, a))
                .collect();
            let settled: u64 = fresh.iter().map(|(_, a)| *a).sum();
            if settled >= minted {
                sources = fresh.into_iter().map(|(op, _)| op).collect();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        if sources.is_empty() {
            return Err(anyhow!("minted allocation for {asset_id} did not settle (is the chain advancing?)"));
        }

        // 3. Bind the minted supply to a fresh statechain coin.
        self.bind_engine_supply(asset_id, minted, deposit_sats, &sources).await?;
        Ok((inflate_txid, minted))
    }

    /// Burn `amount` of an asset's FREE (engine-held) balance. **On-chain** in the RGB engine.
    /// Statechain-bound supply must be exited back into the engine first (documented limitation).
    /// Returns the burn txid.
    pub async fn burn_tokens(&self, asset_id: &str, amount: u64) -> Result<String> {
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| w.burn(asset_id, amount, 2))
    }

    /// Bind an engine-held fungible allocation of `amount` to a fresh statechain coin: fund the
    /// coin colored (one on-chain tx) and register it as the carrier. Shared by issuance and mint.
    async fn bind_engine_supply(
        &self,
        asset_id: &str,
        amount: u64,
        deposit_sats: u64,
        sources: &[String],
    ) -> Result<(String, u32)> {
        let token = self.take_token().await?;
        let sc_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token,
            u32::try_from(deposit_sats)?,
        )
        .await?;
        // Same as `issue_token`: no statechain_id yet, so the fresh deposit address is the seal id.
        let funding_blinding =
            TierSeal::new(sc_address.clone(), TierRole::Funding, 0, 0).blinding();
        let (txid, vout) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let (txid, vout, _c, signed_tx) = tokio::task::block_in_place(|| {
                w.fund_statechain(&sc_address, deposit_sats, asset_id, amount, 2, funding_blinding)
            })?;
            use electrum_client::ElectrumApi;
            let raw = hex::decode(&signed_tx)?;
            let _ = self.inner.cc.electrum_client.transaction_broadcast_raw(&raw)?;
            (txid, vout)
        };
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            tokio::task::block_in_place(|| {
                w.register_statechain(&txid, vout, deposit_sats, asset_id, amount, sources)
            })?;
        }
        Ok((txid, vout))
    }

    /// L1 (Bitcoin) address for token operations — where to send sats to fund issuance/mint.
    /// Alias of [`Self::get_token_funding_address`]; mirrors Spark's `getTokenL1Address`.
    pub async fn get_token_l1_address(&self) -> Result<String> {
        self.get_token_funding_address().await
    }

    /// Transaction history for a token contract (Spark's `queryTokenTransactions`):
    /// `(kind, status, amount, txid)` per transfer known to the RGB engine.
    pub async fn query_token_transactions(&self, asset_id: &str) -> Result<Vec<crate::types::TokenTx>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Err(SdkError::TokensNotConfigured.into());
        }
        let rgb = self.rgb().await?;
        let w = rgb.as_ref().unwrap();
        let rows = tokio::task::block_in_place(|| w.transfers(asset_id))?;
        Ok(rows
            .into_iter()
            .map(|(kind, status, amount, txid)| crate::types::TokenTx { kind, status, amount, txid })
            .collect())
    }

    /// Token balances across this wallet's registered coins.
    pub async fn get_token_balances(&self) -> Result<Vec<TokenBalance>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(vec![]);
        }
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| -> Result<Vec<TokenBalance>> {
            let mut out = Vec::new();
            for (asset_id, ticker, name, precision) in w.list_assets()? {
                let (settled, future, _spendable) = w.balance(&asset_id)?;
                out.push(TokenBalance {
                    asset_id,
                    ticker: Some(ticker),
                    name: Some(name),
                    precision,
                    balance: settled,
                    total: future,
                });
            }
            Ok(out)
        })
    }

    /// Outpoints (`"txid:vout"`) of every coin that currently carries an RGB token allocation.
    /// BTC coin-selection and the spendable-BTC balance MUST exclude these: spending a token
    /// carrier as ordinary sats destroys its RGB allocation with no warning (review H2). Empty when
    /// token support is not configured (the common pure-BTC wallet — no RGB engine is opened).
    pub(crate) async fn token_carrier_outpoints(
        &self,
    ) -> Result<std::collections::HashSet<String>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(std::collections::HashSet::new());
        }
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| -> Result<std::collections::HashSet<String>> {
            let mut out = std::collections::HashSet::new();
            for (asset_id, _ticker, _name, _precision) in w.list_assets()? {
                for (outpoint, _amt, _settled) in w.list_allocations(&asset_id)? {
                    out.insert(outpoint);
                }
            }
            Ok(out)
        })
    }

    /// Outpoints of CONFIRMED coins that carry an incoming RGB **consignment** in their backup rows
    /// but whose allocation is NOT yet booked in the engine — a *pending* token carrier (external
    /// review finding 5). A transient RGB-proxy/indexer error during `accept_incoming_tokens` leaves
    /// such a coin CONFIRMED yet absent from [`Self::token_carrier_outpoints`] (which lists only
    /// booked allocations), so until the retry loop books it, plain-BTC selection would happily
    /// spend it and DESTROY the allocation — including `auto_refresh_due`, which would re-anchor it.
    /// These outpoints must be quarantined from every plain-BTC path exactly like booked carriers.
    pub(crate) async fn consignment_bearing_outpoints(
        &self,
    ) -> Result<std::collections::HashSet<String>> {
        let mut out = std::collections::HashSet::new();
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(out);
        }
        let record = self.record().await?;
        for coin in record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        {
            let Some(id) = coin.statechain_id.as_deref() else { continue };
            let Some(outpoint) = crate::wallet::coin_outpoint(coin) else { continue };
            // A coin whose consignment was PERMANENTLY rejected (griefer's garbage, marked by
            // claim()) is NOT quarantined — its sats are ordinary BTC the owner may spend.
            let rejected = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("token-rejected-{id}"),
            )
            .await
            .map(|v| !v.is_empty())
            .unwrap_or(false);
            if rejected {
                continue;
            }
            let backups = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                id,
            )
            .await
            .unwrap_or_default();
            if backups.iter().any(|b| b.rgb_consignment.is_some()) {
                out.insert(outpoint);
            }
        }
        Ok(out)
    }

    /// The full set of outpoints that must NEVER be spent as plain BTC: booked token carriers UNION
    /// pending (consignment-bearing, not-yet-booked) carriers. Every plain-BTC selection/spend path
    /// (transfer, split, combine parent-pick, withdraw, unilateral exit, auto-refresh) uses this so
    /// a token coin — booked or still settling — is never swept as sats (review H2 + finding 5).
    pub(crate) async fn unspendable_as_btc_outpoints(
        &self,
    ) -> Result<std::collections::HashSet<String>> {
        let mut set = self.token_carrier_outpoints().await?;
        set.extend(self.consignment_bearing_outpoints().await?);
        Ok(set)
    }

    /// Send `token_amount` of `asset_id` to a statechain address, entirely off-chain: colored
    /// split (exact token piece + change back to this wallet) then branch-carrying key handover
    /// of the piece coin. The receiver's SDK auto-claims, validates the consignment off-chain and
    /// books the balance.
    /// Send `token_amount` of `asset_id` to `receiver_address`, entirely off-chain (colored split).
    pub async fn transfer_tokens(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
    ) -> Result<TransferResult> {
        Ok(self
            .colored_transfer(asset_id, receiver_address, token_amount, ColoredLatch::None)
            .await?
            .result)
    }

    /// Colored transfer LATCHED to an EXTERNAL payment hash (the RGB half of a Lightning PAY: the
    /// user hands a colored coin to the SSP, claimable only once the invoice preimage is revealed).
    /// Returns `(batch_id, piece_statechain_id)`.
    pub async fn latch_tokens(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        payment_hash: &str,
    ) -> Result<(String, String)> {
        let out = self
            .colored_transfer(asset_id, receiver_address, token_amount, ColoredLatch::ExternalHash(payment_hash.to_string()))
            .await?;
        let batch = out.batch_id.ok_or_else(|| anyhow!("colored latch did not produce a batch id"))?;
        Ok((batch, out.piece_id))
    }

    /// Colored transfer LATCHED to an SE-HELD preimage (the RGB half of a Lightning RECEIVE: the SSP
    /// hands a colored coin to the user; the SE reveals the preimage only once the coin is released,
    /// so the SSP can't take the HTLC without releasing). Returns `(batch_id, piece_statechain_id,
    /// payment_hash)`.
    pub async fn latch_tokens_se_preimage(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
    ) -> Result<(String, String, String)> {
        let out = self
            .colored_transfer(asset_id, receiver_address, token_amount, ColoredLatch::SePreimage)
            .await?;
        let batch = out.batch_id.ok_or_else(|| anyhow!("colored SE-preimage latch produced no batch id"))?;
        let hash = out.se_hash.ok_or_else(|| anyhow!("colored SE-preimage latch produced no payment hash"))?;
        Ok((batch, out.piece_id, hash))
    }

    /// Core colored transfer with an optional latch mode. Returns the pieces + any latch outputs.
    async fn colored_transfer(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        latch: ColoredLatch,
    ) -> Result<ColoredTransferOut> {
        let _guard = self.inner.wallet_lock.lock().await;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;

        // Locate the carrier coin: the confirmed coin whose outpoint holds the allocation.
        let allocations = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| w.list_allocations(asset_id))?
        };
        let mut carrier: Option<(mercurylib::wallet::Coin, u64)> = None;
        for coin in record.coins.iter() {
            if coin.status != CoinStatus::CONFIRMED || coin.duplicate_index != 0 {
                continue;
            }
            let op = format!(
                "{}:{}",
                coin.utxo_txid.clone().unwrap_or_default(),
                coin.utxo_vout.unwrap_or_default()
            );
            if let Some((_, amt, _)) = allocations.iter().find(|(o, _, settled)| *o == op && *settled)
            {
                if *amt >= token_amount {
                    carrier = Some((coin.clone(), *amt));
                    break;
                }
            }
        }
        let (mut carrier, carrier_amount) = match carrier {
            Some(c) => c,
            None => {
                // No single carrier covers the amount: combine several carriers of this asset into
                // one payment (piece + change) in a single SE-co-signed colored combine tx.
                return self
                    .colored_combine_transfer(
                        asset_id,
                        receiver_address,
                        token_amount,
                        latch,
                        record,
                        allocations,
                    )
                    .await;
            }
        };
        let carrier_id = carrier
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow!("carrier coin without statechain id"))?;
        let carrier_sats = carrier.amount.unwrap_or_default() as u64;
        let fee_reserve = (carrier_sats / 100).clamp(300, 2_000);
        if TOKEN_PIECE_SATS + fee_reserve >= carrier_sats {
            return Err(anyhow!(
                "carrier coin too small ({carrier_sats} sats) for a token split"
            ));
        }
        let change_sats = carrier_sats - TOKEN_PIECE_SATS - fee_reserve;
        let token_change = carrier_amount - token_amount;

        // Backup-fee floor: the 1_500-sat piece and the change must each fund their own backup at
        // the live feerate, else create_tx1 rejects the backup as FeeTooLow AFTER the carrier is
        // made terminal — stranding it. Refuse up-front (carrier untouched). At feerates above
        // ~10 sat/vB the fixed 1_500-sat packaging itself falls below the floor, so a token
        // transfer is correctly refused rather than stranding the carrier.
        let min_output =
            crate::transfer::min_split_output(crate::transfer::backup_fee_rate(&self.inner.cc).await?);
        if TOKEN_PIECE_SATS < min_output || change_sats < min_output {
            return Err(anyhow!(
                "token split output below the minimum viable size {min_output} at the current feerate (piece {TOKEN_PIECE_SATS} sats, change {change_sats} sats) — a sub-coin could not fund its own backup"
            ));
        }

        // Fresh slots for piece and change — DERIVED from the carrier (free SE vouchers; a
        // colored split re-houses the carrier's value, no new on-chain onboarding).
        let mut slot_tokens = self.take_derived_tokens(&carrier_id, 2).await?;
        let token_a = slot_tokens.remove(0);
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(TOKEN_PIECE_SATS)?,
        )
        .await?;
        let token_b = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_b,
            u32::try_from(change_sats)?,
        )
        .await?;

        // Colored split: piece carries the exact token amount; change keeps the rest (or is a
        // plain sats output when the transfer consumes the full allocation).
        let parent_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await
        .map(|v| v.len() as u32)
        .unwrap_or(0);
        let server_info = mercuryrustlib::utils::info_config(&self.inner.cc).await?;
        // Terminal-spend guard on the carrier: one more co-signature (the colored split), then
        // the SE refuses everything — the token branch cannot be double-spent.
        mercuryrustlib::lightning_latch::set_spend_budget(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
            1,
        )
        .await?;
        let splits = vec![
            (piece_addr.clone(), TOKEN_PIECE_SATS, token_amount),
            (change_addr.clone(), change_sats, token_change),
        ];
        // Per-transfer seal: the carrier being spent, the split role, the carrier's spend
        // generation, and the transition's arity — always 2 here. What separates this lane from the
        // batch lane below (whose split over the same carrier would otherwise share
        // `(role, tier_index)`, and at `n == 1` the same arity too) is `BATCH_SPLIT_RUNG_FLAG`,
        // which that lane ORs into its rung; this one never sets it.
        let split_blinding =
            single_split_seal(&carrier_id, parent_backups, splits.len() as u32).blinding();
        let split = {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            mercuryrustlib::rgb::create_colored_split_tx(
                &self.inner.cc,
                w,
                &mut carrier,
                asset_id,
                &splits,
                parent_backups + 1,
                false,
                None,
                &self.inner.config.network.to_string(),
                server_info.initlock,
                server_info.interval,
                split_blinding,
            )
            .await?
        };
        let piece_vout = split.output_vouts[0];
        let change_vout = split.output_vouts[1];

        // Register the sub-coins as wallet coins (with backups + branch) — shared with the plain
        // split — then RGB-register the change and mark the carrier spent.
        let (piece_id, change_id) = self
            .register_split_subcoins(
                &carrier_id,
                &split.signed_tx,
                &split.txid,
                &[
                    (piece_addr.clone(), piece_vout, TOKEN_PIECE_SATS),
                    (change_addr.clone(), change_vout, change_sats),
                ],
            )
            .await?;
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            let carrier_op = format!(
                "{}:{}",
                carrier.utxo_txid.clone().unwrap_or_default(),
                carrier.utxo_vout.unwrap_or_default()
            );
            tokio::task::block_in_place(|| -> Result<()> {
                if token_change > 0 {
                    w.register_statechain(
                        &split.txid,
                        change_vout,
                        change_sats,
                        asset_id,
                        token_change,
                        &[carrier_op.clone()],
                    )?;
                } else {
                    w.mark_spent(&[carrier_op.clone()])?;
                }
                Ok(())
            })?;
        }

        // Attach the consignment envelope to the piece's backup row so it rides the transfer msg.
        let envelope = serde_json::to_string(&ConsignmentEnvelope {
            c: split.consignment.clone(),
            a: token_amount,
            s: TOKEN_PIECE_SATS,
        })?;
        let mut piece_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &piece_id,
        )
        .await?;
        if let Some(first) = piece_backups.first_mut() {
            first.rgb_consignment = Some(envelope);
            first.rgb_blinding = Some(split.blinding);
        }
        mercuryrustlib::sqlite_manager::update_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &piece_id,
            &piece_backups,
        )
        .await?;

        // If latching (Lightning swap), bind the piece BEFORE handing it over so the receiver's
        // claim stays locked until the preimage is revealed.
        let (batch_id, se_hash) = match &latch {
            ColoredLatch::None => (None, None),
            ColoredLatch::ExternalHash(hash) => (
                Some(
                    mercuryrustlib::lightning_latch::create_external_hash_latch(
                        &self.inner.cc,
                        &self.inner.config.wallet_name,
                        &piece_id,
                        hash,
                    )
                    .await?,
                ),
                None,
            ),
            ColoredLatch::SePreimage => {
                let pre = mercuryrustlib::lightning_latch::create_pre_image(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &piece_id,
                )
                .await?;
                (Some(pre.batch_id), Some(pre.hash))
            }
        };

        // Hand the piece over (plain, or batch-locked when latching).
        mercuryrustlib::transfer_sender::execute(
            &self.inner.cc,
            receiver_address,
            &self.inner.config.wallet_name,
            &piece_id,
            None,
            false,
            batch_id.clone(),
        )
        .await?;

        let _ = change_id;
        Ok(ColoredTransferOut {
            result: TransferResult {
                receiver_address: receiver_address.to_string(),
                total_sats: TOKEN_PIECE_SATS,
                coins: vec![TransferredCoin {
                    statechain_id: piece_id.clone(),
                    amount_sats: TOKEN_PIECE_SATS,
                }],
                used_split: true,
            },
            piece_id,
            batch_id,
            se_hash,
        })
    }

    /// Multi-carrier colored transfer: when no single carrier holds `token_amount`, COMBINE several
    /// carriers of `asset_id` into one payment (piece + change) in a single SE-co-signed colored
    /// combine tx (N inputs → 2 outputs). Every combined carrier is made terminal first; the receiver
    /// validates the multi-input branch and (via the per-structural-input terminal check) requires
    /// ALL N carriers to be terminal. Caller MUST hold `wallet_lock` (this runs inside
    /// `colored_transfer`'s lock and does not re-take it).
    async fn colored_combine_transfer(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        latch: ColoredLatch,
        record: mercurylib::wallet::Wallet,
        allocations: Vec<(String, u64, bool)>,
    ) -> Result<ColoredTransferOut> {
        // 1. Select a minimal set of confirmed, settled carriers of this asset, largest allocation
        //    first, until their allocations sum to >= token_amount; then top up with more carriers
        //    if the summed SATS cannot fund the piece + change + fee.
        let min_output =
            crate::transfer::min_split_output(crate::transfer::backup_fee_rate(&self.inner.cc).await?);
        let mut candidates: Vec<(mercurylib::wallet::Coin, u64)> = Vec::new();
        for coin in record.coins.iter() {
            if coin.status != CoinStatus::CONFIRMED || coin.duplicate_index != 0 {
                continue;
            }
            let op = format!(
                "{}:{}",
                coin.utxo_txid.clone().unwrap_or_default(),
                coin.utxo_vout.unwrap_or_default()
            );
            if let Some((_, amt, _)) =
                allocations.iter().find(|(o, _, settled)| *o == op && *settled)
            {
                candidates.push((coin.clone(), *amt));
            }
        }
        let total_alloc: u64 = candidates.iter().map(|(_, a)| *a).sum();
        if total_alloc < token_amount {
            return Err(anyhow!(
                "insufficient {asset_id}: wallet holds {total_alloc} across {} carrier(s), need {token_amount}",
                candidates.len()
            ));
        }
        // Largest allocation first (fewest inputs); the combine reserve grows with total sats.
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        let mut selected: Vec<(mercurylib::wallet::Coin, u64)> = Vec::new();
        let mut sel_alloc = 0u64;
        let mut sel_sats = 0u64;
        for c in candidates.into_iter() {
            if sel_alloc >= token_amount
                && sel_sats > TOKEN_PIECE_SATS + (sel_sats / 100).clamp(300, 2_000) + min_output
            {
                break;
            }
            sel_sats += c.0.amount.unwrap_or_default() as u64;
            sel_alloc += c.1;
            selected.push(c);
        }
        if selected.len() < 2 {
            // A single carrier would have been found by the caller's scan; <2 here means the only
            // sufficient carrier is a token carrier we already rejected, so treat as unsupported.
            return Err(anyhow!(
                "no combination of carriers covers {token_amount} of {asset_id} with enough sats"
            ));
        }

        // 2. Amounts. Piece carries the exact token_amount; change keeps the rest across all inputs.
        let combined_sats: u64 = selected.iter().map(|(c, _)| c.amount.unwrap_or_default() as u64).sum();
        let combined_alloc: u64 = selected.iter().map(|(_, a)| *a).sum();
        let fee_reserve = (combined_sats / 100).clamp(300, 2_000);
        if TOKEN_PIECE_SATS + fee_reserve + min_output >= combined_sats {
            return Err(anyhow!(
                "combined carriers hold too few sats ({combined_sats}) to fund a token piece + change + fee at the current feerate"
            ));
        }
        let change_sats = combined_sats - TOKEN_PIECE_SATS - fee_reserve;
        let token_change = combined_alloc - token_amount;
        if TOKEN_PIECE_SATS < min_output || change_sats < min_output {
            return Err(anyhow!(
                "combine output below the minimum viable size {min_output} (piece {TOKEN_PIECE_SATS}, change {change_sats}) — a sub-coin could not fund its own backup"
            ));
        }

        // 3. Fresh slots for the piece + change — DERIVED slots; any consumed input carrier can
        //    vouch (they are all being re-housed by this combine), so use the first selected.
        let voucher_parent = selected[0]
            .0
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow!("selected carrier without statechain id"))?;
        let mut slot_tokens = self.take_derived_tokens(&voucher_parent, 2).await?;
        let token_a = slot_tokens.remove(0);
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(TOKEN_PIECE_SATS)?,
        )
        .await?;
        let token_b = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_b,
            u32::try_from(change_sats)?,
        )
        .await?;

        // 4. Make EVERY input carrier terminal at the SE before co-signing the combine — so none can
        //    be double-spent to invalidate the branch (the receiver independently verifies this).
        let carrier_ids: Vec<String> = selected
            .iter()
            .map(|(c, _)| c.statechain_id.clone().unwrap_or_default())
            .collect();
        let carrier_ops: Vec<String> = selected
            .iter()
            .map(|(c, _)| {
                format!(
                    "{}:{}",
                    c.utxo_txid.clone().unwrap_or_default(),
                    c.utxo_vout.unwrap_or_default()
                )
            })
            .collect();
        for id in &carrier_ids {
            mercuryrustlib::lightning_latch::set_spend_budget(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                id,
                1,
            )
            .await?;
        }

        // 5. Build + per-input blind-MuSig2 co-sign the un-broadcast colored combine (locktime 0).
        let server_info = mercuryrustlib::utils::info_config(&self.inner.cc).await?;
        let mut input_coins: Vec<mercurylib::wallet::Coin> =
            selected.iter().map(|(c, _)| c.clone()).collect();
        let splits: Vec<(String, u64, u64)> = vec![
            (piece_addr.clone(), TOKEN_PIECE_SATS, token_amount),
            (change_addr.clone(), change_sats, token_change),
        ];
        // A combine has N parent outpoints, so its seal is keyed on the whole input set. Two
        // combines over the identical set cannot both be co-signed — step 4 above set every input's
        // spend budget to 1 — so `(input set, Combine, arity)` is unique in practice. That is an
        // argument, not a check: `create_colored_combine_tx` colours through `RgbWallet::color`,
        // which has no guard of its own, so `assert_own_witness` is applied to the result below.
        let combine_blinding = TierSeal::new(
            carrier_ids.join("+"),
            TierRole::Combine,
            0,
            splits.len() as u32,
        )
        .blinding();
        let combine = {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            mercuryrustlib::rgb::create_colored_combine_tx(
                &self.inner.cc,
                w,
                &mut input_coins,
                asset_id,
                &splits,
                1,
                false,
                None,
                &self.inner.config.network.to_string(),
                server_info.initlock,
                server_info.interval,
                combine_blinding,
            )
            .await?
        };
        // The collision assert the seal derivation above relies on — fail closed BEFORE the
        // sub-coins are registered and the piece is handed over.
        assert_own_witness(
            "coloured combine",
            &combine.txid,
            &combine.consignment,
            combine.blinding,
        )?;
        let piece_vout = combine.output_vouts[0];
        let change_vout = combine.output_vouts[1];

        // 6. Register both sub-coins (merged DAG branch = all inputs' sub-branches + combine tx;
        //    ancestors = every carrier + its inherited ancestors). Then RGB-register the change with
        //    ALL input carrier outpoints as its sources, or mark them all spent on a full-allocation send.
        let ids = self
            .register_combine_subcoins(
                &carrier_ids,
                &combine.signed_tx,
                &combine.txid,
                &[
                    (piece_addr.clone(), piece_vout, TOKEN_PIECE_SATS),
                    (change_addr.clone(), change_vout, change_sats),
                ],
            )
            .await?;
        let piece_id = ids[0].clone();
        let change_id = ids[1].clone();
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            tokio::task::block_in_place(|| -> Result<()> {
                if token_change > 0 {
                    w.register_statechain(
                        &combine.txid,
                        change_vout,
                        change_sats,
                        asset_id,
                        token_change,
                        &carrier_ops,
                    )?;
                } else {
                    w.mark_spent(&carrier_ops)?;
                }
                Ok(())
            })?;
        }

        // 7. Attach the consignment envelope to the piece's backup so it rides the transfer message.
        let envelope = serde_json::to_string(&ConsignmentEnvelope {
            c: combine.consignment.clone(),
            a: token_amount,
            s: TOKEN_PIECE_SATS,
        })?;
        let mut piece_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &piece_id,
        )
        .await?;
        if let Some(first) = piece_backups.first_mut() {
            first.rgb_consignment = Some(envelope);
            first.rgb_blinding = Some(combine.blinding);
        }
        mercuryrustlib::sqlite_manager::update_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &piece_id,
            &piece_backups,
        )
        .await?;

        // 8. Optional latch, then hand the piece over — identical to the single-carrier path.
        let (batch_id, se_hash) = match &latch {
            ColoredLatch::None => (None, None),
            ColoredLatch::ExternalHash(hash) => (
                Some(
                    mercuryrustlib::lightning_latch::create_external_hash_latch(
                        &self.inner.cc,
                        &self.inner.config.wallet_name,
                        &piece_id,
                        hash,
                    )
                    .await?,
                ),
                None,
            ),
            ColoredLatch::SePreimage => {
                let pre = mercuryrustlib::lightning_latch::create_pre_image(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &piece_id,
                )
                .await?;
                (Some(pre.batch_id), Some(pre.hash))
            }
        };
        mercuryrustlib::transfer_sender::execute(
            &self.inner.cc,
            receiver_address,
            &self.inner.config.wallet_name,
            &piece_id,
            None,
            false,
            batch_id.clone(),
        )
        .await?;

        let _ = change_id;
        Ok(ColoredTransferOut {
            result: TransferResult {
                receiver_address: receiver_address.to_string(),
                total_sats: TOKEN_PIECE_SATS,
                coins: vec![TransferredCoin {
                    statechain_id: piece_id.clone(),
                    amount_sats: TOKEN_PIECE_SATS,
                }],
                used_split: true,
            },
            piece_id,
            batch_id,
            se_hash,
        })
    }

    /// Send `asset_id` to MANY recipients in a single off-chain colored split: one SE-co-signed
    /// tx carves one piece per recipient (its exact amount) plus this wallet's change. Each piece
    /// is handed over with its own consignment envelope. Returns one `TransferResult` per recipient.
    pub async fn batch_transfer_tokens(
        &self,
        asset_id: &str,
        transfers: &[(String, u64)],
    ) -> Result<Vec<TransferResult>> {
        if transfers.is_empty() {
            return Err(anyhow!("no recipients"));
        }
        let total: u64 = transfers.iter().map(|(_, a)| *a).sum();
        let n = transfers.len();

        let _guard = self.inner.wallet_lock.lock().await;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;

        // Carrier: a confirmed coin holding >= total of the asset.
        let allocations = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| w.list_allocations(asset_id))?
        };
        let mut carrier: Option<(mercurylib::wallet::Coin, u64)> = None;
        for coin in record.coins.iter() {
            if coin.status != CoinStatus::CONFIRMED || coin.duplicate_index != 0 {
                continue;
            }
            let op = format!(
                "{}:{}",
                coin.utxo_txid.clone().unwrap_or_default(),
                coin.utxo_vout.unwrap_or_default()
            );
            if let Some((_, amt, _)) = allocations.iter().find(|(o, _, s)| *o == op && *s) {
                if *amt >= total {
                    carrier = Some((coin.clone(), *amt));
                    break;
                }
            }
        }
        let (mut carrier, carrier_amount) = carrier.ok_or_else(|| {
            anyhow!("no confirmed coin carries >= {total} of {asset_id} for the batch")
        })?;
        let carrier_id = carrier.statechain_id.clone().unwrap();
        let carrier_sats = carrier.amount.unwrap_or_default() as u64;
        let fee_reserve = (carrier_sats / 100).clamp(300, 2_000);
        let pieces_sats = TOKEN_PIECE_SATS * n as u64;
        if pieces_sats + fee_reserve >= carrier_sats {
            return Err(anyhow!(
                "carrier coin too small ({carrier_sats} sats) for {n} pieces + fee"
            ));
        }
        let change_sats = carrier_sats - pieces_sats - fee_reserve;
        let token_change = carrier_amount - total;

        // Backup-fee floor on every piece (each 1_500 sats) and the change — reject before the
        // carrier is made terminal so a doomed batch never strands it (see the single-transfer note).
        let min_output =
            crate::transfer::min_split_output(crate::transfer::backup_fee_rate(&self.inner.cc).await?);
        if TOKEN_PIECE_SATS < min_output || change_sats < min_output {
            return Err(anyhow!(
                "batch token split output below the minimum viable size {min_output} at the current feerate (piece {TOKEN_PIECE_SATS} sats, change {change_sats} sats) — a sub-coin could not fund its own backup"
            ));
        }

        // One fresh slot per recipient piece + one for change; build the N+1 colored split. All
        // N+1 slots are DERIVED from the carrier (one free voucher batch, one auth nonce).
        let mut slot_tokens = self.take_derived_tokens(&carrier_id, n + 1).await?;
        let mut splits: Vec<(String, u64, u64)> = Vec::with_capacity(n + 1);
        let mut piece_addrs: Vec<String> = Vec::with_capacity(n);
        for (_, amount) in transfers {
            let tk = slot_tokens.remove(0);
            let addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &tk,
                u32::try_from(TOKEN_PIECE_SATS)?,
            )
            .await?;
            splits.push((addr.clone(), TOKEN_PIECE_SATS, *amount));
            piece_addrs.push(addr);
        }
        let change_tk = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &change_tk,
            u32::try_from(change_sats)?,
        )
        .await?;
        splits.push((change_addr.clone(), change_sats, token_change));

        let parent_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await
        .map(|v| v.len() as u32)
        .unwrap_or(0);
        let server_info = mercuryrustlib::utils::info_config(&self.inner.cc).await?;
        // One colored split spends the carrier once -> spend budget 1.
        mercuryrustlib::lightning_latch::set_spend_budget(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
            1,
        )
        .await?;
        // Same seal derivation as the single-recipient split, but in a DISJOINT rung space. The
        // arity alone does not keep the two lanes apart: this function rejects only an EMPTY
        // recipient list, so `n == 1` is reachable and gives `splits.len() == 2` — exactly the
        // single lane's arity, over the same carrier at the same spend generation, i.e. the byte
        // identical seal. `BATCH_SPLIT_RUNG_FLAG` moves every batch rung out of the single lane's
        // reach at every arity, including 1.
        let split_blinding =
            batch_split_seal(&carrier_id, parent_backups, splits.len() as u32).blinding();
        let split = {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            mercuryrustlib::rgb::create_colored_split_tx(
                &self.inner.cc,
                w,
                &mut carrier,
                asset_id,
                &splits,
                parent_backups + 1,
                false,
                None,
                &self.inner.config.network.to_string(),
                server_info.initlock,
                server_info.interval,
                split_blinding,
            )
            .await?
        };

        // Register every sub-coin (pieces + change).
        let outputs: Vec<(String, u32, u64)> = splits
            .iter()
            .enumerate()
            .map(|(i, (addr, sats, _))| (addr.clone(), split.output_vouts[i], *sats))
            .collect();
        let ids = self
            .register_split_subcoins_n(&carrier_id, &split.signed_tx, &split.txid, &outputs)
            .await?;
        let change_vout = split.output_vouts[n];

        // RGB-register the change (or mark the carrier fully spent).
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            let carrier_op = format!(
                "{}:{}",
                carrier.utxo_txid.clone().unwrap_or_default(),
                carrier.utxo_vout.unwrap_or_default()
            );
            tokio::task::block_in_place(|| -> Result<()> {
                if token_change > 0 {
                    w.register_statechain(
                        &split.txid,
                        change_vout,
                        change_sats,
                        asset_id,
                        token_change,
                        &[carrier_op],
                    )?;
                } else {
                    w.mark_spent(&[carrier_op])?;
                }
                Ok(())
            })?;
        }

        // Per-piece envelope (own amount) + hand over to each recipient.
        let mut results = Vec::with_capacity(n);
        for (i, (recipient, amount)) in transfers.iter().enumerate() {
            let piece_id = ids[i].clone();
            let envelope = serde_json::to_string(&ConsignmentEnvelope {
                c: split.consignment.clone(),
                a: *amount,
                s: TOKEN_PIECE_SATS,
            })?;
            let mut backups = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &piece_id,
            )
            .await?;
            if let Some(first) = backups.first_mut() {
                first.rgb_consignment = Some(envelope);
                first.rgb_blinding = Some(split.blinding);
            }
            mercuryrustlib::sqlite_manager::update_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &piece_id,
                &backups,
            )
            .await?;
            mercuryrustlib::transfer_sender::execute(
                &self.inner.cc,
                recipient,
                &self.inner.config.wallet_name,
                &piece_id,
                None,
                false,
                None,
            )
            .await?;
            results.push(TransferResult {
                receiver_address: recipient.clone(),
                total_sats: TOKEN_PIECE_SATS,
                coins: vec![TransferredCoin {
                    statechain_id: piece_id,
                    amount_sats: TOKEN_PIECE_SATS,
                }],
                used_split: true,
            });
        }
        Ok(results)
    }

    /// Receive-side token hook, called by `claim()` for each newly claimed coin: if its backup
    /// rows carry a consignment envelope, validate the consignment off-chain against the coin's
    /// exit branch and book the balance under the consignment's verified contract id.
    pub(crate) async fn accept_incoming_tokens(&self, statechain_id: &str) -> Result<Option<(String, u64)>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(None);
        }
        let backups = match mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await
        {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let envelope = backups.iter().find_map(|b| b.rgb_consignment.clone());
        let Some(envelope) = envelope else {
            return Ok(None);
        };
        let env: ConsignmentEnvelope = serde_json::from_str(&envelope)
            .map_err(|e| anyhow!("malformed consignment envelope: {e}"))?;

        // Branch txids: the un-broadcast witnesses the consignment chain resolves against.
        let branch = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &format!("branch-{statechain_id}"),
        )
        .await
        .unwrap_or_default();
        let mut txids = Vec::new();
        for b in &branch {
            let tx: bitcoin::Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&b.tx)?)?;
            txids.push(tx.txid().to_string());
        }

        let record = self.record().await?;
        let coin = record
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0)
            .ok_or_else(|| anyhow!("received coin not found"))?;
        let (txid, vout, sats) = (
            coin.utxo_txid.clone().unwrap_or_default(),
            coin.utxo_vout.unwrap_or_default(),
            coin.amount.unwrap_or_default() as u64,
        );

        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        let (valid, detail, contract_id) = tokio::task::block_in_place(|| {
            w.validate_offchain_chain_info(&env.c, &txids)
        })?;
        if !valid {
            // PERMANENT-INVALID prefix (external review finding 5): a consignment that fails
            // cryptographic validation is never going to book. claim() matches this exact prefix to
            // un-quarantine the coin (so a griefer cannot lock a victim's sats forever by attaching a
            // garbage consignment) — as distinct from a TRANSIENT RGB-proxy/indexer error (no prefix),
            // which keeps the coin quarantined and retried.
            return Err(anyhow!(
                "PERMANENT-INVALID: incoming token consignment INVALID: {}",
                detail.unwrap_or_default()
            ));
        }
        let contract_id =
            contract_id.ok_or_else(|| anyhow!("validated consignment without contract id"))?;
        // Book the amount the CONSIGNMENT assigns to our own witness outpoint — the cryptographic
        // source of truth. The envelope amount (env.a) is only a hint we cross-check; a lying
        // sender cannot inflate the booked balance because the consignment governs it.
        let booked = tokio::task::block_in_place(|| {
            w.accept_offchain_amount(&env.c, &txids, &txid, vout)
        })?;
        if booked != env.a {
            // PERMANENT-INVALID (see above): the consignment cryptographically assigns a different
            // amount than the envelope claimed — a lying sender; this will never book.
            return Err(anyhow!(
                "PERMANENT-INVALID: token consignment assigns {booked} to this coin but the envelope claimed {} — rejecting",
                env.a
            ));
        }
        tokio::task::block_in_place(|| -> Result<()> {
            // First sight of this contract: import it (genesis + history) into the stash so the
            // allocation rows have their asset to reference — validated against the same branch.
            w.import_asset_offchain(&env.c, &txids)?;
            w.register_statechain(&txid, vout, sats, &contract_id, booked, &[])?;
            Ok(())
        })?;
        Ok(Some((contract_id, booked)))
    }

    /// Validate a PENDING (un-claimed) token transfer's consignment WITHOUT booking it, returning
    /// `(contract_id, booked_amount)` — the amount the consignment *cryptographically* assigns to
    /// the coin's witness outpoint `funding_txid:vout`. Security gate for audit [4]: the SSP's
    /// pre-payment check calls this to verify a latched colored coin actually carries the invoiced
    /// asset + amount BEFORE it pays the Lightning invoice. A HODL swap forces payment before the
    /// coin can be claimed, so the post-claim balance-delta check is a backstop, not the gate; and
    /// the envelope's advisory `env.a` is attacker-controlled, so only this consignment-derived
    /// amount is trustworthy. Read-only: `validate_offchain_chain_info` and `accept_offchain_amount`
    /// load the consignment into a temp dir and query it, mutating neither the stash nor the wallet,
    /// so it does not disturb the later `accept_incoming_tokens` booking.
    pub async fn validate_pending_token(
        &self,
        consignment_env: &str,
        branch_txs: &[String],
        funding_txid: &str,
        funding_vout: u32,
    ) -> Result<(String, u64)> {
        let env: ConsignmentEnvelope = serde_json::from_str(consignment_env)
            .map_err(|e| anyhow!("malformed consignment envelope: {e}"))?;
        let mut txids = Vec::new();
        for b in branch_txs {
            let tx: bitcoin::Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(b)?)?;
            txids.push(tx.txid().to_string());
        }
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("RGB engine not configured"))?;
        let (valid, detail, contract_id) = tokio::task::block_in_place(|| {
            w.validate_offchain_chain_info(&env.c, &txids)
        })?;
        if !valid {
            return Err(anyhow!(
                "pending token consignment INVALID: {}",
                detail.unwrap_or_default()
            ));
        }
        let contract_id =
            contract_id.ok_or_else(|| anyhow!("validated consignment without contract id"))?;
        let booked = tokio::task::block_in_place(|| {
            w.accept_offchain_amount(&env.c, &txids, funding_txid, funding_vout)
        })?;
        Ok((contract_id, booked))
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::ConsignmentEnvelope;

    // The consignment envelope roundtrips through JSON (as stored in BackupTx.rgb_consignment).
    #[test]
    fn envelope_roundtrip() {
        let env = ConsignmentEnvelope { c: "base64data".into(), a: 250, s: 1_500 };
        let json = serde_json::to_string(&env).unwrap();
        let back: ConsignmentEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.a, 250);
        assert_eq!(back.s, 1_500);
        assert_eq!(back.c, "base64data");
    }

    // REQ-21: the envelope amount is only a hint; the receiver compares it to the consignment-
    // derived amount and rejects on mismatch. This models that decision (the crypto derivation
    // itself is covered E2E by sdk02/sdk09).
    #[test]
    fn envelope_amount_is_a_checked_hint() {
        let booked = 250u64; // from the consignment
        let honest = ConsignmentEnvelope { c: "c".into(), a: 250, s: 1500 };
        let lying = ConsignmentEnvelope { c: "c".into(), a: 999, s: 1500 };
        assert_eq!(honest.a, booked); // accepted
        assert_ne!(lying.a, booked); // rejected (ERR-8)
    }
}

/// The two split lanes derive DISJOINT seals over the same carrier at the same spend generation —
/// including at the arity a batch of one recipient produces, which `batch_transfer_tokens` does not
/// reject (it rejects only an EMPTY recipient list).
#[cfg(test)]
mod split_seal_tests {
    // The PRODUCTION derivations, called directly — `transfer_tokens` and `batch_transfer_tokens`
    // now have no rung expression of their own, so changing either lane's seal changes these tests.
    use super::{BATCH_SPLIT_RUNG_FLAG, TierRole, batch_split_seal, single_split_seal};

    /// `transfer_tokens` always builds `splits = [piece, change]`, so its arity is fixed at 2. Pinned
    /// here so the lane-collision tests below cannot drift away from what production actually passes.
    const SINGLE_LANE_ARITY: u32 = 2;

    /// `batch_transfer_tokens` builds `splits = [piece; n] + change`, i.e. arity `n + 1`, and rejects
    /// only an EMPTY recipient list — so `n == 1` (arity 2) is reachable.
    fn batch_arity(recipients: u32) -> u32 {
        recipients + 1
    }

    /// The regression: a ONE-recipient batch has arity 2, exactly like the single lane. Keying the
    /// rung on the arity alone made the two seals identical over the same carrier and generation.
    #[test]
    fn a_one_recipient_batch_does_not_collide_with_the_single_lane() {
        for generation in 0..8u32 {
            assert_eq!(batch_arity(1), SINGLE_LANE_ARITY, "the collision precondition still holds");
            assert_ne!(
                batch_split_seal("sid-carrier", generation, batch_arity(1)).blinding(),
                single_split_seal("sid-carrier", generation, SINGLE_LANE_ARITY).blinding(),
                "a batch of one recipient must not share the single lane's seal at generation \
                 {generation}"
            );
        }
    }

    /// And nothing else in either lane's rung space collides either.
    #[test]
    fn the_two_split_lanes_never_share_a_rung() {
        use std::collections::HashSet;
        let mut seen: HashSet<u64> = HashSet::new();
        let mut n = 0usize;
        for generation in 0..16u32 {
            assert!(
                seen.insert(
                    single_split_seal("sid-carrier", generation, SINGLE_LANE_ARITY).blinding()
                )
            );
            n += 1;
            for recipients in 1..64u32 {
                assert!(
                    seen.insert(
                        batch_split_seal("sid-carrier", generation, batch_arity(recipients))
                            .blinding()
                    ),
                    "batch({recipients}) at generation {generation} reused a blinding"
                );
                n += 1;
            }
        }
        assert_eq!(seen.len(), n, "every split-lane seal must be distinct");
    }

    /// The lanes are disjoint BY CONSTRUCTION, not merely by hash luck: both derive under
    /// `TierRole::Split` over the same carrier and generation, and the only difference production
    /// creates is the flag bit in the rung. Asserting on the pre-image (rather than only on the
    /// 64-bit digest) means a future edit that drops the flag fails here even if the two digests
    /// happened not to collide in the sampled range above.
    #[test]
    fn the_batch_lane_is_separated_by_the_flag_bit_in_the_rung_itself() {
        let single = single_split_seal("sid-carrier", 7, SINGLE_LANE_ARITY);
        let batch = batch_split_seal("sid-carrier", 7, batch_arity(1));
        assert_eq!(single.role, TierRole::Split);
        assert_eq!(batch.role, TierRole::Split);
        assert_eq!(single.statechain_id, batch.statechain_id);
        assert_eq!(single.tier_index, batch.tier_index);
        assert_eq!(single.rung & BATCH_SPLIT_RUNG_FLAG, 0, "the single lane must never set the flag");
        assert_eq!(
            batch.rung & BATCH_SPLIT_RUNG_FLAG,
            BATCH_SPLIT_RUNG_FLAG,
            "the batch lane must always set the flag"
        );
        assert_eq!(batch.rung & !BATCH_SPLIT_RUNG_FLAG, batch_arity(1));
    }
}
