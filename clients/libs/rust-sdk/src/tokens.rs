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
use serde::{Deserialize, Serialize};

use crate::types::{SdkError, TokenBalance, TransferResult, TransferredCoin};
use crate::wallet::SparkWallet;

/// Seal blinding used for SDK token flows (both sides derive validation from the consignment, so
/// a fixed value is fine; randomize per-transfer once bindings expose it end-to-end).
pub(crate) const TOKEN_BLINDING: u64 = 777;
/// Sats carried by a token-piece sub-coin (just above dust; the token is the payload).
/// `pub(crate)` so the granularity model (`granularity_model.rs`) can pin the carrier floor.
pub(crate) const TOKEN_PIECE_SATS: u64 = 1_500;

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

impl SparkWallet {
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
        let (txid, vout) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let (txid, vout, _consignment, signed_tx) = tokio::task::block_in_place(|| {
                w.fund_statechain(&sc_address, deposit_sats, &asset_id, supply, 2, TOKEN_BLINDING)
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
        let (txid, vout) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let (txid, vout, _c, signed_tx) = tokio::task::block_in_place(|| {
                w.fund_statechain(&sc_address, deposit_sats, asset_id, amount, 2, TOKEN_BLINDING)
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

        // Fresh slots for piece and change.
        let token_a = self.take_token().await?;
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(TOKEN_PIECE_SATS)?,
        )
        .await?;
        let token_b = self.take_token().await?;
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
                TOKEN_BLINDING,
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
            first.rgb_blinding = Some(TOKEN_BLINDING);
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

        // 3. Fresh slots for the piece + change.
        let token_a = self.take_token().await?;
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(TOKEN_PIECE_SATS)?,
        )
        .await?;
        let token_b = self.take_token().await?;
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
                TOKEN_BLINDING,
            )
            .await?
        };
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
            first.rgb_blinding = Some(TOKEN_BLINDING);
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

        // One fresh slot per recipient piece + one for change; build the N+1 colored split.
        let mut splits: Vec<(String, u64, u64)> = Vec::with_capacity(n + 1);
        let mut piece_addrs: Vec<String> = Vec::with_capacity(n);
        for (_, amount) in transfers {
            let tk = self.take_token().await?;
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
        let change_tk = self.take_token().await?;
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
                TOKEN_BLINDING,
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
                first.rgb_blinding = Some(TOKEN_BLINDING);
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
            return Err(anyhow!(
                "incoming token consignment INVALID: {}",
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
            return Err(anyhow!(
                "token consignment assigns {booked} to this coin but the envelope claimed {} — rejecting",
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
