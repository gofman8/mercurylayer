//! Sending: exact-amount transfers with automatic coin selection and off-chain split.
//!
//! Mercury transfers move whole statechain coins (like Spark leaves). `transfer` makes arbitrary
//! amounts frictionless:
//! 1. If a subset of confirmed coins sums to the amount exactly → native key-handover transfer of
//!    each coin (works with any Mercury wallet as receiver, fully async).
//! 2. Otherwise → the SDK **splits one coin off-chain** (SE-co-signed, un-broadcast, single-use
//!    sub-coins) to mint the exact remainder, then transfers the pieces. The split sub-coins are
//!    SDK-native coins; the sender keeps the change sub-coin.

use anyhow::{anyhow, Result};
use bitcoin::psbt::Psbt;
use mercurylib::transaction::{
    create_and_commit_nonces, create_signature, get_partial_sig_request_for_colored_tx,
    get_unsigned_split_psbt, new_backup_transaction,
};
use mercurylib::wallet::{Coin, CoinStatus};
use std::str::FromStr;

use crate::select::{self, Candidate, Plan};
use crate::types::{SdkError, TransferResult, TransferredCoin};
use crate::wallet::SparkWallet;

impl SparkWallet {
    /// Send `amount_sats` to a statechain address. Exact amounts always work: the SDK either finds
    /// an exact subset of coins or mints one via an off-chain split. The receiver claims
    /// asynchronously (their SDK background watcher, or any Mercury wallet's receive flow for the
    /// exact-subset path).
    pub async fn transfer(&self, receiver_address: &str, amount_sats: u64) -> Result<TransferResult> {
        let _guard = self.inner.wallet_lock.lock().await;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;

        let spendable: Vec<&Coin> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .collect();
        let candidates: Vec<Candidate> = spendable
            .iter()
            .enumerate()
            .map(|(index, c)| Candidate {
                index,
                amount_sats: c.amount.unwrap_or_default() as u64,
            })
            .collect();

        let plan = select::plan(&candidates, amount_sats);
        let (mut to_send, used_split): (Vec<String>, bool) = match plan {
            Plan::Insufficient { available } => {
                return Err(SdkError::InsufficientBalance {
                    requested_sats: amount_sats,
                    available_sats: available,
                }
                .into());
            }
            Plan::Exact(indices) => (
                indices
                    .iter()
                    .filter_map(|&i| spendable[i].statechain_id.clone())
                    .collect(),
                false,
            ),
            Plan::WithSplit {
                whole,
                split,
                split_amount,
            } => {
                let mut ids: Vec<String> = whole
                    .iter()
                    .filter_map(|&i| spendable[i].statechain_id.clone())
                    .collect();
                let split_coin_id = spendable[split]
                    .statechain_id
                    .clone()
                    .ok_or_else(|| anyhow!("coin without statechain id"))?;
                drop(spendable);
                drop(record);
                let (piece_id, _change_id) =
                    self.split_coin(&split_coin_id, split_amount).await?;
                ids.push(piece_id);
                (ids, true)
            }
        };

        // Hand each coin over (async key handover through the SE message relay).
        let mut coins = Vec::new();
        let record = self.record().await?;
        for id in to_send.drain(..) {
            let amount = record
                .coins
                .iter()
                .filter(|c| c.statechain_id.as_deref() == Some(id.as_str()))
                .filter_map(|c| c.amount)
                .next_back()
                .unwrap_or_default() as u64;
            mercuryrustlib::transfer_sender::execute(
                &self.inner.cc,
                receiver_address,
                &self.inner.config.wallet_name,
                &id,
                None,
                false,
                None,
            )
            .await?;
            coins.push(TransferredCoin {
                statechain_id: id,
                amount_sats: amount,
            });
        }

        Ok(TransferResult {
            receiver_address: receiver_address.to_string(),
            total_sats: coins.iter().map(|c| c.amount_sats).sum(),
            coins,
            used_split,
        })
    }

    /// Split a confirmed coin into (`piece_sats`, remainder) **off-chain**: one SE-co-signed,
    /// un-broadcast transaction whose outputs are two fresh single-use statechain coins owned by
    /// this wallet. Returns (piece statechain_id, change statechain_id). Broadcasting the split tx
    /// later is the unilateral exit for the sub-coins.
    pub async fn split_coin(&self, statechain_id: &str, piece_sats: u64) -> Result<(String, String)> {
        let record = self.record().await?;
        let parent = record
            .coins
            .iter()
            .find(|c| {
                c.statechain_id.as_deref() == Some(statechain_id)
                    && c.status == CoinStatus::CONFIRMED
                    && c.duplicate_index == 0
            })
            .cloned()
            .ok_or_else(|| anyhow!("no confirmed coin with statechain id {statechain_id}"))?;
        let parent_sats = parent.amount.unwrap_or_default() as u64;
        // Reserve a miner-fee margin for the (only-on-exit) broadcast of the split tx.
        let fee_reserve = split_fee_reserve(parent_sats);
        if piece_sats + fee_reserve >= parent_sats {
            return Err(anyhow!(
                "piece {piece_sats} + fee reserve {fee_reserve} does not fit in coin of {parent_sats} sats"
            ));
        }
        let change_sats = parent_sats - piece_sats - fee_reserve;

        // Two fresh statechain slots owned by this wallet (SE handshake only — no on-chain tx).
        // Normal coins: sub-coin security is Mercury's decrementing-locktime scheme, with the
        // split tx as the shared exit branch below the parent's deposit backup.
        let token_a = self.take_token().await?;
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(piece_sats)?,
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

        // Build + blind-MuSig2 co-sign the un-broadcast split tx (plain BTC: no coloring step).
        // qt = parent's backup count + 1 so the split's locktime sits one decrement BELOW the
        // parent's own backup — the branch always wins the exit race against stale parent state.
        let parent_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await
        .map(|v| v.len() as u32)
        .unwrap_or(0);
        let mut parent = parent;
        let signed = self
            .sign_split_tx(
                &mut parent,
                &[(piece_addr.clone(), piece_sats), (change_addr.clone(), change_sats)],
                parent_backups + 1,
            )
            .await?;
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&signed)?)?;
        let txid = tx.txid().to_string();

        // Register both sub-coins (coin records + backups + shared branch).
        let (piece_id, change_id) = self
            .register_split_subcoins(
                statechain_id,
                &signed,
                &txid,
                &[
                    (piece_addr.clone(), 0, piece_sats),
                    (change_addr.clone(), 1, change_sats),
                ],
            )
            .await?;
        Ok((piece_id, change_id))
    }

    /// Register the outputs of a signed (un-broadcast) split tx as wallet coins: patch the
    /// freshly-initialised coin records onto the outputs, mark the parent spent, give each
    /// sub-coin its own first backup tx + locktime, and persist the shared exit branch under
    /// "branch-<id>". `outputs` is `[(aggregated_address, vout, sats), ...]`; returns the
    /// statechain ids in the same order (currently piece, change).
    pub(crate) async fn register_split_subcoins(
        &self,
        parent_statechain_id: &str,
        signed_split_tx_hex: &str,
        split_txid: &str,
        outputs: &[(String, u32, u64)],
    ) -> Result<(String, String)> {
        let mut record = self.record().await?;
        let mut ids: Vec<String> = vec![String::new(); outputs.len()];
        for coin in record.coins.iter_mut() {
            let addr = coin.aggregated_address.clone().unwrap_or_default();
            if coin.status == CoinStatus::INITIALISED {
                if let Some((i, (_, vout, sats))) =
                    outputs.iter().enumerate().find(|(_, (a, _, _))| *a == addr)
                {
                    coin.utxo_txid = Some(split_txid.to_string());
                    coin.utxo_vout = Some(*vout);
                    coin.amount = Some(u32::try_from(*sats)?);
                    coin.status = CoinStatus::CONFIRMED;
                    ids[i] = coin.statechain_id.clone().unwrap_or_default();
                    continue;
                }
            }
            if coin.statechain_id.as_deref() == Some(parent_statechain_id)
                && coin.duplicate_index == 0
            {
                // Parent is terminally spent by the split.
                coin.status = CoinStatus::WITHDRAWN;
            }
        }
        if ids.iter().any(|i| i.is_empty()) {
            return Err(anyhow!("split sub-coin registration failed"));
        }

        // Each sub-coin gets its own first backup tx (exit leaf) + locktime.
        let network = self.inner.config.network.to_string();
        let mut sub_backups: Vec<(String, mercurylib::wallet::BackupTx)> = Vec::new();
        for coin in record.coins.iter_mut() {
            let id = coin.statechain_id.clone().unwrap_or_default();
            if ids.contains(&id) && coin.status == CoinStatus::CONFIRMED {
                let bkp =
                    mercuryrustlib::deposit::create_tx1(&self.inner.cc, coin, &network, 1).await?;
                coin.locktime = Some(mercurylib::utils::get_blockheight(&bkp)?);
                sub_backups.push((id, bkp));
            }
        }
        self.save_record(&record).await?;

        let branch = mercurylib::wallet::BackupTx {
            tx_n: 1,
            tx: signed_split_tx_hex.to_string(),
            client_public_nonce: String::new(),
            server_public_nonce: String::new(),
            client_public_key: String::new(),
            server_public_key: String::new(),
            blinding_factor: String::new(),
            rgb_consignment: None,
            rgb_blinding: None,
        };
        for (id, bkp) in &sub_backups {
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                id,
                &vec![bkp.clone()],
            )
            .await?;
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("branch-{id}"),
                &vec![branch.clone()],
            )
            .await?;
        }

        Ok((ids[0].clone(), ids[1].clone()))
    }

    /// Blind-MuSig2 co-sign a multi-output spend of `coin` (the plain-BTC split; the RGB-colored
    /// variant lives in `mercuryrustlib::rgb::create_colored_split_tx`). `qt_backup_tx` positions
    /// the split's locktime in the decrement ladder (backup count + 1 beats the parent's backup).
    async fn sign_split_tx(
        &self,
        coin: &mut Coin,
        outputs: &[(String, u64)],
        qt_backup_tx: u32,
    ) -> Result<String> {
        let cc = &self.inner.cc;
        let network = self.inner.config.network.to_string();
        let server_info = mercuryrustlib::utils::info_config(cc).await?;

        let coin_nonce = create_and_commit_nonces(coin)?;
        coin.secret_nonce = Some(coin_nonce.secret_nonce.clone());
        coin.public_nonce = Some(coin_nonce.public_nonce.clone());
        coin.blinding_factor = Some(coin_nonce.blinding_factor.clone());
        let server_public_nonce = mercuryrustlib::transaction::sign_first(
            cc,
            &coin_nonce.sign_first_request_payload,
        )
        .await?;
        coin.server_public_nonce = Some(server_public_nonce);

        let block_height = {
            use electrum_client::ElectrumApi;
            cc.electrum_client.block_headers_subscribe_raw()?.height as u32
        };

        let unsigned_psbt_b64 = get_unsigned_split_psbt(
            coin,
            block_height,
            server_info.initlock,
            server_info.interval,
            qt_backup_tx,
            outputs.to_vec(),
            network.clone(),
            false,
        )?;
        let psbt = Psbt::from_str(&unsigned_psbt_b64)
            .map_err(|e| anyhow!("could not parse split psbt: {e}"))?;
        let tx_hex = hex::encode(bitcoin::consensus::encode::serialize(&psbt.unsigned_tx));

        let partial_sig_request =
            get_partial_sig_request_for_colored_tx(coin, tx_hex.clone(), network)?;
        let server_partial_sig = mercuryrustlib::transaction::sign_second(
            cc,
            &partial_sig_request.partial_signature_request_payload,
        )
        .await?;
        let signature = create_signature(
            partial_sig_request.msg,
            partial_sig_request.client_partial_sig,
            hex::encode(server_partial_sig.serialize()),
            partial_sig_request.encoded_session,
            partial_sig_request.output_pubkey,
        )?;
        Ok(new_backup_transaction(tx_hex, signature)?)
    }
}

/// Miner-fee margin left in a split tx for its (exit-only) broadcast.
fn split_fee_reserve(parent_sats: u64) -> u64 {
    // ~200 vB at a couple sat/vB, floored so tiny test coins still split.
    (parent_sats / 100).clamp(300, 2_000)
}
