use std::sync::Arc;

use anyhow::{anyhow, Result};
use mercurylib::wallet::{Coin, CoinStatus, Wallet as WalletRecord};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::sqlite_manager::{get_wallet, insert_wallet, update_wallet};
use tokio::sync::{broadcast, Mutex};

use crate::config::SdkConfig;
use crate::events::WalletEvent;
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

pub(crate) struct Inner {
    pub cc: ClientConfig,
    pub config: SdkConfig,
    pub events_tx: broadcast::Sender<WalletEvent>,
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
        // invite an RGB-destroying spend. For a non-token wallet there are no carriers, so an empty
        // set is correct.
        let carriers = if self.inner.config.rgb_data_dir.is_some()
            && self.inner.config.rgb_proxy_url.is_some()
        {
            self.unspendable_as_btc_outpoints().await?
        } else {
            self.unspendable_as_btc_outpoints().await.unwrap_or_default()
        };
        let mut balance = compute_balance_excluding(&record, &carriers);
        balance.tokens = self.get_token_balances().await.unwrap_or_default();
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

        // Auto-establish the TES-R exit ladder for every fresh deposit — unconditional, there is one
        // protocol. Only CONFIRMED, non-duplicate, non-single-use ROOT coins with no existing ladder
        // qualify (idempotent). The exit payee is the coin's seed-derived `backup_address`
        // (recoverable from the mnemonic), NEVER an out-of-wallet address. An establish failure
        // leaves the coin un-laddered — still exitable via its signed-once backup — and the next
        // claim() retries.
        {
            let mut rec = self.record().await?;
            let network = rec.network.clone();
            // Terminal-freeze invariant (PROTOCOL.md §5.10, rule 1): RGB rides the signed-once colored
            // carrier model and is NEVER anchored on the renewable T/X/S ladder — a plain tier spend
            // would destroy the allocation. So an RGB carrier must not get a ladder. `single_use`
            // catches terminalized/combine carriers, but a resting issuance/received carrier holding
            // an allocation need not be single_use, so we also exclude the RGB carrier set. Fail CLOSED
            // for a token wallet whose RGB state is momentarily unavailable: skip establishing this
            // pass (leave coins un-laddered) rather than risk laddering a carrier — next claim() retries.
            let carriers = if self.inner.config.rgb_data_dir.is_some()
                && self.inner.config.rgb_proxy_url.is_some()
            {
                self.unspendable_as_btc_outpoints().await.map(Some).unwrap_or(None)
            } else {
                Some(std::collections::HashSet::new())
            };
            // `None` = a token wallet whose RGB state is momentarily unavailable → skip establishing
            // this pass rather than risk laddering a carrier; the next claim() retries.
            for coin in rec.coins.iter_mut() {
                let Some(carriers) = carriers.as_ref() else { break };
                let sid = match &coin.statechain_id { Some(s) => s.clone(), None => continue };
                if coin.status != CoinStatus::CONFIRMED
                    || coin.duplicate_index != 0
                    || coin.single_use
                    || is_token_carrier(coin, carriers)
                {
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
                let Some(f_spk_hex) = f_spk_hex else { continue };
                match mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, &sid).await {
                    Ok(Some(_)) => continue, // already established — idempotent
                    Ok(None) => {}
                    Err(_) => continue,
                }
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
                    _ => continue, // coordinator unreachable / no record → not known bindable → skip
                };
                if mercuryrustlib::tesr::ladder_binding_precheck(
                    &sid,
                    &f_spk_hex,
                    se_agg.as_deref(),
                    &network,
                )
                .is_err()
                {
                    continue;
                }
                let payee = coin.backup_address.clone();
                match mercuryrustlib::tesr::establish_auto(&self.inner.cc, coin, &payee, &network).await {
                    Ok(bundle) => {
                        if mercuryrustlib::tesr::persist(&self.inner.cc, &self.inner.config.wallet_name, &bundle).await.is_ok() {
                            let _ = self.inner.events_tx.send(WalletEvent::LadderEstablished { statechain_id: sid });
                        }
                    }
                    Err(_) => { /* leave un-laddered: its tx1 backup still exits; resumable retry is a later stage */ }
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

    /// Start the background watcher (deposit confirmation + incoming-transfer auto-claim).
    /// Returns a handle; abort it to stop. Mirrors the Spark SDK's background stream + claim
    /// automation (poll-based — Mercury has no server push).
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
                // Watchtower pass: protect off-chain coins nearing their exit-race deadline —
                // force-exit plain sub-coins, MATERIALIZE token carriers (REQ-33). Default-on so an
                // idle receiver is protected without scheduling anything.
                if wallet.inner.config.auto_exit {
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
    pub async fn auto_exit_due(&self, margin_blocks: u32) -> Result<Vec<String>> {
        use electrum_client::ElectrumApi;
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let record = self.record().await?;
        let carriers = self.unspendable_as_btc_outpoints().await.unwrap_or_default();
        let ids: Vec<String> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| !is_token_carrier(c, &carriers))
            .filter_map(|c| c.statechain_id.clone())
            .collect();
        let mut exited = Vec::new();
        for id in ids {
            let est = match self.estimate_exit_cost(&id).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            // Only off-chain sub-coins carry an exit-race deadline (flat coins return None).
            let Some(deadline) = est.exit_deadline_block else { continue };
            if tip + margin_blocks < deadline {
                continue; // still comfortably ahead of the deadline
            }
            let _ = self.inner.events_tx.send(WalletEvent::ExitDeadlineApproaching {
                statechain_id: id.clone(),
                deadline_block: deadline,
                tip,
            });
            if self.unilateral_exit(Some(vec![id.clone()]), None).await.is_ok() {
                exited.push(id);
            }
        }

        // Received token carriers: MATERIALIZE (branch only) — never plain-exit (that destroys the
        // allocation). Disjoint from the loop above (which excluded carriers). An issued/flat carrier
        // has no branch → estimate_exit_cost yields no deadline → it is skipped.
        let carrier_ids: Vec<String> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| is_token_carrier(c, &carriers))
            .filter_map(|c| c.statechain_id.clone())
            .collect();
        for id in carrier_ids {
            let est = match self.estimate_exit_cost(&id).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            let Some(deadline) = est.exit_deadline_block else { continue };
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
            // via ExitBranchConflict on a racing spend).
            if self.broadcast_branch_if_any(&id).await.unwrap_or(false) {
                exited.push(id);
            }
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
    pub async fn defend_ladders(&self) -> Result<Vec<String>> {
        let record = self.record().await?;
        let mut acted = Vec::new();
        for c in &record.coins {
            if c.duplicate_index != 0 {
                continue;
            }
            let Some(id) = c.statechain_id.clone() else { continue };
            let Some(bundle) = mercuryrustlib::tesr::load(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &id,
            )
            .await?
            else {
                continue;
            };
            let broadcast = mercuryrustlib::tesr::watch_pass(&self.inner.cc, &bundle);
            if !broadcast.is_empty() {
                let _ = self.inner.events_tx.send(WalletEvent::LadderDefended {
                    statechain_id: id.clone(),
                    tiers_broadcast: broadcast.len() as u32,
                });
                acted.push(id);
            }
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
        let exit_deadline_block = if branch.is_empty() {
            None
        } else {
            self.deposit_anchored_exit_deadline(&branch).await
        };

        Ok(crate::types::ExitCostEstimate {
            statechain_id: statechain_id.to_string(),
            branch_txs: branch.len() as u32,
            branch_vbytes,
            backup_vbytes,
            total_vbytes: branch_vbytes + backup_vbytes,
            wait_blocks,
            exit_deadline_block,
        })
    }

    /// The deposit-anchored exit-race deadline (audit [10]): resolve the branch ROOT's on-chain
    /// funding-deposit confirmation height `H_deposit` and return `H_deposit + initlock`. This is
    /// deposit-anchored (independent of the split tip), so it is a safe (early) bound over every
    /// ancestor's backup maturity. Returns `None` if the SE config or the on-chain deposit height
    /// cannot be resolved (an offline watchtower must cache `initlock` and the deposit height to
    /// compute this itself — never fall back to `leaf_locktime + interval`).
    async fn deposit_anchored_exit_deadline(
        &self,
        branch: &[mercurylib::wallet::BackupTx],
    ) -> Option<u32> {
        use electrum_client::ElectrumApi;
        let initlock = mercuryrustlib::utils::info_config(&self.inner.cc).await.ok()?.initlock;
        // Branch is stored root-first; the root's single input spends the on-chain deposit outpoint.
        let root = branch.iter().min_by_key(|b| b.tx_n)?;
        let root_tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&root.tx).ok()?).ok()?;
        let deposit_outpoint = root_tx.input.first()?.previous_output;
        // Fetch the deposit funding tx to read the scriptPubKey being spent, then find that tx's
        // confirmation height in the address history.
        let dep_tx = self
            .inner
            .cc
            .electrum_client
            .transaction_get(&deposit_outpoint.txid)
            .ok()?;
        let spk = &dep_tx.output.get(deposit_outpoint.vout as usize)?.script_pubkey;
        let history = self.inner.cc.electrum_client.script_get_history(spk).ok()?;
        let h_deposit = history
            .iter()
            .find(|h| h.tx_hash == deposit_outpoint.txid && h.height > 0)
            .map(|h| h.height as u32)?;
        Some(deposit_anchored_deadline(h_deposit, initlock))
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
                let (_broadcast, done) = mercuryrustlib::tesr::exit_pass(&self.inner.cc, &bundle);
                let wait_blocks = if done {
                    0
                } else {
                    mercuryrustlib::tesr::next_exit_tier(&self.inner.cc, &bundle).unwrap_or(0) as u32
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
                let (_broadcast, done) = mercuryrustlib::tesr::exit_child_pass(&self.inner.cc, &cb);
                let wait_blocks = if done {
                    0
                } else {
                    mercuryrustlib::tesr::next_child_exit_tier(&self.inner.cc, &cb).unwrap_or(0) as u32
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
