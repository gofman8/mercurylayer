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
const TOKEN_PIECE_SATS: u64 = 1_500;

/// Envelope stored in `BackupTx.rgb_consignment` so a token transfer is self-describing.
#[derive(Serialize, Deserialize)]
pub(crate) struct ConsignmentEnvelope {
    /// Consignment, base64.
    pub c: String,
    /// Token amount assigned to the receiver's sub-coin (validated contract-side on receive).
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

    /// Send `token_amount` of `asset_id` to a statechain address, entirely off-chain: colored
    /// split (exact token piece + change back to this wallet) then branch-carrying key handover
    /// of the piece coin. The receiver's SDK auto-claims, validates the consignment off-chain and
    /// books the balance.
    pub async fn transfer_tokens(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
    ) -> Result<TransferResult> {
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
        let (mut carrier, carrier_amount) = carrier.ok_or_else(|| {
            anyhow!("no single coin carries >= {token_amount} of {asset_id} (multi-coin token combine not yet wired)")
        })?;
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

        // Hand the piece over.
        mercuryrustlib::transfer_sender::execute(
            &self.inner.cc,
            receiver_address,
            &self.inner.config.wallet_name,
            &piece_id,
            None,
            false,
            None,
        )
        .await?;

        let _ = change_id;
        Ok(TransferResult {
            receiver_address: receiver_address.to_string(),
            total_sats: TOKEN_PIECE_SATS,
            coins: vec![TransferredCoin {
                statechain_id: piece_id,
                amount_sats: TOKEN_PIECE_SATS,
            }],
            used_split: true,
        })
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
        tokio::task::block_in_place(|| -> Result<()> {
            // First sight of this contract: import it (genesis + history) into the stash so the
            // allocation rows have their asset to reference — validated against the same branch.
            w.import_asset_offchain(&env.c, &txids)?;
            w.register_statechain(&txid, vout, sats, &contract_id, env.a, &[])?;
            Ok(())
        })?;
        Ok(Some((contract_id, env.a)))
    }
}
