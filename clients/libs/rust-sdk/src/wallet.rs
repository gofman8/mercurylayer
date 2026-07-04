use std::sync::Arc;

use anyhow::{anyhow, Result};
use mercurylib::wallet::{Coin, CoinStatus, Wallet as WalletRecord};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::sqlite_manager::{get_wallet, insert_wallet, update_wallet};
use tokio::sync::{broadcast, Mutex};

use crate::config::SdkConfig;
use crate::events::WalletEvent;
use crate::types::{Balance, ClaimResult, DepositAddressInfo, SdkError};

pub(crate) struct Inner {
    pub cc: ClientConfig,
    pub config: SdkConfig,
    pub events_tx: broadcast::Sender<WalletEvent>,
    /// Pre-paid deposit-token ids, consumed one per statechain slot (deposit or split output).
    pub token_pool: Mutex<Vec<String>>,
    /// Guards wallet-record read-modify-write cycles within this process.
    pub wallet_lock: Mutex<()>,
    /// Lazily-opened RGB engine (token support); None until first token operation.
    pub rgb: Mutex<Option<mercury_rgb::RgbWallet>>,
}

/// Spark-parity wallet on Mercury+RGB. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct SparkWallet {
    pub(crate) inner: Arc<Inner>,
}

impl SparkWallet {
    /// Create or load a wallet. Returns the wallet and its mnemonic (persist it — it is the
    /// only backup needed to restore the statechain keys).
    ///
    /// - No wallet named `config.wallet_name` in the database: a new one is created, from
    ///   `mnemonic` if given, freshly generated otherwise.
    /// - Wallet exists: it is loaded; a differing `mnemonic` argument is an error.
    pub async fn initialize(config: SdkConfig, mnemonic: Option<&str>) -> Result<(Self, String)> {
        let cc = ClientConfig::from_params(
            config.statechain_entity_url.clone(),
            config.electrum_url.clone(),
            config.electrum_type.clone(),
            config.network,
            config.database_file.clone(),
            config.confirmation_target,
        )
        .await?;

        let record = match get_wallet(&cc.pool, &config.wallet_name).await {
            Ok(existing) => {
                if let Some(m) = mnemonic {
                    if existing.mnemonic != m {
                        return Err(anyhow!(
                            "wallet '{}' already exists with a different mnemonic",
                            config.wallet_name
                        ));
                    }
                }
                existing
            }
            Err(_) => {
                let record = build_wallet_record(&cc, &config.wallet_name, mnemonic).await?;
                insert_wallet(&cc.pool, &record).await?;
                record
            }
        };
        let mnemonic_out = record.mnemonic.clone();

        let (events_tx, _) = broadcast::channel(256);
        let wallet = SparkWallet {
            inner: Arc::new(Inner {
                cc,
                config,
                events_tx,
                token_pool: Mutex::new(Vec::new()),
                wallet_lock: Mutex::new(()),
                rgb: Mutex::new(None),
            }),
        };
        Ok((wallet, mnemonic_out))
    }

    /// Subscribe to wallet events (deposits confirmed, transfers claimed, balance updates).
    pub fn subscribe(&self) -> broadcast::Receiver<WalletEvent> {
        self.inner.events_tx.subscribe()
    }

    /// Fixed derivation path for the wallet's stable identity key (distinct from coin keys at
    /// m/86h/0h/0h and auth keys at m/89h/0h/0h).
    const IDENTITY_PATH: &'static str = "m/1000h/0h/0h";

    /// The wallet's stable identity keypair, deterministically derived from the seed — it does not
    /// change as coins come and go (unlike per-coin keys).
    async fn identity_keypair(&self) -> Result<(bitcoin::secp256k1::SecretKey, bitcoin::secp256k1::PublicKey)> {
        let record = self.record().await?;
        let kd = record
            .generate_new_key(Self::IDENTITY_PATH, 0, 0)
            .map_err(|e| anyhow!("identity key derivation failed: {e:?}"))?;
        let sk = bitcoin::secp256k1::SecretKey::from_slice(&kd.secret_key.secret_bytes())?;
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let pk = bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &sk);
        Ok((sk, pk))
    }

    /// The wallet's stable identity public key (hex, 33-byte compressed).
    pub async fn get_identity_public_key(&self) -> Result<String> {
        let (_, pk) = self.identity_keypair().await?;
        Ok(hex::encode(pk.serialize()))
    }

    /// Sign a message with the identity key (BIP340 Schnorr over sha256(message)). Returns the
    /// 64-byte signature (hex). Mirrors Spark's `signMessageWithIdentityKey`.
    pub async fn sign_message_with_identity_key(&self, message: &[u8]) -> Result<String> {
        use bitcoin::secp256k1::{KeyPair, Message, Secp256k1};
        use sha2::{Digest, Sha256};
        let (sk, _) = self.identity_keypair().await?;
        let secp = Secp256k1::new();
        let keypair = KeyPair::from_secret_key(&secp, &sk);
        let digest = Sha256::digest(message);
        let msg = Message::from_slice(&digest)?;
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &keypair);
        Ok(hex::encode(sig.as_ref()))
    }

    /// Verify a Schnorr signature over sha256(message) against a compressed identity public key
    /// (hex). Mirrors Spark's `validateMessageWithIdentityKey`.
    pub fn validate_message_with_identity_key(message: &[u8], signature_hex: &str, public_key_hex: &str) -> Result<bool> {
        use bitcoin::secp256k1::{schnorr::Signature, Message, Secp256k1, XOnlyPublicKey};
        use sha2::{Digest, Sha256};
        let secp = Secp256k1::new();
        let sig = Signature::from_slice(&hex::decode(signature_hex)?)?;
        let pk = bitcoin::secp256k1::PublicKey::from_slice(&hex::decode(public_key_hex)?)?;
        let xonly = XOnlyPublicKey::from(pk);
        let digest = Sha256::digest(message);
        let msg = Message::from_slice(&digest)?;
        Ok(secp.verify_schnorr(&sig, &msg, &xonly).is_ok())
    }

    /// The wallet's stable statechain address (bech32m `ml1…`/`tml1…`) — hand this to senders.
    /// Reuse is supported; each incoming transfer lands on its own coin.
    pub async fn get_spark_address(&self) -> Result<String> {
        let _guard = self.inner.wallet_lock.lock().await;
        let record = self.record().await?;
        if let Some(c) = record.coins.first() {
            return Ok(c.address.clone());
        }
        drop(record);
        // No coins yet: create the identity receive slot (persisted).
        let addr = mercuryrustlib::transfer_receiver::new_transfer_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
        )
        .await?;
        Ok(addr)
    }

    /// Balance across all coins, including per-asset token balances (when configured).
    pub async fn get_balance(&self) -> Result<Balance> {
        let record = self.record().await?;
        let mut balance = compute_balance(&record);
        balance.tokens = self.get_token_balances().await.unwrap_or_default();
        Ok(balance)
    }

    /// Provide a pre-paid, confirmed deposit-token id. Each statechain slot (deposit address or
    /// off-chain split output) consumes one token. Without pooled tokens the SDK requests one from
    /// the SE and surfaces `SdkError::TokenPaymentRequired` if it is not free.
    pub async fn add_prepaid_token(&self, token_id: &str) {
        self.inner.token_pool.lock().await.push(token_id.to_string());
    }

    /// Take a usable token id: pooled first, then config default, then ask the SE.
    pub(crate) async fn take_token(&self) -> Result<String> {
        if let Some(t) = self.inner.token_pool.lock().await.pop() {
            return Ok(t);
        }
        if let Some(t) = &self.inner.config.deposit_token_id {
            return Ok(t.clone());
        }
        let token = mercuryrustlib::deposit::get_token(&self.inner.cc).await?;
        // A token that demands payment cannot be consumed silently — surface it.
        if token.payment_method != "free" && token.fee > 0 {
            return Err(SdkError::TokenPaymentRequired {
                token_id: token.token_id.clone(),
                deposit_address: token.deposit_address.clone().unwrap_or_default(),
                fee_sats: token.fee,
            }
            .into());
        }
        Ok(token.token_id)
    }

    /// Fresh single-use Bitcoin deposit address for `amount_sats`. Full Mercury security: the
    /// deposit gets a decrementing-locktime backup transaction (unilateral exit) automatically.
    pub async fn get_deposit_address(&self, amount_sats: u64) -> Result<String> {
        let token_id = self.take_token().await?;
        let address = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_id,
            u32::try_from(amount_sats)?,
        )
        .await?;
        Ok(address)
    }

    /// One claim pass: detect deposits (mempool → confirmed), claim incoming transfers, emit
    /// events. The background watcher calls this on an interval; apps can also call it directly.
    pub async fn claim(&self) -> Result<ClaimResult> {
        let _guard = self.inner.wallet_lock.lock().await;
        let before = self.record().await?;
        let confirmed_before: Vec<String> = coins_in(&before, CoinStatus::CONFIRMED);

        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let receive = mercuryrustlib::transfer_receiver::execute(
            &self.inner.cc,
            &self.inner.config.wallet_name,
        )
        .await?;
        // transfer_receiver::execute updates statuses of freshly claimed coins internally; refresh
        // once more so claimed coins show as confirmed.
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;

        let after = self.record().await?;
        let mut confirmed_deposits = Vec::new();
        for coin in after.coins.iter() {
            if coin.status == CoinStatus::CONFIRMED {
                let key = coin_key(coin);
                if !confirmed_before.contains(&key) {
                    confirmed_deposits.push(DepositAddressInfo {
                        address: coin.aggregated_address.clone().unwrap_or_default(),
                        amount_sats: coin.amount.unwrap_or_default() as u64,
                        statechain_id: coin.statechain_id.clone(),
                    });
                }
            }
        }

        for d in &confirmed_deposits {
            let _ = self.inner.events_tx.send(WalletEvent::DepositConfirmed {
                address: d.address.clone(),
                amount_sats: d.amount_sats,
            });
        }
        if !receive.received_statechain_ids.is_empty() {
            let _ = self.inner.events_tx.send(WalletEvent::TransferClaimed {
                statechain_ids: receive.received_statechain_ids.clone(),
            });
            // Token hook: coins that arrived with a consignment get validated + booked.
            for id in &receive.received_statechain_ids {
                match self.accept_incoming_tokens(id).await {
                    Ok(Some((asset_id, amount))) => {
                        let _ = self.inner.events_tx.send(WalletEvent::TokenTransferClaimed {
                            asset_id,
                            amount,
                            statechain_id: id.clone(),
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        println!("token accept error for {id}: {e}");
                    }
                }
            }
        }
        if !confirmed_deposits.is_empty() || !receive.received_statechain_ids.is_empty() {
            let _ = self.inner.events_tx.send(WalletEvent::BalanceUpdate {
                balance: compute_balance(&after),
            });
        }

        Ok(ClaimResult {
            claimed_transfers: receive.received_statechain_ids.len() as u32,
            confirmed_deposits,
        })
    }

    /// Start the background watcher (deposit confirmation + incoming-transfer auto-claim).
    /// Returns a handle; abort it to stop. Mirrors the Spark SDK's background stream + claim
    /// automation (poll-based — Mercury has no server push).
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        let wallet = self.clone();
        let interval = self.inner.config.poll_interval_secs.max(1);
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }

    /// Cooperative exit: withdraw specific coins (or `None` = all confirmed coins) to an on-chain
    /// address. One on-chain transaction per coin, SE co-signed, no timelock wait.
    pub async fn withdraw(
        &self,
        to_address: &str,
        statechain_ids: Option<Vec<String>>,
        fee_rate: Option<f64>,
    ) -> Result<Vec<String>> {
        let record = self.record().await?;
        let ids: Vec<String> = match statechain_ids {
            Some(ids) => ids,
            None => record
                .coins
                .iter()
                .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
                .filter_map(|c| c.statechain_id.clone())
                .collect(),
        };
        let mut withdrawn = Vec::new();
        for id in ids {
            // An off-chain sub-coin's funding tx is un-broadcast: materialize its exit branch
            // first so the withdraw spend has an on-chain input. Idempotent for shared branches.
            self.broadcast_branch_if_any(&id).await?;
            mercuryrustlib::withdraw::execute(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &id,
                to_address,
                fee_rate,
                None,
            )
            .await?;
            withdrawn.push(id);
        }
        Ok(withdrawn)
    }

    /// Broadcast the stored exit branch of a coin, root-first, if one exists. Returns whether a
    /// branch exists. Broadcast errors for already-confirmed branch txs (shared with a sibling
    /// coin's earlier exit) are tolerated; other errors surface.
    pub(crate) async fn broadcast_branch_if_any(&self, statechain_id: &str) -> Result<bool> {
        let branch = match mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &format!("branch-{statechain_id}"),
        )
        .await
        {
            Ok(b) if !b.is_empty() => b,
            _ => return Ok(false),
        };
        use electrum_client::ElectrumApi;
        for b in branch.iter() {
            let tx: bitcoin::Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&b.tx)?)?;
            match self.inner.cc.electrum_client.transaction_broadcast(&tx) {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    // Tolerate an already-materialized branch (sibling exit / rebroadcast).
                    if !(msg.contains("already") || msg.contains("in block chain") || msg.contains("txn-mempool-conflict")) {
                        return Err(anyhow!("branch broadcast failed for {statechain_id}: {msg}"));
                    }
                }
            }
        }
        Ok(true)
    }

    /// Estimate the cost and readiness of unilaterally exiting a coin: how many transactions,
    /// their total vsize (fee = vsize x feerate via [`crate::types::ExitCostEstimate::fee_sats_at`]),
    /// and how many blocks remain until the backup's locktime allows broadcast.
    pub async fn estimate_exit_cost(&self, statechain_id: &str) -> Result<crate::types::ExitCostEstimate> {
        use electrum_client::ElectrumApi;
        let branch = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &format!("branch-{statechain_id}"),
        )
        .await
        .unwrap_or_default();
        let backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?;
        let latest = backups
            .iter()
            .max_by_key(|b| b.tx_n)
            .ok_or_else(|| anyhow!("no backup tx stored for {statechain_id}"))?;

        let mut branch_vbytes = 0u64;
        for b in &branch {
            let tx: bitcoin::Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&b.tx)?)?;
            branch_vbytes += tx.vsize() as u64;
        }
        let backup_tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&latest.tx)?)?;
        let backup_vbytes = backup_tx.vsize() as u64;

        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let locktime = mercurylib::utils::get_blockheight(latest)?;
        let wait_blocks = locktime.saturating_sub(tip);

        Ok(crate::types::ExitCostEstimate {
            statechain_id: statechain_id.to_string(),
            branch_txs: branch.len() as u32,
            branch_vbytes,
            backup_vbytes,
            total_vbytes: branch_vbytes + backup_vbytes,
            wait_blocks,
        })
    }

    /// Unilateral exit: broadcast the exit branch (immediately valid) and the latest pre-signed
    /// backup tx for each coin. Needs no SE cooperation. A backup whose locktime has not been
    /// reached is reported as `complete=false` with the remaining `wait_blocks` — call again
    /// once the chain advances (the branch stays out either way).
    pub async fn unilateral_exit(
        &self,
        statechain_ids: Option<Vec<String>>,
        _to_address: Option<String>,
    ) -> Result<Vec<crate::types::ExitStatus>> {
        use electrum_client::ElectrumApi;
        let record = self.record().await?;
        let ids: Vec<String> = match statechain_ids {
            Some(ids) => ids,
            None => record
                .coins
                .iter()
                .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
                .filter_map(|c| c.statechain_id.clone())
                .collect(),
        };
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let mut statuses = Vec::new();
        for id in ids {
            // Materialize the coin's funding first (no locktime on branch txs).
            self.broadcast_branch_if_any(&id).await?;

            let backups = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &id,
            )
            .await?;
            let latest = backups
                .iter()
                .max_by_key(|b| b.tx_n)
                .ok_or_else(|| anyhow!("no backup tx stored for {id}"))?;
            let locktime = mercurylib::utils::get_blockheight(latest)?;
            if locktime > tip {
                statuses.push(crate::types::ExitStatus {
                    statechain_id: id,
                    complete: false,
                    wait_blocks: locktime - tip,
                });
                continue;
            }
            let tx: bitcoin::Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&latest.tx)?)?;
            match self.inner.cc.electrum_client.transaction_broadcast(&tx) {
                Ok(_) => statuses.push(crate::types::ExitStatus {
                    statechain_id: id,
                    complete: true,
                    wait_blocks: 0,
                }),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("already") || msg.contains("in block chain") {
                        statuses.push(crate::types::ExitStatus {
                            statechain_id: id,
                            complete: true,
                            wait_blocks: 0,
                        });
                    } else {
                        return Err(anyhow!("backup broadcast failed for {id}: {msg}"));
                    }
                }
            }
        }
        Ok(statuses)
    }

    /// Transfer history (deposits, sends, receives).
    pub async fn get_activities(&self) -> Result<Vec<mercurylib::wallet::Activity>> {
        Ok(self.record().await?.activities)
    }

    /// Transfer history restricted to sends/receives (Spark's `getTransfers`). Deposits/withdraws
    /// come from `get_activities`.
    pub async fn get_transfers(&self) -> Result<Vec<mercurylib::wallet::Activity>> {
        Ok(self
            .record()
            .await?
            .activities
            .into_iter()
            .filter(|a| a.action == "Transfer" || a.action == "Receive")
            .collect())
    }

    /// Look up a single activity by its utxo (`txid:vout` or `txid`), Spark's `getTransfer(id)`.
    pub async fn get_transfer(&self, utxo: &str) -> Result<Option<mercurylib::wallet::Activity>> {
        Ok(self
            .record()
            .await?
            .activities
            .into_iter()
            .find(|a| a.utxo == utxo || a.utxo.starts_with(utxo)))
    }

    /// The wallet's coins (Spark's leaf/UTXO inventory). Off-chain sub-coins are flagged.
    pub async fn list_coins(&self) -> Result<Vec<crate::types::CoinInfo>> {
        let record = self.record().await?;
        let mut out = Vec::new();
        for c in &record.coins {
            if c.duplicate_index != 0 {
                continue;
            }
            let has_branch = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("branch-{}", c.statechain_id.clone().unwrap_or_default()),
            )
            .await
            .map(|v| !v.is_empty())
            .unwrap_or(false);
            out.push(crate::types::CoinInfo {
                statechain_id: c.statechain_id.clone(),
                amount_sats: c.amount.unwrap_or_default() as u64,
                status: format!("{}", c.status),
                utxo_txid: c.utxo_txid.clone(),
                utxo_vout: c.utxo_vout,
                off_chain: has_branch,
            });
        }
        Ok(out)
    }

    /// Estimate the cooperative-withdrawal fee for `coins` (or all confirmed coins) at the current
    /// electrum-estimated feerate. Spark's `getWithdrawalFeeQuote`.
    pub async fn get_withdrawal_fee_quote(
        &self,
        statechain_ids: Option<Vec<String>>,
    ) -> Result<crate::types::WithdrawalFeeQuote> {
        use electrum_client::ElectrumApi;
        let record = self.record().await?;
        let n = match &statechain_ids {
            Some(ids) => ids.len() as u32,
            None => record
                .coins
                .iter()
                .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
                .count() as u32,
        };
        // Electrum estimatefee returns BTC/kvB; convert to sat/vB, floor at 1.
        let btc_per_kvb = self.inner.cc.electrum_client.estimate_fee(2).unwrap_or(0.0);
        let mut fee_rate = (btc_per_kvb * 100_000.0).max(1.0); // BTC/kvB -> sat/vB
        if !fee_rate.is_finite() {
            fee_rate = 1.0;
        }
        // A cooperative withdraw is ~1 taproot key-spend input + 1 output ≈ 111 vB per coin.
        let est_vbytes = (n as u64) * 111;
        Ok(crate::types::WithdrawalFeeQuote {
            n_coins: n,
            est_vbytes,
            fee_rate_sat_vb: fee_rate,
            fee_sats: (est_vbytes as f64 * fee_rate).ceil() as u64,
        })
    }

    /// The underlying Mercury client config (advanced integrations, e.g. the SSP service).
    pub fn client_config(&self) -> &ClientConfig {
        &self.inner.cc
    }

    /// This wallet's name in the local database.
    pub fn wallet_name(&self) -> &str {
        &self.inner.config.wallet_name
    }

    pub(crate) async fn record(&self) -> Result<WalletRecord> {
        get_wallet(&self.inner.cc.pool, &self.inner.config.wallet_name).await
    }

    pub(crate) async fn save_record(&self, record: &WalletRecord) -> Result<()> {
        update_wallet(&self.inner.cc.pool, record).await
    }
}

/// Coins in a given status, as stable keys.
fn coins_in(record: &WalletRecord, status: CoinStatus) -> Vec<String> {
    record
        .coins
        .iter()
        .filter(|c| c.status == status)
        .map(coin_key)
        .collect()
}

fn coin_key(c: &Coin) -> String {
    format!(
        "{}:{}:{}",
        c.statechain_id.clone().unwrap_or_default(),
        c.utxo_txid.clone().unwrap_or_default(),
        c.utxo_vout.unwrap_or_default()
    )
}

pub(crate) fn compute_balance(record: &WalletRecord) -> Balance {
    let mut b = Balance::default();
    for c in &record.coins {
        if c.duplicate_index != 0 {
            continue;
        }
        let sats = c.amount.unwrap_or_default() as u64;
        match c.status {
            CoinStatus::CONFIRMED => b.available_sats += sats,
            CoinStatus::IN_MEMPOOL | CoinStatus::UNCONFIRMED => b.pending_sats += sats,
            CoinStatus::IN_TRANSFER => b.in_transfer_sats += sats,
            _ => {}
        }
    }
    b
}

/// Build a wallet record like `mercuryrustlib::wallet::create_wallet`, honouring a caller-supplied
/// mnemonic for restore.
async fn build_wallet_record(
    cc: &ClientConfig,
    name: &str,
    mnemonic: Option<&str>,
) -> Result<WalletRecord> {
    let mut record = mercuryrustlib::wallet::create_wallet(name, cc).await?;
    if let Some(m) = mnemonic {
        // Same settings/derivation, caller's seed.
        record.mnemonic = m.to_string();
    }
    Ok(record)
}

#[cfg(test)]
mod identity_tests {
    use super::SparkWallet;
    use bitcoin::secp256k1::{KeyPair, Message, Secp256k1};
    use sha2::{Digest, Sha256};

    // REQ (message signing): a Schnorr signature over sha256(msg) validates against the signer's
    // compressed pubkey, and a tampered message does not.
    #[test]
    fn sign_validate_roundtrip() {
        let secp = Secp256k1::new();
        let sk = bitcoin::secp256k1::SecretKey::from_slice(&[0x42u8; 32]).unwrap();
        let kp = KeyPair::from_secret_key(&secp, &sk);
        let pk = bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &sk);
        let pk_hex = hex::encode(pk.serialize());

        let msg = b"spark parity attestation";
        let digest = Sha256::digest(msg);
        let m = Message::from_slice(&digest).unwrap();
        let sig = secp.sign_schnorr_no_aux_rand(&m, &kp);
        let sig_hex = hex::encode(sig.as_ref());

        assert!(SparkWallet::validate_message_with_identity_key(msg, &sig_hex, &pk_hex).unwrap());
        // tampered message fails
        assert!(!SparkWallet::validate_message_with_identity_key(b"other message", &sig_hex, &pk_hex).unwrap());
    }
}
