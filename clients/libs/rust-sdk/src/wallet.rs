use std::sync::Arc;

use anyhow::{anyhow, Result};
use mercurylib::wallet::{Coin, CoinStatus, Wallet as WalletRecord};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::sqlite_manager::{get_wallet, insert_wallet, update_wallet};
use tokio::sync::{broadcast, Mutex};

use crate::config::SdkConfig;
use crate::events::{LadderSkipReason, WalletEvent, WatchtowerPass};
use crate::types::{Balance, ClaimResult, DepositAddressInfo, SdkError, TokenClaimState, TokenClaimStatus};

/// A complete off-line recovery bundle for a wallet (review H3). Contains everything that lives
/// ONLY on the owner's disk and that the SE cannot re-serve after a claim: the full wallet record
/// (mnemonic + coins + activity), every backup row (the pre-signed exit ladder, the off-chain
/// `branch-*` exit branches, and the `parents-*` terminal-ancestor lists), and the RGB engine seed.
/// NOTE: the RGB *stash* (contracts/consignments under `rgb_data_dir`) is NOT embedded — copy that
/// directory too; from re-obtainable consignments the stash can be rebuilt, but not from the seed
/// alone.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RecoveryBundle {
    pub version: u32,
    pub wallet_name: String,
    pub wallet: WalletRecord,
    /// (statechain_id-or-pseudo-key, raw txs JSON) for every backup row.
    pub backups: Vec<(String, String)>,
    /// The RGB engine's BIP39 seed (from `rgb_data_dir/rgb.mnemonic`), if the wallet uses tokens.
    pub rgb_mnemonic: Option<String>,
    pub notes: String,
}

/// A deadline-critical background pass that could not SEE what it needed to see, retained on the
/// wallet until a later pass of the same kind succeeds (external review F3).
///
/// A background watcher must not die on a transient fault — but it must not look healthy while
/// protecting nothing either. So each failing pass emits [`WalletEvent::WatchtowerBlind`] AND leaves
/// this record behind; [`UtexoWallet::watchtower_faults`] reads it back on demand, so an app that
/// subscribed late (or does not subscribe at all) can still answer "is my protection running?".
#[derive(Clone, Debug)]
pub struct WatchtowerFault {
    /// Which pass is blind.
    pub pass: WatchtowerPass,
    /// The most recent underlying error.
    pub detail: String,
    /// How many consecutive passes of this kind have failed. `1` is a blip; a growing number on a
    /// wallet holding received off-chain coins is an incident.
    pub consecutive_failures: u32,
    /// Unix seconds of the FIRST failure in this run (i.e. how long the wallet has been blind).
    pub since_unix: u64,
    /// Unix seconds of the most recent failure.
    pub last_unix: u64,
}

pub(crate) struct Inner {
    pub cc: ClientConfig,
    pub config: SdkConfig,
    pub events_tx: broadcast::Sender<WalletEvent>,
    /// Retained "this pass is blind" state, keyed by pass. Empty = every pass that has run, ran with
    /// full visibility. See [`WatchtowerFault`].
    pub watchtower_faults: Mutex<std::collections::BTreeMap<WatchtowerPass, WatchtowerFault>>,
    /// Chain height at which [`UtexoWallet::defend_ladders`] last ran from the background loop. The
    /// TES-R defence is a PER-BLOCK reaction (tier CSVs only mature on a new block), so the loop
    /// gates on this instead of hammering the chain backend every `poll_interval_secs`.
    pub last_defended_height: Mutex<Option<u32>>,
    /// Pre-paid ONBOARDING deposit-token ids, consumed one per fresh on-chain slot (deposit
    /// address, issuance carrier). Derived slots (split/combine/refresh outputs) never draw on
    /// this pool — they use free SE-minted derived tokens (`take_derived_tokens`).
    pub token_pool: Mutex<Vec<String>>,
    /// Guards wallet-record read-modify-write cycles within this process.
    pub wallet_lock: Mutex<()>,
    /// Lazily-opened RGB engine (token support); None until first token operation.
    pub rgb: Mutex<Option<mercury_rgb::RgbWallet>>,
}

/// Utexo wallet (Spark-compatible API) on Mercury+RGB. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct UtexoWallet {
    pub(crate) inner: Arc<Inner>,
}

impl UtexoWallet {
    /// Create or load a wallet. Returns the wallet and its mnemonic.
    ///
    /// ⚠️ BACKUP: the mnemonic ALONE is **not** a sufficient backup (review H3). It restores the
    /// wallet's key hierarchy, but NOT the per-coin exit material that only the owner holds and the
    /// SE cannot re-serve after a claim: the pre-signed backup ladder, the off-chain exit branches
    /// (`branch-*`), the terminal-ancestor lists (`parents-*`), and — for token wallets — the entire
    /// RGB stash (under a SEPARATE `rgb.mnemonic` inside `rgb_data_dir`). Losing `wallet.db` (or
    /// `rgb_data_dir`) is **total loss** of every off-chain coin and token, even with the mnemonic.
    /// Back up the whole recovery bundle with [`Self::export_recovery_bundle`] and restore it with
    /// [`Self::import_recovery_bundle`]; also copy `rgb_data_dir` for token wallets.
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
        let wallet = UtexoWallet {
            inner: Arc::new(Inner {
                cc,
                config,
                events_tx,
                token_pool: Mutex::new(Vec::new()),
                wallet_lock: Mutex::new(()),
                rgb: Mutex::new(None),
                watchtower_faults: Mutex::new(std::collections::BTreeMap::new()),
                last_defended_height: Mutex::new(None),
            }),
        };
        Ok((wallet, mnemonic_out))
    }

    /// Export a full [`RecoveryBundle`] as JSON (review H3): the wallet record, every backup row
    /// (exit ladder + `branch-*` + `parents-*`), and the RGB engine seed. This — plus a copy of
    /// `rgb_data_dir` for token wallets — is the ONLY complete backup; the mnemonic alone is not.
    /// Store it securely: it contains the wallet seed. Re-export after any transfer/claim/split,
    /// since those mint new coins and exit material.
    pub async fn export_recovery_bundle(&self) -> Result<String> {
        // Audit [27]: hold the wallet lock across BOTH reads so the coin record and the backup rows
        // are one consistent snapshot. Without it, a concurrent background claim/split (which writes
        // the record before the backup rows in register_split_subcoins) could yield a torn bundle
        // that restores a coin with no exit material — silent unrecoverable off-chain funds.
        let _guard = self.inner.wallet_lock.lock().await;
        let record = self.record().await?;
        let backups = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await?;
        let rgb_mnemonic = self.inner.config.rgb_data_dir.as_ref().and_then(|dir| {
            std::fs::read_to_string(std::path::Path::new(dir).join("rgb.mnemonic"))
                .ok()
                .map(|s| s.trim().to_string())
        });
        let bundle = RecoveryBundle {
            version: 1,
            wallet_name: self.inner.config.wallet_name.clone(),
            wallet: record,
            backups,
            rgb_mnemonic,
            notes: "Complete recovery bundle. For token wallets also copy the entire rgb_data_dir \
                    (the RGB stash of contracts/consignments is NOT embedded here). Re-export after \
                    every transfer/claim/split."
                .to_string(),
        };
        Ok(serde_json::to_string_pretty(&bundle)?)
    }

    /// Restore a wallet from a [`RecoveryBundle`] JSON produced by [`Self::export_recovery_bundle`]
    /// into a FRESH database (`config.database_file`). Recreates the wallet record and all backup
    /// rows, and re-seeds the RGB engine (`rgb.mnemonic`) if `config.rgb_data_dir` is set — remember
    /// to also restore the rgb_data_dir stash contents for token balances. Fails if a wallet of the
    /// same name already exists in the target database.
    pub async fn import_recovery_bundle(config: SdkConfig, bundle_json: &str) -> Result<(Self, String)> {
        let bundle: RecoveryBundle = serde_json::from_str(bundle_json)?;
        let cc = ClientConfig::from_params(
            config.statechain_entity_url.clone(),
            config.electrum_url.clone(),
            config.electrum_type.clone(),
            config.network,
            config.database_file.clone(),
            config.confirmation_target,
        )
        .await?;

        if get_wallet(&cc.pool, &config.wallet_name).await.is_ok() {
            return Err(anyhow!(
                "wallet '{}' already exists in the target database; import into a fresh database_file",
                config.wallet_name
            ));
        }

        // Restore the wallet record under the target name.
        let mut record = bundle.wallet;
        record.name = config.wallet_name.clone();
        let mnemonic_out = record.mnemonic.clone();
        insert_wallet(&cc.pool, &record).await?;

        // Restore every backup row (exit ladder, branch-*, parents-*).
        for (key, txs_json) in &bundle.backups {
            mercuryrustlib::sqlite_manager::insert_raw_backup_txs(
                &cc.pool,
                &config.wallet_name,
                key,
                txs_json,
            )
            .await?;
        }

        // Re-seed the RGB engine (stash contents must be restored separately by copying rgb_data_dir).
        if let (Some(dir), Some(m)) = (config.rgb_data_dir.as_ref(), bundle.rgb_mnemonic.as_ref()) {
            std::fs::create_dir_all(dir)?;
            let p = std::path::Path::new(dir).join("rgb.mnemonic");
            if !p.exists() {
                std::fs::write(p, m)?;
            }
        }

        let (events_tx, _) = broadcast::channel(256);
        let wallet = UtexoWallet {
            inner: Arc::new(Inner {
                cc,
                config,
                events_tx,
                token_pool: Mutex::new(Vec::new()),
                wallet_lock: Mutex::new(()),
                rgb: Mutex::new(None),
                watchtower_faults: Mutex::new(std::collections::BTreeMap::new()),
                last_defended_height: Mutex::new(None),
            }),
        };
        Ok((wallet, mnemonic_out))
    }

    /// Subscribe to wallet events (deposits confirmed, transfers claimed, balance updates).
    pub fn subscribe(&self) -> broadcast::Receiver<WalletEvent> {
        self.inner.events_tx.subscribe()
    }

    /// Every deadline-critical pass that is currently BLIND, newest state per pass (external review
    /// F3). Empty = every pass that has run, ran with full visibility.
    ///
    /// This is the retained half of the fail-loud contract: a background watcher must survive a
    /// transient fault, so it keeps looping — but it emits [`WalletEvent::WatchtowerBlind`] on every
    /// failing pass AND leaves a [`WatchtowerFault`] here, so an app that never subscribed (or
    /// subscribed after the fact) can still tell "nothing to do" from "I could not see". Poll this
    /// next to `get_balance` and alert on a non-empty result: while it is non-empty, nothing is
    /// racing a clawback or a hostile trigger on this wallet's behalf.
    pub async fn watchtower_faults(&self) -> Vec<WatchtowerFault> {
        self.inner.watchtower_faults.lock().await.values().cloned().collect()
    }

    /// `true` iff any deadline-critical pass is currently blind. Convenience over
    /// [`Self::watchtower_faults`].
    pub async fn is_watchtower_blind(&self) -> bool {
        !self.inner.watchtower_faults.lock().await.is_empty()
    }

    /// Record (or extend) a blind pass: emit [`WalletEvent::WatchtowerBlind`] and retain the state.
    /// Called by the pass itself, so a DIRECT caller of `auto_exit_due` / `defend_ladders` gets the
    /// same signal as the background loop.
    pub(crate) async fn note_watchtower_blind(&self, pass: WatchtowerPass, detail: String) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        {
            let mut faults = self.inner.watchtower_faults.lock().await;
            let entry = faults.entry(pass).or_insert_with(|| WatchtowerFault {
                pass,
                detail: detail.clone(),
                consecutive_failures: 0,
                since_unix: now,
                last_unix: now,
            });
            entry.detail = detail.clone();
            entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
            entry.last_unix = now;
        }
        // Emitted on EVERY failing pass, not only on the transition: a subscriber that starts while
        // the wallet is already blind must not have to wait for a recovery to learn about it.
        let _ = self
            .inner
            .events_tx
            .send(WalletEvent::WatchtowerBlind { pass, detail });
    }

    /// Clear the retained blind state for a pass that has now run with full visibility.
    pub(crate) async fn note_watchtower_ok(&self, pass: WatchtowerPass) {
        self.inner.watchtower_faults.lock().await.remove(&pass);
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
    pub async fn get_utexo_address(&self) -> Result<String> {
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
        // Fail CLOSED for token wallets (audit [23]): if RGB state is unavailable we cannot know
        // which coins are carriers, and silently counting a carrier's sats as spendable BTC would
        // invite an RGB-destroying spend.
        //
        // [F3] The non-token arm used to swallow the error with `unwrap_or_default()`. It is
        // infallible TODAY (both `token_carrier_outpoints` and `consignment_bearing_outpoints`
        // return early with an empty set when the RGB config is absent) — which is exactly why
        // swallowing it bought nothing and hid everything: the day either helper grows a fallible
        // step before that early return, an empty carrier set silently becomes "no carriers" and a
        // carrier's sats are reported as spendable BTC. Propagate from both arms.
        let carriers = self.unspendable_as_btc_outpoints().await.map_err(|e| {
            anyhow!("cannot compute balance: RGB token-carrier state is unavailable ({e}) — a carrier's sats must never be reported as spendable BTC")
        })?;
        let mut balance = compute_balance_excluding(&record, &carriers);
        // [HIGH] ...and then the very next line swallowed the RGB engine. `unwrap_or_default()` here
        // made an unreadable stash report `tokens: []`, i.e. "you hold no tokens" — the same benign
        // empty result the argument four lines above rejects for the sats side, and a strictly worse
        // one: an owner reading a zero token balance has no reason to run the near-deadline
        // materialisation, and the ~6.9-day clawback deadline on a received carrier keeps running.
        // If the carrier enumeration succeeded but the per-asset balances cannot be read, say so.
        balance.tokens = self.get_token_balances().await.map_err(|e| {
            anyhow!("cannot compute balance: RGB token balances are unavailable ({e}) — reporting an empty token list would tell the owner they hold nothing while a received carrier's clawback deadline keeps running")
        })?;
        Ok(balance)
    }

    /// Provide a pre-paid, confirmed deposit-token id. Each fresh ON-CHAIN onboarding slot (a
    /// deposit address, a token issuance carrier) consumes one pooled token. Slots minted by
    /// SE-co-signed flows over an existing statechain — split outputs, combine outputs, refresh
    /// re-anchors — do NOT draw on the pool: they use free DERIVED tokens vouched by the parent
    /// coin ([`Self::take_derived_tokens`]). Without pooled tokens the SDK requests one from the
    /// SE and surfaces `SdkError::TokenPaymentRequired` if it is not free.
    pub async fn add_prepaid_token(&self, token_id: &str) {
        self.inner.token_pool.lock().await.push(token_id.to_string());
    }

    /// Take a usable ONBOARDING token id: pooled first, then config default, then ask the SE.
    /// For a derived slot (split/combine/refresh) use [`Self::take_derived_tokens`] instead —
    /// onboarding tokens cost real money in a paid deployment.
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

    /// Take `n` DERIVED-slot token ids vouched by `parent_statechain_id` — the free tokens the SE
    /// mints for slots created by co-signed flows over an existing statechain (split piece/change,
    /// combine outputs, a refresh re-anchor). Never draws on the prepaid pool (that is onboarding
    /// money: with a paid token server a 2-output split would otherwise cost 2× the onboarding fee
    /// for zero new on-chain surface). One SE round-trip authorizes the whole batch (a single
    /// consumed auth nonce).
    ///
    /// Falls back to onboarding tokens ([`Self::take_token`]) ONLY when the SE genuinely does not
    /// offer derived issuance — the endpoint is absent (old server) or the operator disabled it
    /// (`get_derived_tokens` → `Ok(None)`). A TRANSIENT or refused failure (proxy/DB down, auth
    /// error, or the parent's lifetime allowance exhausted) is SURFACED, never silently paid for:
    /// external review finding 4 flagged that the old blanket fallback would quietly spend prepaid
    /// onboarding tokens (real money in a paid deployment) on a momentary derived-endpoint blip. A
    /// caller that truly wants to pay can catch the error and call [`Self::take_token`] itself.
    pub(crate) async fn take_derived_tokens(
        &self,
        parent_statechain_id: &str,
        n: usize,
    ) -> Result<Vec<String>> {
        let record = self.record().await?;
        let parent = record
            .coins
            .iter()
            .find(|c| {
                c.statechain_id.as_deref() == Some(parent_statechain_id) && c.duplicate_index == 0
            })
            .cloned();
        drop(record);
        // No parent coin locally (shouldn't happen on a derived path) — fall back to onboarding.
        let Some(parent) = parent else {
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(self.take_token().await?);
            }
            return Ok(out);
        };
        match mercuryrustlib::deposit::get_derived_tokens(
            &self.inner.cc,
            &parent,
            parent_statechain_id,
            u32::try_from(n)?,
        )
        .await?
        {
            // Issued — free, vouched by the parent.
            Some(ids) => Ok(ids),
            // Server does not offer derived issuance at all: legacy onboarding path (free in
            // no-server mode; a paid deployment surfaces `TokenPaymentRequired` from take_token).
            None => {
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.take_token().await?);
                }
                Ok(out)
            }
        }
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

        // Auto-establish the TES-R exit ladder — UNCONDITIONAL: every CONFIRMED coin is laddered
        // unless it falls in one of the three narrow classes that structurally cannot be
        // (RGB carrier / [B0] un-broadcast funding / legacy no-aggregate). There is one protocol.
        // The exit payee is the coin's seed-derived `backup_address` (recoverable from the
        // mnemonic), NEVER an out-of-wallet address. Idempotent: a coin that already has a ladder
        // is left alone.
        //
        // Every skip is now RECORDED (`ladderskip-<sid>` in the wallet DB) and SURFACED
        // (`WalletEvent::LadderSkipped`) — a review flagged the previous silence as a UX defect,
        // since the owner only discovered a flat-only coin at transfer time. The record is also
        // load-bearing for the conveyance path: `transfer_sender::assert_flat_conveyance_is_legitimate`
        // reads it back to tell a legitimately-flat coin from an unexplained one, because that crate
        // cannot see RGB state.
        {
            let mut rec = self.record().await?;
            let network = rec.network.clone();
            // Mark the wallet as ladder-managed: from here on, an unexplained un-laddered coin in
            // this wallet is a bug and the flat conveyance path refuses it.
            //
            // NOT best-effort. This write used to be `let _ = ...`, which made a failed insert a
            // silent, permanent global off-switch for the conveyance-path classifier. The classifier
            // no longer depends on this row alone (it arms itself from any ladder artefact — see
            // `transfer_sender::wallet_is_provably_pre_sdk`), but a wallet DB that cannot take this
            // write is a fault the caller must see, not one to paper over.
            mercuryrustlib::transfer_sender::mark_ladder_managed(
                &self.inner.cc,
                &self.inner.config.wallet_name,
            )
            .await
            .map_err(|e| anyhow!("could not mark the wallet ladder-managed: {e}"))?;
            // Terminal-freeze invariant (PROTOCOL.md §5.10, rule 1): RGB rides the signed-once colored
            // carrier model and is NEVER anchored on the renewable T/X/S ladder — a plain tier spend
            // would destroy the allocation. So an RGB carrier must not get a ladder. `single_use`
            // catches terminalized/combine carriers, but a resting issuance/received carrier holding
            // an allocation need not be single_use, so we also exclude the RGB carrier set. Fail CLOSED
            // for a token wallet whose RGB state is momentarily unavailable: skip establishing this
            // pass (leave coins un-laddered) rather than risk laddering a carrier — next claim() retries.
            // AUDITED-SWALLOW: fails toward LESS work but MORE safety — `None` means "RGB state
            // unavailable", and the only thing skipped is ESTABLISHING a ladder. An un-laddered coin
            // keeps its absolute-locktime backup and stays exitable, whereas laddering a carrier
            // would destroy its RGB allocation. It is also surfaced (`note_flat` +
            // `LadderSkipped{RgbStateUnavailable}`) and retried on the next claim(), so it is not
            // silent. Propagating instead would abort the whole claim pass over a transient blip.
            let carriers = if self.inner.config.rgb_data_dir.is_some()
                && self.inner.config.rgb_proxy_url.is_some()
            {
                self.unspendable_as_btc_outpoints().await.map(Some).unwrap_or(None)
            } else {
                Some(std::collections::HashSet::new())
            };
            // `None` = a token wallet whose RGB state is momentarily unavailable → skip establishing
            // this pass rather than risk laddering a carrier; the next claim() retries. Unlike the
            // old blanket `break`, the affected coins are now recorded + surfaced so the app knows
            // the whole wallet is flat-only this pass.
            let rgb_state_unavailable = carriers.is_none();
            // AUDITED-SWALLOW: the `None` case is already captured in `rgb_state_unavailable` above
            // and drives the `note_flat` below; this default only supplies an unused empty set.
            let carriers = carriers.unwrap_or_default();
            for coin in rec.coins.iter_mut() {
                let sid = match &coin.statechain_id { Some(s) => s.clone(), None => continue };
                // A coin that is not CONFIRMED is not "flat-only", it is simply not ready: the
                // deposit may still be confirming, or the coin is mid-transfer/withdrawing (where a
                // fresh SE co-sign would inflate `num_sigs` against an open transfer). Not recorded,
                // not surfaced — nothing has been decided about it.
                if coin.status != CoinStatus::CONFIRMED {
                    continue;
                }
                let dup = coin.duplicate_index;
                if dup != 0 {
                    // Keyed apart (`ladderskip-<sid>#<n>`), so this never touches the index-0 coin's
                    // record — and the sid's ladder belongs to the index-0 coin, so an existing
                    // ladder says nothing about the duplicate.
                    self.note_flat(&sid, dup, LadderSkipReason::DuplicateDeposit).await?;
                    continue;
                }
                // [M2/M3] Read the coin's ladder row FIRST — before the carrier tests and before the
                // electrum round-trip below.
                //
                // [M3] ORDER IS LOAD-BEARING FOR ACCURACY. The carrier / `single_use` /
                // `rgb-state-unavailable` arms used to run BEFORE this probe, so an ALREADY-LADDERED
                // coin that happens to be (or to look like) a carrier got a `ladderskip-` record
                // written for it — and `ladder_skip_reason` / `flat_only_coins` then reported a
                // laddered coin as flat-only, which is simply false. A coin that HAS a ladder is not
                // on the flat lane by any reading, so nothing is recorded for it at all.
                //
                // [M2] It also means the recorded reason describes the coin's ACTUAL blocker:
                // deciding `funding-not-onchain` ahead of this would let a coin with an unreadable
                // `tesr-` row be recorded under a reason that LICENSES flat conveyance. And it is
                // the cheaper test: an already-laddered coin costs one DB read, not a chain query.
                match mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, &sid).await {
                    // Already established — idempotent. Drop any stale "left flat" record so it can
                    // never later excuse a flat conveyance of a coin that now HAS a ladder.
                    Ok(Some(_)) => {
                        self.clear_flat_note(&sid).await?;
                        continue;
                    }
                    Ok(None) => {}
                    // [M2] A bundle row we cannot READ. Do not overwrite it by establishing a second
                    // ladder — but DO record and surface it, because this is the single most
                    // important flat-only case to tell the owner about: the conveyance path refuses
                    // such a coin outright (`transfer_sender::execute`'s `tesr::load` Err arm), so
                    // the coin is untransferable until the row is restored, and it used to reach
                    // that refusal with no record and no event whatsoever. The coin stays
                    // withdrawable and unilaterally exitable throughout.
                    // AUDITED-SWALLOW: fails toward MORE safety and is LOUD — an unreadable ladder
                    // row must not be overwritten by establishing a second ladder, and the coin is
                    // recorded + evented as flat-only (`LadderUnreadable`) rather than dropped. Note
                    // `note_flat` itself is `?`, so the RECORDING of this cannot be swallowed.
                    Err(_) => {
                        self.note_flat(&sid, 0, LadderSkipReason::LadderUnreadable).await?;
                        continue;
                    }
                }
                if rgb_state_unavailable {
                    self.note_flat(&sid, 0, LadderSkipReason::RgbStateUnavailable).await?;
                    continue;
                }
                // `single_use` marks a terminalized/combine carrier. It has no production setter
                // today, but it is NOT redundant: in a wallet with no RGB config the carrier set is
                // empty by construction, so dropping this flag would newly ladder such a coin —
                // a fail-OPEN on the terminal-freeze rule. Kept as part of the carrier exclusion.
                if coin.single_use {
                    self.note_flat(&sid, 0, LadderSkipReason::TerminalizedCarrier).await?;
                    continue;
                }
                if is_token_carrier(coin, &carriers) {
                    self.note_flat(&sid, 0, LadderSkipReason::RgbCarrier).await?;
                    continue;
                }
                // ROOT-ONLY [B0]: only a coin whose funding `F` is ON-CHAIN may be laddered. A split
                // SUB-COIN's F is an un-broadcast split output, so its ladder's trigger has no prevout
                // to spend: `unilateral_exit`'s laddered branch takes `exit_pass` (skipping the branch
                // materialisation), the trigger broadcast fails, and the coin stalls forever reporting
                // `wait_blocks: 0` — effectively unexitable via the SDK. Test the property that actually
                // matters rather than a proxy: F must be a known on-chain tx. Fail CLOSED — if electrum
                // cannot confirm F, skip this pass (a missed ladder is harmless and the next claim()
                // retries; a laddered sub-coin is not). Laddering a sub-coin properly is the in-ladder
                // split's job (`in_ladder_pay`: `establish_child` gives each split child its own two-tier
                // ladder, Model A on the piece), never this pass's.
                //
                // The on-chain funding OUTPUT (not merely "is F on chain") is also what [R5] below
                // needs, so read it once and keep the scriptPubKey.
                let f_spk_hex = {
                    use electrum_client::ElectrumApi;
                    let tx0 = coin
                        .utxo_txid
                        .as_ref()
                        .and_then(|t| t.parse::<bitcoin::Txid>().ok())
                        .and_then(|txid| self.inner.cc.electrum_client.transaction_get(&txid).ok());
                    match (tx0, coin.utxo_vout) {
                        (Some(tx0), Some(vout)) => tx0
                            .output
                            .get(vout as usize)
                            .map(|o| hex::encode(o.script_pubkey.as_bytes())),
                        _ => None,
                    }
                };
                let Some(f_spk_hex) = f_spk_hex else {
                    // [B1, recording side] `funding-not-onchain` is a PERMANENT reason and therefore
                    // LICENSES a flat conveyance — so it may only be recorded when the coin is
                    // POSITIVELY known to be an off-chain sub-coin. The `None` above does not prove
                    // that: an electrum fault produces exactly the same value as a genuinely
                    // un-broadcast `F`. Writing the permanent reason on a transient fault would
                    // re-create the very hole [B1] closes, one level up — a blip during claim()
                    // would license that coin to convey flat forever after. So prove it from the
                    // coin's own exit material (a `branch-<id>` chain or a `ctesr-<id>` child
                    // bundle), and otherwise record a TRANSIENT reason that licenses nothing.
                    let reason = if self.has_offchain_funding_row(&sid).await {
                        LadderSkipReason::FundingNotOnChain
                    } else {
                        LadderSkipReason::FundingUnresolvable
                    };
                    self.note_flat(&sid, 0, reason).await?;
                    continue;
                };
                // [R5] BINDABILITY GATE. A receiver accepts a conveyed ladder only through
                // `verify_bundle_bound`, which fails CLOSED unless the COORDINATOR has an aggregate on
                // record for the sid and that aggregate is the key controlling `F`. The aggregate column
                // arrived with migration 0009 and was backfilled forward only, so pre-0009 rows are NULL
                // — and those coins are otherwise ordinary confirmed on-chain roots that sail through
                // every test above. Laddering one produces a bundle no receiver can bind. The
                // acceptance-path rule is right and stays untouched (relaxing it is exactly the C-1
                // decoy-ladder hole); what must change is that we never build a ladder that cannot be
                // bound. Fail CLOSED here too — a coordinator we cannot reach, or a record we cannot
                // read, means we do not know the coin is bindable, so we do not ladder it this pass.
                //
                // ⚠️ HONEST SCOPE: skipping does NOT restore transferability, but not for the reason an
                // earlier draft of this comment gave. The claim path's flat lane still ACCEPTS
                // `protocol_version < 2` — the unconditional floor was implemented, shown to break
                // sdk41, and deliberately narrowed to a version/payload consistency check (a sender may
                // not ship ladder material while asking to be judged by pre-ladder rules). So a legacy
                // no-aggregate coin conveyed as version 0 still claims today. What it cannot do is carry
                // a BOUND ladder, because there is no coordinator aggregate to bind against. Its value is
                // still recoverable (on-chain withdrawal and its signed-once backup exit are untouched),
                // but it cannot move off-chain until the COORDINATOR backfills `aggregate_xonly` for the
                // legacy rows from its own columns (`x_only(user_public_key + server_public_key)` — the
                // same value deposit-init records today, no client input). That is the completing fix
                // and it lives server-side.
                //
                // What skipping buys here: establishing a ladder co-signs three tiers through the SE,
                // which is IRREVERSIBLE — it burns signature budget and permanently raises the sid's
                // `num_sigs` — on a coin that gains nothing, and it persists an unclaimable bundle that
                // would later be conveyed as authentic. And it is self-healing: once the coordinator
                // backfills, the next claim() pass ladders the coin with no client change.
                let se_agg = match mercuryrustlib::utils::get_statechain_info(&sid, &self.inner.cc).await {
                    Ok(Some(info)) => info.aggregate_pubkey.clone(),
                    // Coordinator unreachable / no record → not known bindable → skip. Recorded as
                    // its own reason, distinct from `NotBindable`: "we could not decide" must never
                    // be read back as "flat is fine for this coin", so the conveyance path treats
                    // this record as a REFUSAL, not a licence.
                    _ => {
                        self.note_flat(&sid, 0, LadderSkipReason::CoordinatorUnavailable).await?;
                        continue;
                    }
                };
                // TYPED CAUSE, not `.is_err()`. Only `NoCoordinatorAggregate` is the permanent,
                // harmless pre-0009 explanation that the conveyance path may later license; a
                // non-taproot funding output, an spk that will not decode, or an aggregate that does
                // not control `F` (a decoy-shaped coin) are DIFFERENT facts and must not be recorded
                // under the licensing spelling. The blanket `is_err()` here — mirroring the one in
                // the classifier — was one of the fail-opens this pass removes.
                if let Err(e) = mercuryrustlib::tesr::ladder_binding_precheck_cause(
                    &sid,
                    &f_spk_hex,
                    se_agg.as_deref(),
                    &network,
                ) {
                    let reason = if e.cause
                        == mercuryrustlib::tesr::BindingRefusal::NoCoordinatorAggregate
                    {
                        LadderSkipReason::NotBindable
                    } else {
                        LadderSkipReason::BindingUnresolved
                    };
                    self.note_flat(&sid, 0, reason).await?;
                    continue;
                }
                let payee = coin.backup_address.clone();
                match mercuryrustlib::tesr::establish_auto(&self.inner.cc, coin, &payee, &network).await {
                    Ok(bundle) => {
                        if mercuryrustlib::tesr::persist(&self.inner.cc, &self.inner.config.wallet_name, &bundle).await.is_ok() {
                            self.clear_flat_note(&sid).await?;
                            let _ = self.inner.events_tx.send(WalletEvent::LadderEstablished { statechain_id: sid });
                        } else {
                            // Co-signed but not persisted — the coin is flat-only until a retry.
                            self.note_flat(&sid, 0, LadderSkipReason::EstablishFailed).await?;
                        }
                    }
                    // Leave un-laddered: its tx1 backup still exits, and the next pass retries. The
                    // owner is told, because a coin that keeps failing here is flat-only in practice.
                    Err(_) => {
                        self.note_flat(&sid, 0, LadderSkipReason::EstablishFailed).await?;
                    }
                }
            }
        }
        // Per-statechain token outcomes for this pass (external review finding 5). Keyed so a coin
        // touched by both the freshly-claimed loop and the retry rescan is recorded once.
        let mut token_status: std::collections::HashMap<String, TokenClaimStatus> =
            std::collections::HashMap::new();
        if !receive.received_statechain_ids.is_empty() {
            let _ = self.inner.events_tx.send(WalletEvent::TransferClaimed {
                statechain_ids: receive.received_statechain_ids.clone(),
            });
            // Token hook: coins that arrived with a consignment get validated + booked.
            for id in &receive.received_statechain_ids {
                self.book_incoming_token(id, &mut token_status).await;
            }
        }

        // Retriable token booking (audit [8], review H6): a transient RGB-proxy/indexer error during
        // the claim pass above leaves the Mercury coin CONFIRMED but its token allocation unbooked and
        // permanently invisible. On EVERY claim pass, rescan CONFIRMED coins that carry a consignment
        // but have no booked allocation and re-run accept_incoming_tokens (idempotent). Because the
        // background watcher calls claim() on an interval, this is the retry loop.
        if self.inner.config.rgb_data_dir.is_some() && self.inner.config.rgb_proxy_url.is_some() {
            // ⚠️ AUDITED-SWALLOW, DELIBERATELY LEFT IN PLACE (external review, this exact class).
            // This is the ONE `unwrap_or_default()` on a carrier set in this file that fails toward
            // MORE work rather than less, so it is safe and must NOT be "fixed" to match its
            // neighbours:
            //
            //   `booked` is used ONLY at the `continue` below, as a "this coin is already booked,
            //   skip it" filter. An empty set therefore makes the filter match NOTHING, so EVERY
            //   confirmed coin is re-run through `book_incoming_token` — which is idempotent (that
            //   is this rescan's whole premise: it exists to retry transient RGB faults). The
            //   failure mode is redundant re-booking attempts, i.e. wasted work, never a skipped
            //   coin.
            //
            // Contrast `auto_exit_due` / `get_balance`, where the same defaulted-empty set means
            // "no carriers exist" and DISARMS a protection. Direction, not spelling, is what makes
            // a swallow a bug. (The enclosing `claim()` still returns Ok here on purpose: a broken
            // RGB engine must not stop plain-BTC coins from being claimed; the resulting per-coin
            // outcomes are reported in `ClaimResult::token_results` as Pending/Rejected.)
            // AUDITED-SWALLOW: fails toward MORE work (argument above) — never toward less.
            let booked = self.token_carrier_outpoints().await.unwrap_or_default();
            for coin in after
                .coins
                .iter()
                .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            {
                let Some(id) = coin.statechain_id.as_deref() else { continue };
                if token_status.contains_key(id) {
                    continue; // already attempted this pass
                }
                if coin_outpoint(coin).map_or(false, |o| booked.contains(&o)) {
                    continue; // already booked
                }
                self.book_incoming_token(id, &mut token_status).await;
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
            token_results: token_status.into_values().collect(),
        })
    }

    /// Record that a coin was left on the FLAT (un-laddered) lane, and tell the application — but
    /// only the FIRST time this reason applies. Both halves matter:
    ///   * the DB record is what `transfer_sender::assert_flat_conveyance_is_legitimate` reads back
    ///     to tell a legitimately-flat coin (carrier / [B0] / legacy) from an unexplained one, since
    ///     that crate cannot see RGB state;
    ///   * the event is the owner's advance notice that the coin is flat-only, instead of finding
    ///     out at transfer time.
    /// De-duplicated on the recorded reason so the polling background watcher does not spam it.
    ///
    /// **The write failure is NOT swallowed.** It used to end in `.unwrap_or(false)`, which meant a
    /// DB fault left a legitimately-flat carrier with no record and no event — and a carrier with no
    /// record cannot be conveyed at all, because the conveyance-path classifier has no way to see
    /// RGB state and depends on this row to explain the coin. Silently producing an untransferable
    /// coin is strictly worse than failing the `claim()` pass, which the caller can retry.
    async fn note_flat(
        &self,
        statechain_id: &str,
        duplicate_index: u32,
        reason: LadderSkipReason,
    ) -> Result<()> {
        let changed = mercuryrustlib::transfer_sender::record_ladder_skip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
            duplicate_index,
            reason.as_str(),
        )
        .await
        .map_err(|e| {
            anyhow!(
                "could not record why statechain id {statechain_id} was left on the flat lane \
                 (reason '{}'): {e}. Refusing to continue the ladder pass silently — without that \
                 record the coin cannot be conveyed at all.",
                reason.as_str()
            )
        })?;
        if changed {
            let _ = self.inner.events_tx.send(WalletEvent::LadderSkipped {
                statechain_id: statechain_id.to_string(),
                reason,
            });
        }
        Ok(())
    }

    /// [M3] Why this coin is **flat-only**, read back from the persisted record — `None` when the
    /// coin has no recorded reason (it is laddered, or no `claim()` pass has decided its lane yet).
    ///
    /// [`WalletEvent::LadderSkipped`] fires only on a TRANSITION, so an app that starts after the
    /// transition — a fresh process, a restored wallet, a UI opened later — would otherwise have NO
    /// way to learn a coin is flat-only until a transfer failed. This is that way, and it is
    /// SDK-level: the underlying record lives in mercuryrustlib and was not reachable from an SDK
    /// consumer at all.
    ///
    /// ⚠️ This returns `None` both when no record exists AND when the record carries a spelling this
    /// build does not know (a forward value written by a newer client). Use
    /// [`Self::ladder_skip_reason_raw`] where that difference matters — it never loses information.
    pub async fn ladder_skip_reason(&self, statechain_id: &str) -> Option<LadderSkipReason> {
        LadderSkipReason::from_str(&self.ladder_skip_reason_raw(statechain_id).await?)
    }

    /// The exact persisted spelling of this coin's flat-only reason (e.g. `"rgb-carrier"`), or
    /// `None` if none is recorded. Preferred over [`Self::ladder_skip_reason`] when the caller must
    /// not silently drop a reason spelling this build predates.
    pub async fn ladder_skip_reason_raw(&self, statechain_id: &str) -> Option<String> {
        mercuryrustlib::transfer_sender::read_ladder_skip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
            0,
        )
        .await
    }

    /// [M3] Every coin in this wallet currently recorded as flat-only, as
    /// `(statechain_id, raw_reason, may_still_be_transferred)`.
    ///
    /// The third element is the one an app actually needs: `true` means the reason is a PERMANENT,
    /// structural one that legitimises conveying the coin on the flat lane (an RGB carrier, an
    /// off-chain funding, a legacy no-aggregate coin), so the coin still transfers; `false` means
    /// the coin is currently NOT transferable and `send` will refuse it — run `claim()` again, and
    /// if the reason persists the coin needs operator attention. Either way the coin's value is
    /// unaffected: it remains withdrawable and unilaterally exitable.
    pub async fn flat_only_coins(&self) -> Result<Vec<(String, String, bool)>> {
        let rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        .map_err(|e| anyhow!("could not read the wallet's backup rows: {e}"))?;
        let mut out = Vec::new();
        for (key, json) in rows {
            // `ladderskip-<sid>` for the index-0 coin; `ladderskip-<sid>#<n>` for a duplicate.
            let Some(rest) = key.strip_prefix("ladderskip-") else { continue };
            if rest.contains('#') {
                continue;
            }
            let Some(reason) = serde_json::from_str::<serde_json::Value>(&json)
                .ok()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(|s| s.to_string()))
            else {
                // An unreadable record is itself a flat-only condition the conveyance path refuses;
                // report it rather than dropping the coin from the listing.
                out.push((rest.to_string(), String::new(), false));
                continue;
            };
            let transferable =
                mercuryrustlib::transfer_sender::is_legitimate_flat_reason(&reason);
            out.push((rest.to_string(), reason, transferable));
        }
        out.sort();
        Ok(out)
    }

    /// Does this coin carry POSITIVE evidence that its funding `F` is off chain — a `branch-<id>`
    /// exit chain (flat sub-coin) or a `ctesr-<id>` child bundle (in-ladder split child)? These are
    /// the same two witnesses `transfer_sender::assert_flat_conveyance_is_legitimate` accepts, so
    /// the recording side and the conveyance side agree on what "[B0] off-chain" means. A DB error
    /// reads as "no evidence", which is the conservative answer here: it downgrades a permanent,
    /// licensing reason to a transient one.
    async fn has_offchain_funding_row(&self, statechain_id: &str) -> bool {
        let Ok(rows) = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        else {
            return false;
        };
        let branch_key = format!("branch-{statechain_id}");
        let child_key = format!("ctesr-{statechain_id}");
        rows.iter().any(|(k, json)| {
            if *k == child_key {
                return true;
            }
            *k == branch_key
                && serde_json::from_str::<serde_json::Value>(json)
                    .ok()
                    .and_then(|v| v.as_array().map(|a| !a.is_empty()))
                    .unwrap_or(false)
        })
    }

    /// Drop a coin's "left flat" record — it now has a ladder, and a stale record must never later
    /// excuse conveying a laddered coin flat. The delete's failure is surfaced for the same reason
    /// [`Self::note_flat`]'s is: this record is load-bearing for the conveyance path, so a DB fault
    /// that leaves it wrong must be visible rather than inferred later from a puzzling refusal.
    async fn clear_flat_note(&self, statechain_id: &str) -> Result<()> {
        mercuryrustlib::transfer_sender::clear_ladder_skip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
            0,
        )
        .await
        .map_err(|e| {
            anyhow!("could not clear the flat-lane record of statechain id {statechain_id}: {e}")
        })
    }

    /// Attempt to book an incoming token consignment for one claimed coin, recording the outcome in
    /// `status` (external review finding 5). A `Ok(None)` (no consignment) records nothing — the coin
    /// is plain sats. A booked allocation emits `TokenTransferClaimed`. A `PERMANENT-INVALID` error
    /// writes a `token-rejected-<id>` marker so [`Self::consignment_bearing_outpoints`] stops
    /// quarantining the coin (a griefer must not lock a victim's sats forever with a garbage
    /// consignment). A transient error leaves the coin quarantined and marked `Pending` for retry.
    async fn book_incoming_token(
        &self,
        id: &str,
        status: &mut std::collections::HashMap<String, TokenClaimStatus>,
    ) {
        match self.accept_incoming_tokens(id).await {
            Ok(Some((asset_id, amount))) => {
                let _ = self.inner.events_tx.send(WalletEvent::TokenTransferClaimed {
                    asset_id: asset_id.clone(),
                    amount,
                    statechain_id: id.to_string(),
                });
                status.insert(id.to_string(), TokenClaimStatus {
                    statechain_id: id.to_string(),
                    state: TokenClaimState::Booked,
                    asset_id: Some(asset_id),
                    amount: Some(amount),
                    detail: None,
                });
            }
            Ok(None) => {} // no consignment on this coin — plain sats, nothing to record
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("PERMANENT-INVALID") {
                    // Un-quarantine: durably mark the coin so its sats become ordinary spendable BTC.
                    let marker = vec![mercurylib::wallet::BackupTx {
                        tx_n: 0,
                        tx: String::new(),
                        client_public_nonce: String::new(),
                        server_public_nonce: String::new(),
                        client_public_key: String::new(),
                        server_public_key: String::new(),
                        blinding_factor: String::new(),
                        rgb_consignment: None,
                        rgb_blinding: None,
                    }];
                    let _ = mercuryrustlib::sqlite_manager::insert_backup_txs(
                        &self.inner.cc.pool,
                        &self.inner.config.wallet_name,
                        &format!("token-rejected-{id}"),
                        &marker,
                    )
                    .await;
                    println!("token accept PERMANENTLY rejected for {id}: {msg}");
                    status.insert(id.to_string(), TokenClaimStatus {
                        statechain_id: id.to_string(),
                        state: TokenClaimState::Rejected,
                        asset_id: None,
                        amount: None,
                        detail: Some(msg),
                    });
                } else {
                    // Transient: coin stays quarantined from plain-BTC spends; retried next pass.
                    println!("token accept pending (transient) for {id}: {msg}");
                    status.insert(id.to_string(), TokenClaimStatus {
                        statechain_id: id.to_string(),
                        state: TokenClaimState::Pending,
                        asset_id: None,
                        amount: None,
                        detail: Some(msg),
                    });
                }
            }
        }
    }

    /// Start the background watcher (deposit confirmation + incoming-transfer auto-claim +
    /// **both** deadline-critical defences).
    /// Returns a handle; abort it to stop. Mirrors the Spark SDK's background stream + claim
    /// automation (poll-based — Mercury has no server push).
    ///
    /// Each pass runs, in order: [`Self::claim`], optional [`Self::auto_refresh_due`],
    /// [`Self::defend_ladders`] (once per new block — external review F2; it was previously spawned
    /// by NOTHING, so a hostile trigger on a laddered coin was never raced) and
    /// [`Self::auto_exit_due`]. A failing pass never kills the loop, but it is never silent either:
    /// it emits [`WalletEvent::WatchtowerBlind`] and retains a [`WatchtowerFault`] visible through
    /// [`Self::watchtower_faults`].
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        let wallet = self.clone();
        let interval = self.inner.config.poll_interval_secs.max(1);
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                // Routine background re-anchoring is OFF by default (B4 economics): refresh is folded
                // into `transfer` and paid on-demand as part of the payment fee, so a running wallet
                // never silently shrinks a balance in the background. Only run it here if the operator
                // explicitly opted in. Deadline safety for idle wallets is the `auto_exit` pass below.
                if wallet.inner.config.auto_refresh && wallet.inner.config.background_auto_refresh {
                    let _ = wallet
                        .auto_refresh_due(wallet.inner.config.auto_refresh_margin_blocks)
                        .await;
                }
                // [F2] TES-R ladder defence. UNCONDITIONAL — there is no opt-in and no config flag,
                // because there is no wallet that wants an un-defended ladder: if someone broadcasts
                // a hostile trigger on a laddered coin, this is the only thing that races it, and the
                // owner's adopted state carries the strictly-lowest CSV so it wins if (and only if)
                // it is actually broadcast. It used to be spawned by nothing at all — every shipped
                // wrapper started only claim() + auto_exit_due, so the defence existed as an API and
                // never as a running process.
                //
                // Gated to ONE pass per new block: a tier's relative-CSV can only mature on a block,
                // so a per-poll pass (5 s on regtest) would add two chain queries per laddered coin
                // for nothing. An un-triggered coin is a no-op either way (`watch_pass` returns
                // early while F is unspent). If the tip cannot be read, run anyway — being unsure is
                // a reason to defend, not to skip.
                // AUDITED-SWALLOW: fails toward MORE work — an unreadable tip yields `None`, and
                // the `None => true` arm below runs the ladder defence ANYWAY. Being unsure is a
                // reason to defend, not to skip.
                let tip_now = {
                    use electrum_client::ElectrumApi;
                    wallet
                        .inner
                        .cc
                        .electrum_client
                        .block_headers_subscribe_raw()
                        .ok()
                        .map(|h| h.height as u32)
                };
                let due = match tip_now {
                    Some(h) => *wallet.inner.last_defended_height.lock().await != Some(h),
                    None => true,
                };
                if due {
                    // Errors are already surfaced by `defend_ladders` itself (WatchtowerBlind + a
                    // retained WatchtowerFault); the loop must keep going either way.
                    let _ = wallet.defend_ladders().await;
                    if let Some(h) = tip_now {
                        *wallet.inner.last_defended_height.lock().await = Some(h);
                    }
                }
                // Watchtower pass: protect off-chain coins nearing their exit-race deadline —
                // force-exit plain sub-coins, MATERIALIZE token carriers (REQ-33). Default-on so an
                // idle receiver is protected without scheduling anything.
                if wallet.inner.config.auto_exit {
                    // Same contract: a failure here is loud (WatchtowerBlind + retained fault), not
                    // fatal to the loop.
                    let _ = wallet
                        .auto_exit_due(wallet.inner.config.auto_exit_margin_blocks)
                        .await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }

    /// Watchtower pass (audit [17], review L7/P2-2): protect any owned OFF-CHAIN coin whose
    /// exit-race deadline is within `margin_blocks` of the current tip, before an ancestor can
    /// broadcast a stale backup (the ONLY defence for an off-chain coin — its exit branch is
    /// locktime-free, so broadcasting it early is always safe and cheap). Two coin kinds, two
    /// mechanisms:
    ///
    /// - **Plain sub-coins** are force-EXITED via [`Self::unilateral_exit`] (branch + latest backup)
    ///   — emits [`WalletEvent::ExitDeadlineApproaching`].
    /// - **Received token carriers** cannot take the plain exit (an RGB-unaware sweep would destroy
    ///   the allocation), so they are instead MATERIALIZED — broadcasting *only* the exit branch
    ///   settles the RGB allocation on-chain and wins the clawback race, WITHOUT the sats-sweeping
    ///   backup. Emits [`WalletEvent::TokenCarrierMaterialized`]. An issued/flat carrier has no exit
    ///   branch (no deadline, no ancestor, no clawback risk) and is naturally skipped. This closes
    ///   the gap that `sdk32` surfaced: before, `auto_exit_due` skipped carriers entirely, leaving a
    ///   received token with no automatic clawback protection.
    ///
    /// The deadline is the deposit-anchored `exit_deadline_block` from `estimate_exit_cost`
    /// (audit [10]). Returns the statechain_ids acted on. Call on an interval from your own loop (or
    /// alongside `claim()`), especially for wallets holding received off-chain coins/tokens — an
    /// offline owner is otherwise exposed to clawback.
    ///
    /// **Fails CLOSED and LOUD (external review F3).** A pass that cannot see is not a pass that
    /// found nothing: any failure here — chain tip, wallet record, or the RGB carrier enumeration —
    /// emits [`WalletEvent::WatchtowerBlind`], retains a [`WatchtowerFault`] readable through
    /// [`Self::watchtower_faults`], and returns `Err`. It never proceeds on a defaulted-empty
    /// carrier set: that made every carrier fail `is_token_carrier`, so the materialisation loop
    /// below found nothing to protect and the pass reported success while doing nothing at all.
    pub async fn auto_exit_due(&self, margin_blocks: u32) -> Result<Vec<String>> {
        match self.auto_exit_due_inner(margin_blocks).await {
            Ok(v) => {
                self.note_watchtower_ok(WatchtowerPass::AutoExit).await;
                Ok(v)
            }
            Err(e) => {
                self.note_watchtower_blind(WatchtowerPass::AutoExit, e.to_string()).await;
                Err(e)
            }
        }
    }

    async fn auto_exit_due_inner(&self, margin_blocks: u32) -> Result<Vec<String>> {
        use electrum_client::ElectrumApi;
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let record = self.record().await?;
        // [F3] NOT `unwrap_or_default()`. An empty carrier set is a legitimate answer only when the
        // enumeration SUCCEEDED; manufacturing one from a failure silently disarms the only clawback
        // protection a received RGB token has (the materialisation loop at the bottom of this
        // function iterates exactly the set this call returns).
        let carriers = self.unspendable_as_btc_outpoints().await.map_err(|e| {
            anyhow!(
                "watchtower pass aborted: could not enumerate RGB token carriers ({e}) — refusing to \
                 run the near-deadline pass against an assumed-empty carrier set, which would skip \
                 every carrier's clawback protection while reporting success"
            )
        })?;
        let ids: Vec<String> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| !is_token_carrier(c, &carriers))
            .filter_map(|c| c.statechain_id.clone())
            .collect();
        let mut exited = Vec::new();
        // [C1/HIGH] Coins this pass could NOT decide about. Collected rather than propagated
        // immediately (same contract as `defend_ladders_inner`): the protection of the OTHER coins
        // is time-critical and must not be cancelled by one unreadable row — but the pass must not
        // return `Ok` either, because `Ok` is read by the background loop and by
        // `note_watchtower_ok` as "this wallet is protected".
        let mut blind: Vec<String> = Vec::new();
        for id in ids {
            // Gate on a VERIFIED branch read first: only a coin with an exit branch has a deadline
            // at all, and only `read_exit_branch` can tell "this coin has none" from "I could not
            // look". Doing this before `estimate_exit_cost` also keeps a genuinely flat coin's
            // unrelated failures (e.g. no stored backup row) from being reported as blindness — a
            // watchtower that cries wolf on healthy wallets is a watchtower nobody reads.
            match self.read_exit_branch(&id).await {
                Ok(b) if b.is_empty() => continue, // verified flat: no ancestor can race it
                Ok(_) => {}
                Err(e) => {
                    blind.push(format!("{id} ({e})"));
                    continue;
                }
            }
            let est = match self.estimate_exit_cost(&id).await {
                Ok(e) => e,
                // Was `Err(_) => continue`: an unreadable estimate silently dropped a coin from the
                // near-deadline pass, and the pass still reported success.
                Err(e) => {
                    blind.push(format!("{id} (exit-cost estimate unreadable: {e})"));
                    continue;
                }
            };
            // [C1] "I could not compute a deadline" is NOT "there is no deadline". Check the blind
            // reason BEFORE the `Option`, because both shapes carry `exit_deadline_block == None`.
            if let Some(reason) = est.exit_deadline_blind.as_deref() {
                blind.push(format!("{id} ({reason})"));
                continue;
            }
            // The branch is non-empty (verified above), so a deadline MUST be either known or
            // blind. If it is somehow neither, fail closed rather than skip: "no deadline" on a
            // branch-bearing coin is never a safe answer.
            let Some(deadline) = est.exit_deadline_block else {
                blind.push(format!(
                    "{id} (has an exit branch but reported neither a deadline nor a reason — \
                     refusing to assume it is safe)"
                ));
                continue;
            };
            if tip + margin_blocks < deadline {
                continue; // still comfortably ahead of the deadline
            }
            let _ = self.inner.events_tx.send(WalletEvent::ExitDeadlineApproaching {
                statechain_id: id.clone(),
                deadline_block: deadline,
                tip,
            });
            // `.is_ok()` used as a classifier discarded the reason a DUE coin failed to exit, and the
            // pass still returned `Ok` ⟹ `note_watchtower_ok`. A coin inside its margin that did not
            // exit is the definition of unprotected: record it.
            match self.unilateral_exit(Some(vec![id.clone()]), None).await {
                Ok(_) => exited.push(id),
                Err(e) => blind.push(format!(
                    "{id} (DUE at block {deadline}, tip {tip}, but the forced exit failed: {e})"
                )),
            }
        }

        // Received token carriers: MATERIALIZE (branch only) — never plain-exit (that destroys the
        // allocation). Disjoint from the loop above (which excluded carriers). An issued/flat carrier
        // has no branch → VERIFIED no deadline → it is skipped (and only then).
        let carrier_ids: Vec<String> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| is_token_carrier(c, &carriers))
            .filter_map(|c| c.statechain_id.clone())
            .collect();
        for id in carrier_ids {
            // Same verified gate as the plain loop. An ISSUED carrier is genuinely branch-free and
            // genuinely unraceable; a RECEIVED carrier whose branch row cannot be read is the most
            // dangerous coin in the wallet, and the two must never share an exit path here.
            match self.read_exit_branch(&id).await {
                Ok(b) if b.is_empty() => continue, // verified: issued/flat carrier, no clawback risk
                Ok(_) => {}
                Err(e) => {
                    blind.push(format!("{id} (token carrier; {e})"));
                    continue;
                }
            }
            let est = match self.estimate_exit_cost(&id).await {
                Ok(e) => e,
                Err(e) => {
                    blind.push(format!("{id} (token carrier; exit-cost estimate unreadable: {e})"));
                    continue;
                }
            };
            // [C1] THE finding. This is the loop the reviewer named: a received RGB token carrier's
            // ONLY clawback protection is being materialised here before its ~6.9-day root deadline.
            // An uncomputable deadline used to reach the `else { continue }` below and vanish.
            if let Some(reason) = est.exit_deadline_blind.as_deref() {
                blind.push(format!("{id} (token carrier; {reason})"));
                continue;
            }
            let Some(deadline) = est.exit_deadline_block else {
                blind.push(format!(
                    "{id} (token carrier has an exit branch but reported neither a deadline nor a \
                     reason — refusing to assume its RGB allocation is safe)"
                ));
                continue;
            };
            if tip + margin_blocks < deadline {
                continue;
            }
            let _ = self.inner.events_tx.send(WalletEvent::TokenCarrierMaterialized {
                statechain_id: id.clone(),
                deadline_block: deadline,
                tip,
            });
            // Materialize = broadcast the exit branch ONLY (settles the RGB allocation on-chain and
            // spends the shared root, defeating the sender's clawback). The plain backup is
            // deliberately NOT broadcast — it would sweep the sats and destroy the allocation. A
            // conflict/failure leaves it for a later pass (broadcast_branch_if_any already alerted
            // via ExitBranchConflict on a racing spend) — but it is recorded as BLIND, because this
            // carrier is INSIDE its margin and did NOT get materialised. `unwrap_or(false)` here
            // discarded exactly that: the pass then returned `Ok` and the wallet reported itself
            // protected while a due carrier sat unprotected.
            match self.broadcast_branch_if_any(&id).await {
                Ok(true) => exited.push(id),
                Ok(false) => blind.push(format!(
                    "{id} (token carrier is DUE at block {deadline} but has no stored exit branch to \
                     materialise — it cannot be protected)"
                )),
                Err(e) => blind.push(format!(
                    "{id} (token carrier is DUE at block {deadline}; materialising its exit branch \
                     failed: {e})"
                )),
            }
        }
        if !blind.is_empty() {
            // Deliberately an `Err`, not a quiet `Ok(exited)`: `auto_exit_due` maps `Ok` to
            // `note_watchtower_ok`, i.e. to "this wallet's clawback protection ran with full
            // visibility". Coins actually protected this pass have already emitted their events, so
            // nothing is lost by failing here.
            //
            // NOTE the deliberate non-action: a blind coin is NOT force-materialised on suspicion.
            // Broadcasting an exit branch is cheap but not free, and a single electrum blip would
            // otherwise dump every off-chain coin in the wallet on-chain. Fail-closed for a WATCHER
            // means refusing to claim protection, not spending money on an unverified premise.
            return Err(anyhow!(
                "near-deadline protection is BLIND on {} coin(s) — their exit-race deadline could \
                 not be computed, so 'nothing is due' cannot be distinguished from 'I could not \
                 tell', and a received token on them can be clawed back unopposed: {}",
                blind.len(),
                blind.join(", ")
            ));
        }
        Ok(exited)
    }

    /// **TES-R watchtower pass (owner-run, keyless-style).** For every coin carrying an adopted TES-R
    /// ladder, run one [`watch_pass`](mercuryrustlib::tesr::watch_pass). If the coin has NOT been
    /// triggered (funding `F` still unspent) this is a no-op — an idle laddered coin never ages, so there is
    /// nothing to defend and no routine renewal is needed. If someone HAS triggered it (a contested
    /// exit: a prior owner racing a stale state, or a griefer), this races the owner's tiers; because
    /// the adopted current state carries the strictly-lowest CSV (enforced at adoption by
    /// `verify_bundle`), it matures first and wins, landing the funds at the owner's own key.
    /// Idempotent and incremental — call once per block from a background loop; already-broadcast
    /// tiers are skipped and a not-yet-mature tier retries next pass. A delegated tower runs the same
    /// pass against the owner's pre-signed bundle with NO key material. Returns the statechain_ids
    /// that had at least one tier broadcast this pass.
    /// **Runs in the default background pass (external review F2).** [`Self::start_background`]
    /// calls it once per NEW BLOCK — the defence is a per-block reaction (tier CSVs only mature on a
    /// block), so gating on the tip keeps it cheap while still racing a hostile trigger from the
    /// first block it lands in. Before this it was spawned by nothing at all: every shipped wrapper
    /// started only `claim()` + `auto_exit_due`, so a broadcast hostile trigger ran unopposed unless
    /// the app happened to schedule this itself.
    ///
    /// **Fails CLOSED and LOUD.** One coin whose ladder row cannot be READ no longer aborts the
    /// whole pass (that would let a single corrupt row blind the defence of every other coin): the
    /// readable coins are all defended first, and the unreadable ones are then reported as an `Err`
    /// naming them — with [`WalletEvent::WatchtowerBlind`] emitted and a [`WatchtowerFault`] retained
    /// for [`Self::watchtower_faults`]. Coins actually defended have already emitted
    /// [`WalletEvent::LadderDefended`], so nothing is lost by the `Err`.
    pub async fn defend_ladders(&self) -> Result<Vec<String>> {
        match self.defend_ladders_inner().await {
            Ok(v) => {
                self.note_watchtower_ok(WatchtowerPass::DefendLadders).await;
                Ok(v)
            }
            Err(e) => {
                self.note_watchtower_blind(WatchtowerPass::DefendLadders, e.to_string()).await;
                Err(e)
            }
        }
    }

    async fn defend_ladders_inner(&self) -> Result<Vec<String>> {
        let record = self.record().await?;
        let mut acted = Vec::new();
        // Coins this pass could NOT decide about (their `tesr-` row is unreadable). Collected rather
        // than propagated immediately: the defence of the OTHER coins is time-critical and must not
        // be cancelled by one corrupt row.
        let mut blind: Vec<String> = Vec::new();
        for c in &record.coins {
            if c.duplicate_index != 0 {
                continue;
            }
            let Some(id) = c.statechain_id.clone() else { continue };
            let bundle = match mercuryrustlib::tesr::load(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &id,
            )
            .await
            {
                Ok(Some(b)) => b,
                Ok(None) => continue, // no ladder on this coin — nothing to defend
                Err(e) => {
                    blind.push(format!("{id} (ladder row unreadable: {e})"));
                    continue;
                }
            };
            // F4: `watch_pass` now reports whether it could SEE. An unreadable chain backend is
            // `Blind`, NOT an empty "nothing to do" — treating the two alike is what let a dead
            // electrum connection masquerade as a quiet, well-defended coin.
            match mercuryrustlib::tesr::watch_pass(&self.inner.cc.electrum_client, &bundle) {
                mercuryrustlib::tesr::WatchState::Idle => {} // F unspent: verifiably nothing to do
                // [HIGH / F2+F4 join] `Acted { ids, .. }` DISCARDED `failures`. That field exists
                // precisely so a caller can tell a ladder that is WAITING (its next tier's
                // relative-CSV has not matured — the normal steady state of a contested exit) from
                // one that is being RACED or is dead (the tier's input was spent by a competing tx,
                // the backend rejected it, the stored hex is unusable). Both looked identical:
                // `ids` empty, no event, no fault, pass returns `Ok`. Classify, and treat anything
                // that is not a recognised "not yet" as blindness — an owner whose exit is being
                // out-raced learns about it on the NEXT pass instead of never.
                mercuryrustlib::tesr::WatchState::Acted { ids, failures } => {
                    if !ids.is_empty() {
                        let _ = self.inner.events_tx.send(WalletEvent::LadderDefended {
                            statechain_id: id.clone(),
                            tiers_broadcast: ids.len() as u32,
                        });
                        acted.push(id.clone());
                    }
                    let hard: Vec<&String> = failures
                        .iter()
                        .filter(|f| !ladder_failure_is_waiting(f))
                        .collect();
                    if !hard.is_empty() {
                        blind.push(format!(
                            "{id} (tier broadcast REJECTED for a reason that is not an immature \
                             CSV — the exit may be raced or unusable: {})",
                            hard.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; ")
                        ));
                    }
                }
                mercuryrustlib::tesr::WatchState::Blind { reason } => {
                    blind.push(format!("{id} ({reason})"));
                }
            }
        }
        if !blind.is_empty() {
            return Err(anyhow!(
                "ladder defence is BLIND on {} coin(s) — their TES-R bundle could not be read, or \
                 the chain backend could not be read for them, so a hostile trigger on them would \
                 run unopposed: {}",
                blind.len(),
                blind.join(", ")
            ));
        }
        Ok(acted)
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
        // Never sweep a token-carrier coin into an RGB-unaware L1 spend (destroys the allocation —
        // audit [7]). Exclude carriers from the withdraw-everything default; if the caller names a
        // carrier explicitly, hard-error so the loss is never silent.
        let carriers = self.unspendable_as_btc_outpoints().await?;
        let ids: Vec<String> = match statechain_ids {
            Some(ids) => {
                for id in &ids {
                    if let Some(c) = record.coins.iter().find(|c| c.statechain_id.as_deref() == Some(id)) {
                        if is_token_carrier(c, &carriers) {
                            return Err(anyhow!(
                                "coin {id} carries an RGB allocation; withdrawing it as plain BTC would destroy the tokens — move the asset off this coin first"
                            ));
                        }
                    }
                }
                ids
            }
            None => record
                .coins
                .iter()
                .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
                .filter(|c| !is_token_carrier(c, &carriers))
                .filter_map(|c| c.statechain_id.clone())
                .collect(),
        };
        let mut withdrawn = Vec::new();
        for id in ids {
            // A received in-ladder split CHILD is first-class off-chain (it CAN be paid onward via
            // `transfer` → `child_retransfer`), but it cannot be COOPERATIVELY withdrawn to an
            // arbitrary on-chain address: its funding `SP.out[j]` is un-broadcast, so there is no
            // confirmed outpoint for withdraw::execute to spend (it would find no backup/statechain
            // rows). Route it to the unilateral exit instead — materializing the pre-signed chain,
            // whose final state already pays this wallet's own key.
            if mercuryrustlib::tesr::load_child(&self.inner.cc, &self.inner.config.wallet_name, &id)
                .await?
                .is_some()
            {
                let _ = self.unilateral_exit(Some(vec![id.clone()]), None).await?;
                // The child's value is now committed to its (multi-block) exit — mark it WITHDRAWING so
                // it leaves the spendable balance immediately, mirroring a cooperative withdraw
                // (withdraw::execute sets the same status). Its exit chain completes over several blocks.
                let mut rec = self.record().await?;
                for c in rec.coins.iter_mut() {
                    if c.statechain_id.as_deref() == Some(id.as_str()) {
                        c.status = CoinStatus::WITHDRAWING;
                    }
                }
                self.save_record(&rec).await?;
                withdrawn.push(id);
                continue;
            }
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

    /// Read a coin's `branch-<id>` exit branch, distinguishing the two answers the raw storage call
    /// **cannot** distinguish. This is the root cause of most of the swallows in this file.
    ///
    /// `sqlite_manager::get_backup_txs` is a `fetch_one`, so a MISSING row (a genuinely flat coin:
    /// on-chain funding, no ancestor, no deadline) and a FAILED read (unreadable/locked/corrupt DB)
    /// both come back as `Err`. Every caller therefore had to collapse the two — `unwrap_or_default()`
    /// in `estimate_exit_cost`, `_ => return Ok(false)` in `broadcast_branch_if_any`,
    /// `.unwrap_or(false)` in `list_coins` — and collapsing them IS the silent-degradation class:
    /// an unreadable database then presents a raceable off-chain sub-coin as a safe flat one, so the
    /// deadline vanishes, the watchtower skips it, and the exit branch that beats the clawback is
    /// never broadcast. A `?` alone would be worse, not better: it would make every flat coin an
    /// error.
    ///
    /// So read the wallet's whole backup table with `fetch_all` — where "no rows" is a SUCCESSFUL
    /// empty result — and look the key up in it:
    ///
    /// - `Ok(vec![])` — **verified**: this coin has no exit branch.
    /// - `Ok(rows)` — the branch, root-first by `tx_n`.
    /// - `Err(_)` — the store could not be read or the row is corrupt. Never "no branch".
    pub(crate) async fn read_exit_branch(
        &self,
        statechain_id: &str,
    ) -> Result<Vec<mercurylib::wallet::BackupTx>> {
        let rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot read the backup store while looking for the exit branch of \
                 {statechain_id} ({e}) — refusing to report 'no exit branch' from a failed read"
            )
        })?;
        let key = format!("branch-{statechain_id}");
        let Some((_, json)) = rows.into_iter().find(|(k, _)| *k == key) else {
            return Ok(Vec::new()); // verified absent: a flat coin
        };
        serde_json::from_str(&json).map_err(|e| {
            anyhow!(
                "the stored exit branch of {statechain_id} is corrupt and cannot be parsed ({e}) — \
                 refusing to read a corrupt row as 'this coin has no exit branch'"
            )
        })
    }

    /// Broadcast the stored exit branch of a coin, root-first, if one exists. Returns whether a
    /// branch exists. Broadcast errors for already-confirmed branch txs (shared with a sibling
    /// coin's earlier exit) are tolerated; other errors surface.
    ///
    /// **[HIGH] A DB read failure is NOT "this coin has no exit branch".** The catch-all arm used
    /// to be `_ => return Ok(false)`, which folded an unreadable wallet DB into the flat-coin
    /// answer and made this function DECLINE to broadcast the one thing that saves the coin — while
    /// returning `Ok`, so `auto_exit_due`'s materialisation loop counted it as "nothing to do" and
    /// `withdraw` proceeded to spend an input that was never materialised. Only a **verified**
    /// absence (see [`Self::read_exit_branch`]) may answer `false`.
    pub(crate) async fn broadcast_branch_if_any(&self, statechain_id: &str) -> Result<bool> {
        let branch = self.read_exit_branch(statechain_id).await?;
        if branch.is_empty() {
            return Ok(false); // verified: this coin genuinely has no exit branch
        }
        use electrum_client::ElectrumApi;
        for b in branch.iter() {
            let tx: bitcoin::Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&b.tx)?)?;
            match self.inner.cc.electrum_client.transaction_broadcast(&tx) {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    // A mempool conflict means a DIFFERENT tx already spends this branch input — a
                    // competing spend racing our exit (a malicious sender front-running the branch
                    // root), NOT an idempotent rebroadcast of our own tx. It must NOT be swallowed
                    // as success (review H1): emit a distinct event and fail so the caller can
                    // fee-bump / alert instead of believing the coin exited.
                    if msg.contains("txn-mempool-conflict") || msg.contains("conflict") {
                        let _ = self.inner.events_tx.send(WalletEvent::ExitBranchConflict {
                            statechain_id: statechain_id.to_string(),
                        });
                        return Err(anyhow!(
                            "exit branch for {statechain_id} conflicts with an existing mempool spend of its root — the exit is being RACED by a competing spend; fee-bump or investigate (do not assume the coin exited): {msg}"
                        ));
                    }
                    // Tolerate ONLY an idempotent rebroadcast of our OWN branch tx (already in the
                    // mempool, or already mined). Anything else — including a double-spend/conflict
                    // rejection that happens to contain "already" — is a real failure.
                    if !is_idempotent_rebroadcast(&msg) {
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
    ///
    /// **[C1] Two distinct "no deadline" answers.** `exit_deadline_block == None` with
    /// `exit_deadline_blind == None` means the coin is flat and genuinely unraceable;
    /// `exit_deadline_blind == Some(reason)` means the coin HAS a branch and the deadline could not
    /// be computed. Callers that act on deadlines MUST branch on
    /// [`crate::types::ExitCostEstimate::deadline_is_unknown`] — see `auto_exit_due`.
    pub async fn estimate_exit_cost(&self, statechain_id: &str) -> Result<crate::types::ExitCostEstimate> {
        use electrum_client::ElectrumApi;
        // [HIGH] NOT `unwrap_or_default()`. A DB read failure used to manufacture an EMPTY branch,
        // which is byte-for-byte the flat-coin answer: `branch_txs: 0`, no branch vbytes, and —
        // fatally — `exit_deadline_block: None` on the `branch.is_empty()` arm below. An unreadable
        // wallet DB therefore reported a raceable off-chain sub-coin as a safe on-chain one, and
        // every deadline consumer (auto_exit_due, export_watch_bundle) skipped it. Fail closed:
        // `read_exit_branch` returns an empty vec ONLY for a verified-absent row.
        let branch = self.read_exit_branch(statechain_id).await?;
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

        // Safety deadline (off-chain sub-coins only): the earliest height an ANCESTOR could broadcast
        // its stale backup to race you; you MUST broadcast your (locktime-free) branch before it.
        //
        // Audit [10]/H5: `leaf_locktime + interval` is WRONG. A post-deposit split anchors the leaf
        // backup at the SPLIT tip (H_split + initlock - interval*qt), while ancestor backups are
        // anchored at the DEPOSIT tip. For an aged/transferred coin H_split >> H_deposit, so
        // leaf+interval overstates the real deadline by thousands of blocks — a watchtower honoring
        // it would exit AFTER a deposit-anchored ancestor backup already matured (clawback). Anchor
        // the deadline to the branch ROOT's on-chain deposit height instead: L0 = H_deposit + initlock
        // is deposit-anchored, split-tip-independent, and a safe (early) bound over all ancestors.
        //
        // [C1] The tri-state. `branch.is_empty()` is the ONLY shape that legitimately has no
        // deadline; anything else that comes back without one is BLINDNESS, and is reported as such
        // instead of being folded into the same `None`.
        let (exit_deadline_block, exit_deadline_blind) = if branch.is_empty() {
            (None, None) // flat/on-chain coin: no ancestor exists to race it. Genuinely safe.
        } else {
            match self.deposit_anchored_exit_deadline(&branch).await {
                Ok(d) => (Some(d), None),
                Err(e) => (
                    None,
                    Some(format!(
                        "the exit-race deadline of {statechain_id} could NOT be computed ({e}) — \
                         this coin has an exit branch, so an ancestor CAN race it; absence of a \
                         deadline here means blindness, not safety"
                    )),
                ),
            }
        };

        Ok(crate::types::ExitCostEstimate {
            statechain_id: statechain_id.to_string(),
            branch_txs: branch.len() as u32,
            branch_vbytes,
            backup_vbytes,
            total_vbytes: branch_vbytes + backup_vbytes,
            wait_blocks,
            exit_deadline_block,
            exit_deadline_blind,
        })
    }

    /// The deposit-anchored exit-race deadline (audit [10]): resolve the branch ROOT's on-chain
    /// funding-deposit confirmation height `H_deposit` and return `H_deposit + initlock`. This is
    /// deposit-anchored (independent of the split tip), so it is a safe (early) bound over every
    /// ancestor's backup maturity. An offline watchtower must cache `initlock` and the deposit
    /// height to compute this itself — never fall back to `leaf_locktime + interval`.
    ///
    /// **[C1] Returns `Result`, not `Option`.** This function is only ever called for a coin that
    /// HAS an exit branch, i.e. for a coin that definitely has a deadline and definitely can be
    /// raced by an ancestor. Every step below can fail for a reason that has nothing to do with the
    /// coin being safe — the SE config is unreachable, electrum is down, the deposit is not yet in
    /// the address history. The whole body used to be `.ok()?`, so all of those collapsed into the
    /// SAME `None` that a genuinely deadline-free flat coin yields, and `auto_exit_due` skipped the
    /// coin: the watcher concluded "nothing is due" from a total inability to tell, on exactly the
    /// coins whose only clawback protection it is. Each failure now carries a reason, and the
    /// caller lifts it into `ExitCostEstimate::exit_deadline_blind` ⟹ `WatchtowerBlind` + a
    /// retained `WatchtowerFault`.
    async fn deposit_anchored_exit_deadline(
        &self,
        branch: &[mercurylib::wallet::BackupTx],
    ) -> Result<u32> {
        use electrum_client::ElectrumApi;
        let initlock = mercuryrustlib::utils::info_config(&self.inner.cc)
            .await
            .map_err(|e| anyhow!("SE config unreachable, so `initlock` is unknown: {e}"))?
            .initlock;
        // Branch is stored root-first; the root's single input spends the on-chain deposit outpoint.
        let root = branch
            .iter()
            .min_by_key(|b| b.tx_n)
            .ok_or_else(|| anyhow!("exit branch is empty — no root tx to anchor the deadline to"))?;
        let root_tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&root.tx).map_err(|e| {
                anyhow!("exit-branch root tx is not valid hex: {e}")
            })?)
            .map_err(|e| anyhow!("exit-branch root tx does not deserialize: {e}"))?;
        let deposit_outpoint = root_tx
            .input
            .first()
            .ok_or_else(|| anyhow!("exit-branch root tx has no input to anchor the deadline to"))?
            .previous_output;
        // Fetch the deposit funding tx to read the scriptPubKey being spent, then find that tx's
        // confirmation height in the address history.
        let dep_tx = self
            .inner
            .cc
            .electrum_client
            .transaction_get(&deposit_outpoint.txid)
            .map_err(|e| {
                anyhow!(
                    "could not fetch the branch root's funding tx {}: {e}",
                    deposit_outpoint.txid
                )
            })?;
        let spk = &dep_tx
            .output
            .get(deposit_outpoint.vout as usize)
            .ok_or_else(|| {
                anyhow!(
                    "funding tx {} has no output {}",
                    deposit_outpoint.txid,
                    deposit_outpoint.vout
                )
            })?
            .script_pubkey;
        let history = self
            .inner
            .cc
            .electrum_client
            .script_get_history(spk)
            .map_err(|e| anyhow!("could not read the deposit address history: {e}"))?;
        let h_deposit = history
            .iter()
            .find(|h| h.tx_hash == deposit_outpoint.txid && h.height > 0)
            .map(|h| h.height as u32)
            .ok_or_else(|| {
                anyhow!(
                    "the branch root's deposit {} is not confirmed in the address history yet, so \
                     its anchor height is unknown",
                    deposit_outpoint.txid
                )
            })?;
        Ok(deposit_anchored_deadline(h_deposit, initlock))
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
        // Same carrier guard as withdraw (audit [7]): a unilateral exit broadcasts an RGB-unaware
        // spend, so a carrier coin must be excluded from the exit-everything default and rejected if
        // named explicitly.
        let carriers = self.unspendable_as_btc_outpoints().await?;
        let ids: Vec<String> = match statechain_ids {
            Some(ids) => {
                for id in &ids {
                    if let Some(c) = record.coins.iter().find(|c| c.statechain_id.as_deref() == Some(id)) {
                        if is_token_carrier(c, &carriers) {
                            return Err(anyhow!(
                                "coin {id} carries an RGB allocation; a plain unilateral exit would destroy the tokens — move the asset off this coin first"
                            ));
                        }
                        // [HF-4 / B1] Never exit a coin that is no longer ours to exit. The explicit-id
                        // branch previously filtered on carrier status ALONE — so a WITHDRAWN parent (a
                        // coin already consumed by a split, `register_split_subcoins_n` sets that status)
                        // could still be exited. On a laddered parent that is precisely the B1 theft: its
                        // retained no-timelock trigger spends F, killing the split tx that funds the
                        // receiver's sub-coin, while the ladder pays the splitter the full parent value.
                        // This does not by itself fix B1 (an attacker can patch their client — the real
                        // fix is the in-ladder split, PROTOCOL.md §5.4), but the SDK must not be the weapon,
                        // and it kills the accidental-loss variant where an honest user exits a spent
                        // parent and destroys their own payee's coin.
                        if c.status != CoinStatus::CONFIRMED {
                            return Err(anyhow!(
                                "coin {id} is not CONFIRMED (status {}) — it has already been spent/transferred and must not be exited; exiting a withdrawn parent would invalidate the sub-coins funded by its split [B1]",
                                c.status
                            ));
                        }
                    }
                }
                ids
            }
            None => record
                .coins
                .iter()
                .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
                .filter(|c| !is_token_carrier(c, &carriers))
                .filter_map(|c| c.statechain_id.clone())
                .collect(),
        };
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let mut statuses = Vec::new();
        for id in ids {
            // Laddered (TES-R) coin: if a ladder was adopted (deposit-established or received via Model A),
            // the unilateral exit runs the tier chain — trigger spends F, then each extension/state as
            // its relative-CSV matures — NOT the un-laddered absolute-locktime backup. exit_pass is idempotent
            // and incremental: it advances the ladder as far as maturity allows on each call, so an
            // owner (or a background loop) calls unilateral_exit once per block until `complete`.
            if let Some(bundle) =
                mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, &id).await?
            {
                // F4: `?` on both calls — an unreadable chain backend must surface as an error, not
                // as `complete: false, wait_blocks: 0`, which reads exactly like a healthy exit
                // still waiting for its next CSV.
                let progress =
                    mercuryrustlib::tesr::exit_pass(&self.inner.cc.electrum_client, &bundle)?;
                let done = progress.complete;
                let wait_blocks = if done {
                    0
                } else {
                    // AUDITED-SWALLOW: `?` already propagates an unreadable backend; the remaining
                    // `None` means "no further tier is waiting", so 0 is the true wait. It reports
                    // the exit as ready SOONER, i.e. it can only cause an extra pass, never a skip.
                    mercuryrustlib::tesr::next_exit_tier(&self.inner.cc.electrum_client, &bundle)?
                        .unwrap_or(0) as u32
                };
                statuses.push(crate::types::ExitStatus { statechain_id: id, complete: done, wait_blocks });
                continue;
            }

            // [in-ladder split] A received/change split CHILD claim (funded by an un-broadcast SP.out[j]):
            // exit its full pre-co-signed chain (T -> X_m -> SP -> ext_child -> state_child) via
            // exit_child_pass — keyless, every tier already signed, the final state pays this wallet's key.
            if let Some(cb) =
                mercuryrustlib::tesr::load_child(&self.inner.cc, &self.inner.config.wallet_name, &id).await?
            {
                let progress =
                    mercuryrustlib::tesr::exit_child_pass(&self.inner.cc.electrum_client, &cb)?;
                let done = progress.complete;
                let wait_blocks = if done {
                    0
                } else {
                    // AUDITED-SWALLOW: same as the parent-ladder case above — `?` covers the blind
                    // backend, `None` genuinely means no tier is pending.
                    mercuryrustlib::tesr::next_child_exit_tier(&self.inner.cc.electrum_client, &cb)?
                        .unwrap_or(0) as u32
                };
                statuses.push(crate::types::ExitStatus { statechain_id: id, complete: done, wait_blocks });
                continue;
            }

            // Materialize the coin's funding first (no locktime on branch txs).
            let has_branch = self.broadcast_branch_if_any(&id).await?;
            // Audit [20]: distinguish a genuinely FLAT coin (on-chain funding, legitimately no branch)
            // from a sub-coin whose exit branch is MISSING (e.g. restored from mnemonic without the
            // recovery bundle). If there is no branch AND the coin's own funding txid is not on-chain,
            // the branch is required but absent — broadcasting the leaf backup would fail with an
            // opaque "missing inputs". Surface an explicit, actionable error instead.
            if !has_branch {
                let funding_txid = record
                    .coins
                    .iter()
                    .find(|c| c.statechain_id.as_deref() == Some(&id))
                    .and_then(|c| c.utxo_txid.clone());
                if let Some(txid_str) = funding_txid {
                    if let Ok(txid) = txid_str.parse::<bitcoin::Txid>() {
                        // AUDITED-SWALLOW: fails toward LOUDER — an unreachable backend classifies
                        // as "funding not on chain" and produces the explicit "restore the recovery
                        // bundle" error instead of an opaque `missing inputs` broadcast rejection.
                        if self.inner.cc.electrum_client.transaction_get(&txid).is_err() {
                            return Err(anyhow!(
                                "sub-coin {id} has no stored exit branch (branch-{id} missing) and its funding {txid_str} is un-broadcast — it cannot be exited; restore the recovery bundle (branch-* rows)"
                            ));
                        }
                    }
                }
            }

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
                    if is_idempotent_rebroadcast(&msg) {
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
            // [class] `.unwrap_or(false)` here reported a coin whose branch row could not be READ as
            // `off_chain: false` — i.e. it showed a raceable off-chain sub-coin as a settled
            // on-chain one, in the listing an owner uses to decide whether anything needs exiting.
            // Propagate: `list_coins` already returns `Result`, and a listing that quietly lies
            // about which coins are off-chain is worse than no listing.
            let has_branch = match c.statechain_id.as_deref() {
                Some(sid) => !self.read_exit_branch(sid).await?.is_empty(),
                None => false, // no statechain_id ⟹ no `branch-<id>` key can exist for it
            };
            out.push(crate::types::CoinInfo {
                statechain_id: c.statechain_id.clone(),
                // AUDITED-SWALLOW: display-only default on an `Option<u32>` field, not a fallible
                // read — an amount-less coin renders as 0 sat and decides no protection.
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

/// The pure formula behind [`UtexoWallet::deposit_anchored_exit_deadline`] (audit [10] fix):
/// `H_deposit + initlock`, deposit-anchored and split-tip-independent. Deliberately k-UNAWARE —
/// it does not subtract the parent's pre-split hop count, so for a parent transferred k times
/// before the split it is LATE by `k·interval` blocks versus the true min-ancestor maturity.
/// That residual is OPEN audit item [17]. The invalidation model
/// (`invalidation_model.rs::deposit_anchored_deadline_exactness_domain`) calls this function
/// directly, so a k-aware fix here fails that test loudly and forces the [17] documentation
/// (test + INVALIDATION-SPEC.md §6) to be updated in the same change.
pub(crate) fn deposit_anchored_deadline(h_deposit: u32, initlock: u32) -> u32 {
    h_deposit + initlock
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

/// The coin's on-chain outpoint (`"txid:vout"`), matching rgb-lib's allocation outpoint format so a
/// token-carrier coin can be recognised and kept out of plain-BTC selection/balance (review H2).
/// `None` when the funding utxo is not yet known.
pub(crate) fn coin_outpoint(c: &Coin) -> Option<String> {
    match (c.utxo_txid.as_ref(), c.utxo_vout) {
        (Some(txid), Some(vout)) => Some(format!("{txid}:{vout}")),
        _ => None,
    }
}

/// True if this coin's utxo currently carries an RGB token allocation. Such a coin must NEVER be
/// selected for a plain-BTC spend — transfer, withdraw, unilateral exit, or an LN swap — because an
/// RGB-unaware spend of the carrier destroys the allocation (review H2 + audit-2026-07 [6]/[7]).
pub(crate) fn is_token_carrier(c: &Coin, carriers: &std::collections::HashSet<String>) -> bool {
    coin_outpoint(c).map_or(false, |o| carriers.contains(&o))
}

pub(crate) fn compute_balance(record: &WalletRecord) -> Balance {
    compute_balance_excluding(record, &std::collections::HashSet::new())
}

/// Like [`compute_balance`] but treats coins in `carriers` (token-carrier outpoints, `"txid:vout"`)
/// as NOT spendable BTC — their sats are excluded from every BTC bucket because spending them as
/// plain sats would destroy the RGB allocation (review H2). Those sats surface only via `tokens`.
pub(crate) fn compute_balance_excluding(
    record: &WalletRecord,
    carriers: &std::collections::HashSet<String>,
) -> Balance {
    let mut b = Balance::default();
    for c in &record.coins {
        if c.duplicate_index != 0 {
            continue;
        }
        if coin_outpoint(c).map_or(false, |o| carriers.contains(&o)) {
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

/// Classify a broadcast-error string: `true` iff it denotes an idempotent rebroadcast of a tx we
/// already submitted — the tx is already in the mempool or already mined — which is safe to treat
/// as success. Everything else is a real failure, INCLUDING errors that merely happen to contain
/// the word "already" (e.g. "inputs already spent", "bad-txns-inputs-already-spent"), which mean
/// the branch was REJECTED and must surface. Adversarial-log review (SDK_E2E chaos): the previous
/// bare `msg.contains("already")` would have swallowed a double-spend/conflict rejection as an
/// exit "success". The accepted phrases are the concrete bitcoind/electrs signals (case-folded):
/// bitcoind -27 emits "already in block chain" (older) or "outputs already in utxo set" (newer;
/// observed in SDK_E2E_7), and an already-accepted mempool tx is "txn-already-known".
/// Classify a TES-R tier-broadcast rejection reported in [`mercuryrustlib::tesr::WatchState::Acted`]'s
/// `failures`: `true` iff it means **"not yet"** (the tier's relative-CSV has not matured, or its
/// parent is not confirmed deep enough for BIP68 to count), which is the ordinary steady state of a
/// contested exit walking up the ladder one block at a time.
///
/// Everything else — a conflict, a spent input, a policy rejection, an unusable tx — means the tier
/// will NOT go out by waiting, and `defend_ladders` reports it as blindness. The list is
/// deliberately CLOSED (unknown wording ⟹ not waiting ⟹ loud): a new backend wording that we
/// mis-read as "raced" costs an alert, whereas one we mis-read as "waiting" costs the coin.
pub(crate) fn ladder_failure_is_waiting(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const WAITING: &[&str] = &[
        "non-bip68-final",   // bitcoind: relative timelock (CSV) not satisfied yet
        "non-final",         // bitcoind: nLockTime not reached (also covers non-BIP68-final)
        "bad-txns-nonfinal", // wording variant
    ];
    // An idempotent rebroadcast is not a failure at all (the tier is already out) — tolerate it
    // here too, since `tx_known` and the broadcast can race across passes.
    WAITING.iter().any(|needle| m.contains(needle)) || is_idempotent_rebroadcast(msg)
}

pub(crate) fn is_idempotent_rebroadcast(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const ACCEPTED: &[&str] = &[
        "already in block chain",   // bitcoind -27: tx already mined
        "already in utxo set",      // bitcoind -27 (newer wording): outputs already in utxo set
        "txn-already-known",        // bitcoind: our tx is already accepted in the mempool
        "already in mempool",       // wording variant across backends
        "already have transaction", // wording variant across backends
    ];
    ACCEPTED.iter().any(|needle| m.contains(needle))
}

#[cfg(test)]
mod broadcast_tests {
    use super::is_idempotent_rebroadcast;

    // Idempotent rebroadcasts of our OWN tx (already in mempool / already mined) are tolerated.
    #[test]
    fn tolerates_already_broadcast_or_mined() {
        for msg in [
            "sendrawtransaction RPC error: {\"code\":-27,\"message\":\"Transaction already in block chain\"}",
            "sendrawtransaction RPC error: {\"code\":-27,\"message\":\"Transaction outputs already in utxo set\"}",
            "bad-txns-inputs error: txn-already-known",
            "Transaction already in mempool",
            "server error: already have transaction",
        ] {
            assert!(is_idempotent_rebroadcast(msg), "should tolerate: {msg}");
        }
    }

    // A rejection that merely CONTAINS "already" is NOT idempotent — it must surface as a failure.
    // These are the cases the old `contains("already")` heuristic wrongly swallowed as success.
    #[test]
    fn rejects_conflict_and_double_spend_bearing_already() {
        for msg in [
            "bad-txns-inputs-already-spent",           // double-spend: our exit was beaten
            "sendrawtransaction RPC error: inputs already spent",
            "txn-mempool-conflict",                    // a different tx already spends the input
            "insufficient fee, rejected",              // unrelated rejection, no "already"
            "missing inputs",
        ] {
            assert!(!is_idempotent_rebroadcast(msg), "should reject: {msg}");
        }
    }
}

#[cfg(test)]
mod identity_tests {
    use super::UtexoWallet;
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

        assert!(UtexoWallet::validate_message_with_identity_key(msg, &sig_hex, &pk_hex).unwrap());
        // tampered message fails
        assert!(!UtexoWallet::validate_message_with_identity_key(b"other message", &sig_hex, &pk_hex).unwrap());
    }
}

/// **The silent-degradation guard for this file, enforced instead of remembered.**
///
/// Three review rounds fixed this class one site at a time and the next round always found more,
/// because the fix is invisible: the corrected code looks exactly like the broken code minus a
/// `unwrap_or_default()`. So the property is checked mechanically, on this file's own source, at
/// `cargo test` time.
///
/// The rule is NOT "never swallow". A swallow that fails toward MORE work is fine (see the audited
/// one in `claim`'s token rescan). The rule is: **where a swallow is combined with a
/// protection-deciding SUBJECT — the exit branch, the exit deadline, the carrier set, the ladder
/// bundle — it must carry an `AUDITED-SWALLOW:` note saying which direction it fails in.** That
/// makes the dangerous cases impossible to add silently, and forces the next author to write down
/// the direction argument this class keeps getting wrong.
#[cfg(test)]
mod silent_degradation_guard {
    /// Spellings that turn a failure into a benign-looking value.
    const SWALLOWS: &[&str] = &[
        "unwrap_or_default()",
        "unwrap_or(",
        ".ok()?",
        ".ok()\n",
        "Err(_) =>",
        "is_err()",
        "is_ok()",
    ];

    /// Reads whose failure decides whether a coin is PROTECTED. A swallow next to one of these is
    /// how "I could not look" becomes "there is nothing to do".
    const PROTECTION_SUBJECTS: &[&str] = &[
        "get_backup_txs",
        "read_exit_branch",
        "estimate_exit_cost",
        "exit_deadline",
        "deposit_anchored_exit_deadline",
        "unspendable_as_btc_outpoints",
        "token_carrier_outpoints",
        "get_token_balances",
        "broadcast_branch_if_any",
        "watch_pass",
        "tesr::load",
    ];

    /// The escape hatch: an explicit, argued exception.
    const MARKER: &str = "AUDITED-SWALLOW";

    /// How far back to look for the statement's start and for the marker comment.
    const LOOKBACK: usize = 14;

    fn source() -> &'static str {
        include_str!("wallet.rs")
    }

    #[test]
    fn no_unaudited_swallow_next_to_a_protection_decision() {
        let src = source();
        let all: Vec<&str> = src.lines().collect();
        // Stop at this module: it necessarily contains the forbidden spellings as data.
        let end = all
            .iter()
            .position(|l| l.starts_with("mod silent_degradation_guard {"))
            .unwrap_or(all.len());
        let lines = &all[..end];
        assert!(end > 1000, "the guard must scan the whole file, not a truncated prefix");
        let mut offenders = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("///") {
                continue; // prose describing the class is not the class
            }
            if !SWALLOWS.iter().any(|s| line.contains(s.trim_end_matches('\n'))) {
                continue;
            }
            let start = i.saturating_sub(LOOKBACK);
            let window = lines[start..=i].join("\n");
            // Only care when the swallow sits next to something protection-deciding...
            if !PROTECTION_SUBJECTS.iter().any(|s| window.contains(s)) {
                continue;
            }
            // ...and has no argued exception recorded next to it.
            if window.contains(MARKER) {
                continue;
            }
            offenders.push(format!("wallet.rs:{}: {}", i + 1, line.trim()));
        }
        assert!(
            offenders.is_empty(),
            "unaudited swallow(s) on a protection-deciding read. Each of these turns a failure \
             into a benign-looking empty/false/None, which is how a dead chain backend or an \
             unreadable DB becomes 'nothing is due'. Either propagate the error, or add an \
             `{MARKER}:` comment above it stating which direction it fails in (toward MORE work is \
             safe, toward LESS protection is the bug):\n  {}",
            offenders.join("\n  ")
        );
    }

    /// The guard must be able to FAIL, otherwise it is decoration.
    #[test]
    fn guard_catches_a_planted_regression() {
        let planted = "\
            let branch = get_backup_txs(&pool, &name, &key).await.unwrap_or_default();\n";
        let lines: Vec<&str> = planted.lines().collect();
        let hit = lines.iter().enumerate().any(|(i, line)| {
            let window = lines[i.saturating_sub(LOOKBACK)..=i].join("\n");
            SWALLOWS.iter().any(|s| line.contains(s.trim_end_matches('\n')))
                && PROTECTION_SUBJECTS.iter().any(|s| window.contains(s))
                && !window.contains(MARKER)
        });
        assert!(hit, "the guard would not catch the exact regression it exists to prevent");
    }

    /// ...and it must NOT fire on the audited exception, or the marker is worthless.
    #[test]
    fn guard_accepts_an_audited_swallow() {
        let audited = "\
            // AUDITED-SWALLOW: fails toward MORE work — an empty set re-attempts every coin.\n\
            let booked = self.token_carrier_outpoints().await.unwrap_or_default();\n";
        let lines: Vec<&str> = audited.lines().collect();
        let hit = lines.iter().enumerate().any(|(i, line)| {
            if line.trim_start().starts_with("//") {
                return false;
            }
            let window = lines[i.saturating_sub(LOOKBACK)..=i].join("\n");
            SWALLOWS.iter().any(|s| line.contains(s.trim_end_matches('\n')))
                && PROTECTION_SUBJECTS.iter().any(|s| window.contains(s))
                && !window.contains(MARKER)
        });
        assert!(!hit, "an explicitly audited swallow must be accepted, or nobody will use the marker");
    }
}
