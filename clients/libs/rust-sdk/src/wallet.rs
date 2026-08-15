use std::sync::Arc;

use anyhow::{anyhow, Result};
use mercurylib::wallet::{Coin, CoinStatus, Wallet as WalletRecord};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::sqlite_manager::{get_wallet, insert_wallet, update_wallet};
use mercuryrustlib::transfer_sender::{CancelConsentRequest, CancelOutcome};
use tokio::sync::{broadcast, Mutex};

use crate::config::SdkConfig;
use crate::events::{LadderSkipReason, WalletEvent, WatchtowerPass};
use crate::types::{Balance, ClaimResult, DepositAddressInfo, SdkError, TokenClaimState, TokenClaimStatus};

/// **[K>1 prerequisite 4] The SE's per-parent LIFETIME derived-slot allowance**, mirroring
/// `max_derived_tokens_per_statechain` (`server/src/server_config.rs`, default 64).
///
/// Mirrored rather than fetched because the number is needed BEFORE the first SE call — the point of
/// the guard is to refuse an oversized batch while nothing has been minted, co-signed or
/// terminalized. An operator who lowers the cap makes this bound optimistic, and the server's own
/// 400 then stops the batch at the mint; an operator who raises it makes it conservative, which
/// costs a large batch a split into two. Both directions are safe; the direction that would not be
/// is admitting a batch the SE cannot slot, and this constant cannot cause that.
pub const DERIVED_SLOTS_PER_STATECHAIN: u32 = 64;

/// The largest K a single in-ladder batch may carry: `K + 1` slots must fit
/// [`DERIVED_SLOTS_PER_STATECHAIN`], so K ≤ 63.
pub const MAX_BATCH_RECIPIENTS: usize = DERIVED_SLOTS_PER_STATECHAIN as usize - 1;

/// How many times a pooled derived-slot voucher may fail to become a slot before it is discarded.
/// See [`UtexoWallet::fail_slot_voucher`].
pub const SLOT_VOUCHER_FAILURE_LIMIT: u32 = 3;

/// Wallet-DB key of the durable derived-slot voucher pool.
const SLOT_VOUCHER_KEY: &str = "slotvouchers";

/// One minted-but-unused derived-slot voucher — see [`UtexoWallet::take_derived_tokens`].
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SlotVoucher {
    pub token_id: String,
    /// Consecutive failed attempts to turn this voucher into a slot.
    #[serde(default)]
    pub failures: u32,
}

/// **[K>1 prerequisite 4] Refuse a slot batch the derived-slot allowance cannot carry, by name.**
///
/// `slots` is `K + 1` for a K-recipient batch. The refusal is a typed
/// [`SdkError::BatchTooManyRecipients`] naming K, the slot count, the cap and the largest admissible
/// K — a caller that batches automatically can read the bound out of the error instead of guessing
/// at a bare "count must be between 1 and 64" from the server, which arrives only after the caller
/// has already assembled the recipient list.
pub(crate) fn refuse_oversized_slot_batch(slots: usize) -> Result<()> {
    if slots > DERIVED_SLOTS_PER_STATECHAIN as usize {
        return Err(SdkError::BatchTooManyRecipients {
            recipients: slots.saturating_sub(1),
            slots,
            cap: DERIVED_SLOTS_PER_STATECHAIN,
            max_recipients: MAX_BATCH_RECIPIENTS,
        }
        .into());
    }
    Ok(())
}

/// The pure half of [`UtexoWallet::fail_slot_voucher`]: bump the failure count of `token_id` and
/// drop it once it hits [`SLOT_VOUCHER_FAILURE_LIMIT`]. Returns whether the pool changed.
fn record_voucher_failure(pool: &mut Vec<SlotVoucher>, token_id: &str) -> bool {
    let Some(v) = pool.iter_mut().find(|v| v.token_id == token_id) else {
        return false;
    };
    v.failures += 1;
    if v.failures >= SLOT_VOUCHER_FAILURE_LIMIT {
        pool.retain(|v| v.token_id != token_id);
    }
    true
}

/// A complete off-line recovery bundle for a wallet (review H3). Contains everything that lives
/// ONLY on the owner's disk and that the SE cannot re-serve after a claim: the full wallet record
/// (mnemonic + coins + activity), every backup row (the pre-signed exit ladder, the off-chain
/// `branch-*` exit branches, and the `parents-*` terminal-ancestor lists), and the RGB engine seed.
/// NOTE: the RGB *stash* (contracts/consignments under `rgb_data_dir`) is NOT embedded — copy that
/// directory too; from re-obtainable consignments the stash can be rebuilt, but not from the seed
/// alone.
/// The only recovery-bundle format version this build understands. (D11: the encoding is frozen;
/// import must refuse an unknown version rather than mis-parse it.)
const RECOVERY_BUNDLE_VERSION: u32 = 1;

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

/// Parse and version-check a [`RecoveryBundle`] JSON. Probes the `version` field FIRST — a future
/// version may carry a layout that would not deserialize into today's struct — and refuses an
/// unknown version by name before the full parse.
fn parse_recovery_bundle(bundle_json: &str) -> Result<RecoveryBundle> {
    #[derive(serde::Deserialize)]
    struct VersionProbe {
        version: u32,
    }
    let probe: VersionProbe = serde_json::from_str(bundle_json)?;
    if probe.version != RECOVERY_BUNDLE_VERSION {
        return Err(SdkError::UnsupportedVersion {
            kind: "recovery bundle",
            found: probe.version as u64,
            supported: RECOVERY_BUNDLE_VERSION as u64,
        }
        .into());
    }
    Ok(serde_json::from_str(bundle_json)?)
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

/// [D13] Whether a split child / spine tip may be force-exited by the near-deadline pass.
#[derive(Debug, PartialEq, Eq)]
enum LeafSplitGate {
    /// No open split names this coin — safe to evaluate for a near-deadline exit.
    Drive,
    /// The split journal could not be read at all, so no leaf can be vouched for. Blanket blindness
    /// is recorded once before the loop; each row is dropped without a second message.
    HoldSilently,
    /// This coin is mid-split: an open split-journal record names it as terminalized, so its stored
    /// row is the state that split SUPERSEDES. Driving it would destroy the pieces the split
    /// already conveyed. Recorded as attention-needed (never idle), never driven.
    Hold(String),
}

/// [D13] THE PREDICATE THE PLAIN-LEAF PORT'S SAFETY RESTS ON. A CONFIRMED status is not proof a
/// leaf is safe to force-exit: a leaf terminalized by its own partial-payment split reads CONFIRMED
/// with a stale row until the split's conveyance completes, and permanently if it crashed. The
/// split journal — written at `Planned` strictly before the irreversible co-signature — is the
/// durable evidence, so a mid-split coin appears here before any piece can be handed out.
///
/// `terminalizing`: `None` = the journal was unreadable (blindness over every child); `Some(set)` =
/// the statechain ids currently mid-split.
fn leaf_split_gate(
    terminalizing: &Option<std::collections::HashSet<String>>,
    cid: &str,
    what: &str,
) -> LeafSplitGate {
    match terminalizing {
        None => LeafSplitGate::HoldSilently,
        Some(set) if set.contains(cid) => LeafSplitGate::Hold(format!(
            "{cid} ({what}) is mid-split — an open split-journal record names it as terminalized, \
             so its stored row is the state that split SUPERSEDES. Force-exiting it would destroy \
             the pieces the split already conveyed. Left for the split's own recovery, reported \
             here so it is not mistaken for idle"
        )),
        Some(_) => LeafSplitGate::Drive,
    }
}

/// [D13-follow-up] Does this leaf's exit actually pay THIS wallet?
///
/// **The cancel-resurrection window.** `child_retransfer` marks the child IN_TRANSFER before its
/// co-sign and stores the superseding, PAYEE-paying bundle before conveying it. If the conveyance
/// leg then fails, the coin sits IN_TRANSFER with a row whose `child_owner_exit_address` is the
/// payee's. A child conveyance opens a real coordinator transfer, so `POST /transfer/cancel` is
/// reachable for it — and `status_after_cancel` lifts IN_TRANSFER back to CONFIRMED
/// (`clients/libs/rust/src/transfer_sender.rs:1715-1720`). The repair step does not help:
/// `reclaim_cancelled_conveyance` loads a `tesr-` bundle and returns `Ok(false)` for a child, which
/// has none, so the row is never re-pointed at the owner.
///
/// A CONFIRMED status therefore does not imply the stored row pays us. Driving one that does not
/// would force-exit the coin straight into the payee's address — handing away the sender's own
/// money. This is the guard for that.
///
/// **Unrecognised is reported, not silently skipped.** Failing closed here means declining to
/// defend a coin, which is itself a loss if the address is legitimately ours and merely unknown to
/// this record. So the caller surfaces it as attention-needed rather than dropping it quietly.
fn leaf_exit_pays_this_wallet(exit_address: &str, record: &WalletRecord) -> bool {
    wallet_holds_address(exit_address, wallet_addresses(record).iter().map(|s| s.as_str()))
}

/// Every address this wallet can be paid at, from its own record.
fn wallet_addresses(record: &WalletRecord) -> Vec<String> {
    let mut out = Vec::new();
    for c in record.coins.iter() {
        out.push(c.backup_address.clone());
        out.push(c.address.clone());
        if let Some(a) = c.aggregated_address.as_ref() {
            out.push(a.clone());
        }
    }
    out
}

/// The comparison itself, split out so it is testable without constructing a whole wallet record
/// (neither `Coin` nor `Wallet` implements `Default`, and a hand-built one would test the fixture
/// rather than the rule).
fn wallet_holds_address<'a>(exit_address: &str, mut known: impl Iterator<Item = &'a str>) -> bool {
    known.any(|a| a == exit_address)
}

/// [D13] The event a near-deadline force-exit emits, chosen by the coin's kind. A coloured row
/// settles a token allocation ([`WalletEvent::TokenCarrierMaterialized`]); a plain sats leaf is
/// driven to L1 to beat its deadline ([`WalletEvent::LeafExitForced`]). Emitting the token event
/// for a plain coin would mis-report it to any integrator watching the stream.
fn near_deadline_exit_event(
    colored: bool,
    statechain_id: String,
    deadline_block: u32,
    tip: u32,
) -> WalletEvent {
    if colored {
        WalletEvent::TokenCarrierMaterialized { statechain_id, deadline_block, tip }
    } else {
        WalletEvent::LeafExitForced { statechain_id, deadline_block, tip }
    }
}

/// Utexo wallet (Spark-compatible API) on Mercury+RGB. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct UtexoWallet {
    pub(crate) inner: Arc<Inner>,
}

/// **[D66] WHICH MAINTENANCE PASSES A BACKGROUND TICK RUNS — as a VALUE, so it can be tested.**
///
/// [D58] fixed a real defect: the deadline pass sat in the `else` arm of an economics flag, so a
/// wallet that OPTED INTO background maintenance lost the sever. [D64] then established that a
/// source-scanning guard **cannot** hold that fix — `deny_optional_deadline_safety` was defeated by
///
/// ```ignore
/// let _ = cfg.background_auto_refresh && wallet.deadline_safety_due(..).await.is_ok();
/// ```
///
/// which is at brace depth 0, contains `.await`, and short-circuits. Presence and depth are all a
/// scan can see; **reachability is not expressible in a substring**.
///
/// So the decision is lifted out of the control flow and into a return value. The loop executes the
/// plan; the plan is a pure function of the config; and
/// [`every_config_still_schedules_the_deadline_pass`] EXECUTES it over the whole config space and
/// asserts the pass is always present. That is a behavioural proof, not a description of one — the
/// `&&` mutation above cannot be written against a `Vec` the caller iterates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenancePass {
    /// Cooperative re-anchor first, then SEVER from `F` for whatever the counterparty declined to
    /// co-sign. A strict superset of `auto_refresh_due`, which it calls as its route 1.
    DeadlineSafety,
}

/// The plan. **`DeadlineSafety` is unconditional and that is the whole point** — it is not gated on
/// `auto_refresh`, on `background_auto_refresh`, or on anything else. If a future change wants a
/// pass that IS conditional, add a variant and gate that one; do not put a condition on this.
pub fn maintenance_plan(_config: &crate::config::SdkConfig) -> Vec<MaintenancePass> {
    vec![MaintenancePass::DeadlineSafety]
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
            config.attestation_identity.clone(),
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
            version: RECOVERY_BUNDLE_VERSION,
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
        let bundle = parse_recovery_bundle(bundle_json)?;
        let cc = ClientConfig::from_params(
            config.statechain_entity_url.clone(),
            config.electrum_url.clone(),
            config.electrum_type.clone(),
            config.network,
            config.database_file.clone(),
            config.confirmation_target,
            config.attestation_identity.clone(),
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
    /// **[K>1 prerequisite 4] RECOVERABLE.** Issuance is counted over the parent's LIFETIME,
    /// spent rows included (`server/src/endpoints/deposit.rs`, `max_derived_tokens_per_statechain`),
    /// so a batch that is minted and then abandoned is *permanently* deducted from that parent's
    /// allowance. At K = 1 (2 slots per attempt) 31 failed attempts survive; at K = 20 (21 slots)
    /// only 2 do — the retry budget collapses exactly as the batch gets big enough to need one.
    ///
    /// So minted-but-unused vouchers are kept: they go into a durable pool the moment they are
    /// issued and leave it only when a slot is actually created from one
    /// ([`Self::create_child_slot`]). A retry therefore re-uses them and costs the allowance
    /// **nothing**. An unused voucher is still `unspent` at the SE, so it is a valid input to
    /// `deposit/init` however long it has sat there; a voucher that keeps failing is dropped after
    /// [`SLOT_VOUCHER_FAILURE_LIMIT`] attempts so a single poisoned id cannot wedge the pool.
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
        // [K>1 prerequisite 4] The SE refuses `count > cap` outright, so a batch that would exceed
        // the allowance must be refused HERE, by name, before anything irreversible — see
        // `refuse_oversized_slot_batch`.
        refuse_oversized_slot_batch(n)?;
        // [K>1 prerequisite 4] Vouchers left over from an earlier attempt first. They cost the
        // parent's lifetime allowance nothing, which is the whole point.
        let mut pool = self.slot_vouchers().await?;
        if pool.len() < n {
            let need = n - pool.len();
            let minted = self.mint_derived_tokens(parent_statechain_id, need).await?;
            pool.extend(minted.into_iter().map(|token_id| SlotVoucher { token_id, failures: 0 }));
            // Durable BEFORE they are handed out: a voucher that exists at the SE and nowhere on
            // disk is exactly the lifetime-allowance leak this pool exists to stop.
            self.save_slot_vouchers(&pool).await?;
        }
        Ok(pool.into_iter().take(n).map(|v| v.token_id).collect())
    }

    /// The SE round-trip half of [`Self::take_derived_tokens`] — mint `n` fresh derived vouchers, or
    /// fall back to onboarding when the SE genuinely does not offer derived issuance.
    async fn mint_derived_tokens(
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

    /// The durable pool of minted-but-unused derived-slot vouchers — see [`Self::take_derived_tokens`].
    pub(crate) async fn slot_vouchers(&self) -> Result<Vec<SlotVoucher>> {
        let rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await?;
        match rows.iter().find(|(k, _)| k == SLOT_VOUCHER_KEY) {
            // An unreadable pool row is an ERROR, never an empty pool: reading it as empty would
            // silently re-mint the batch and charge the parent's lifetime allowance twice.
            Some((_, json)) => serde_json::from_str(json)
                .map_err(|e| anyhow!("unreadable derived-slot voucher pool: {e}")),
            None => Ok(vec![]),
        }
    }

    async fn save_slot_vouchers(&self, pool: &[SlotVoucher]) -> Result<()> {
        mercuryrustlib::sqlite_manager::insert_raw_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            SLOT_VOUCHER_KEY,
            &serde_json::to_string(pool)?,
        )
        .await
    }

    /// Drop a voucher that has been SPENT (a slot was created from it). Called only on success —
    /// a voucher removed on failure would be a voucher paid for and thrown away, which is what this
    /// pool exists to prevent.
    pub(crate) async fn consume_slot_voucher(&self, token_id: &str) -> Result<()> {
        let mut pool = self.slot_vouchers().await?;
        let before = pool.len();
        pool.retain(|v| v.token_id != token_id);
        if pool.len() != before {
            self.save_slot_vouchers(&pool).await?;
        }
        Ok(())
    }

    /// Record that a voucher failed to become a slot, and drop it once it has failed
    /// [`SLOT_VOUCHER_FAILURE_LIMIT`] times.
    ///
    /// The count is what makes the pool self-healing. A voucher can fail for two reasons that look
    /// identical from here — a transient error (retry it, that is the whole point) or a token the SE
    /// has already consumed and this wallet failed to record (retrying it forever wedges every
    /// later batch behind a dead id). Counting attempts costs a transient failure two extra tries
    /// and bounds the poisoned case, without parsing an error string to tell them apart.
    pub(crate) async fn fail_slot_voucher(&self, token_id: &str) -> Result<()> {
        let mut pool = self.slot_vouchers().await?;
        if !record_voucher_failure(&mut pool, token_id) {
            return Ok(());
        }
        self.save_slot_vouchers(&pool).await
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
        // `execute_reporting_cancellations` + `fold_receive_outcome`, NOT the raising `execute`.
        // A cancelled incoming transfer used to `?` out right here, before `ClaimResult` was built
        // and before a single event was emitted — so one withdrawn payment discarded the whole
        // pass's DepositConfirmed / TransferClaimed / TokenTransferClaimed / BalanceUpdate for the
        // money that DID arrive. The cancellation is now reported (`cancelled_transfers` +
        // `WalletEvent::TransferCancelled`) rather than thrown; every OTHER error still propagates.
        let receive = fold_receive_outcome(
            mercuryrustlib::transfer_receiver::execute_reporting_cancellations(
                &self.inner.cc,
                &self.inner.config.wallet_name,
            )
            .await,
        )?;
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
            // [CTES-R] The BOOKED allocations, `outpoint -> Some((contract_id, amount))`, or
            // `Some(None)` for an outpoint holding more than one. Only fetched when the coloured
            // lane is enabled, so the plain path does not gain a single RGB read.
            //
            // `carriers` above is the UNION of booked and *pending* (consignment-bearing, not yet
            // booked) carriers. That union must be SPLIT here: `build_colored_tier` on a pending
            // carrier fails with `InvalidColoringInfo { available (0) }`, so a pending carrier stays
            // on the flat lane and is retried on the next pass, once its allocation is booked.
            //
            // AUDITED-SWALLOW: an unreadable allocation map degrades to "colour nothing this pass",
            // which is the pre-CTES-R behaviour — strictly less work, strictly more safety, and the
            // affected carriers are still recorded + surfaced as `RgbCarrier` below.
            let booked: std::collections::HashMap<String, Option<(String, u64)>> =
                if self.inner.config.colored_ladder && !rgb_state_unavailable {
                    self.token_carrier_allocations().await.unwrap_or_default()
                } else {
                    std::collections::HashMap::new()
                };
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
                // ── [RGB STAGE 0] A COIN WHOSE OWN BACKUP ROWS CARRY A CONSIGNMENT IS A CARRIER ───
                //
                // `is_token_carrier` below reads the ALLOCATION set, and at claim() time that set is
                // not yet populated for a freshly received carrier: this loop runs BEFORE
                // `book_incoming_token` → `accept_incoming_tokens` → `register_statechain`, so the
                // outpoint is not in `unspendable_as_btc_outpoints()` yet and the carrier reads as an
                // ordinary coin.
                //
                // What has been saving that is the ROOT-ONLY `f_on_chain` test further down — every
                // carrier a receiver sees today is funded by an UN-BROADCAST split tx, so
                // `transaction_get` fails and the coin is skipped. That is an accident of shape, not a
                // rule, and `auto_exit_due` (default ON) removes it: it MATERIALISES received carriers
                // near their deadline, putting the funding tx on chain. A materialised carrier then has
                // `f_on_chain == true`, `is_token_carrier == false`, `tesr::load == None` — and this
                // loop ladders an RGB carrier. That co-signs an un-timelocked trigger over `F` which
                // every past holder keeps forever ([B1] verbatim), violates terminal-freeze so every
                // later exit BURNS the asset, and propagates silently because the next hop sees a
                // ladder and flips to `protocol_version = 2`.
                //
                // The coin's own backup rows are authoritative at this instant and need no RGB
                // round-trip: a consignment on any row means the sender conveyed an allocation on this
                // coin. Checked BEFORE the allocation-set decision precisely because it does not
                // depend on booking having happened.
                //
                // FAIL CLOSED on an unreadable row, and note that absence and failure are DIFFERENT
                // here (`get_backup_txs` is `fetch_one`, so a missing row is an `Err`): a coin with no
                // rows is an ordinary coin and proceeds, while a database that will not answer must
                // not be read as "no consignment" — that reading is exactly how an RGB carrier gets
                // laddered.
                // **SCOPED TO THE CASE IT IS FOR: a carrier the allocation set does not KNOW is one.**
                // A RECOGNISED carrier must keep falling through to the decision site below, or the
                // CTES-R coloured lane — the whole point of which is to ladder carriers correctly —
                // could never be taken: a coloured carrier has consignment rows too, so an
                // unconditional skip here would permanently deny it the coloured ladder it is
                // entitled to. This check exists only to catch the coin the set has NOT booked yet.
                if !is_token_carrier(coin, &carriers) {
                    match crate::tokens::read_backup_rows(
                        &self.inner.cc.pool,
                        &self.inner.config.wallet_name,
                        &sid,
                    )
                    .await
                    {
                        Ok(Some(rows)) if rows.iter().any(|b| b.rgb_consignment.is_some()) => {
                            self.note_flat(&sid, 0, LadderSkipReason::RgbCarrier).await?;
                            continue;
                        }
                        Ok(_) => {}
                        Err(_) => {
                            self.note_flat(&sid, 0, LadderSkipReason::RgbStateUnavailable).await?;
                            continue;
                        }
                    }
                }

                // [CTES-R] THE DECISION SITE. A carrier either gets a COLOURED ladder — every tier
                // carrying a valid RGB state transition, so laddering MOVES the allocation instead
                // of destroying it — or it stays on the flat lane exactly as before.
                //
                // Fail CLOSED in every ambiguous case: the coloured lane is taken ONLY when the
                // outpoint resolves to exactly ONE booked allocation. Zero (a pending carrier whose
                // consignment is not booked yet) or more than one (no single-transition tier shape
                // exists) both fall through to `note_flat(RgbCarrier)` + `continue`, i.e. today's
                // behaviour, retried next pass.
                let mut colored_target: Option<(String, u64)> = None;
                if is_token_carrier(coin, &carriers) {
                    let one = coin_outpoint(coin).and_then(|o| booked.get(&o).cloned()).flatten();
                    match (self.inner.config.colored_ladder, one) {
                        (true, Some(alloc)) => colored_target = Some(alloc),
                        _ => {
                            self.note_flat(&sid, 0, LadderSkipReason::RgbCarrier).await?;
                            continue;
                        }
                    }
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
                        // [D69] CLASSIFY, don't fold. `get_statechain_info` now also fails when this
                        // build has no pinned enclave attestation identity and none is configured —
                        // a PERMANENT local fault that `coordinator-unavailable` would mislabel as
                        // "retry later". Re-resolving the pin decides which it is, by asking the
                        // same function rather than matching on a message.
                        let reason = match mercurylib::tesr::TesrParams::attestation_identity(
                            &self.inner.cc.network.to_string(),
                            self.inner.cc.attestation_identity.as_deref(),
                        ) {
                            Ok(_) => LadderSkipReason::CoordinatorUnavailable,
                            Err(_) => LadderSkipReason::AttestationIdentityUnpinned,
                        };
                        self.note_flat(&sid, 0, reason).await?;
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
                // ONE call site, TWO builders. A plain coin takes `establish_auto` unchanged — the
                // plain path is byte-identical, down to the RGB engine never being opened. A carrier
                // with exactly one booked allocation takes the COLOURED sibling, whose only
                // differences are the opret, the payload at vout 1 and the `n_payload + 1` fee.
                let was_colored = colored_target.is_some();
                let established = match colored_target {
                    Some((ref contract_id, amount)) => {
                        // TWO PHASES. Colour first (engine only, no network), then co-sign (network
                        // only, no engine). The RGB engine's resolver is `!Sync`, so its guard must
                        // NOT be alive across an `await` or the whole `claim()` future stops being
                        // `Send` and cannot run in the background watcher's task — hence the block.
                        // It is free: a tier's txid is stable across signing.
                        let draft = {
                            let mut rgb = self.rgb().await?;
                            let w = rgb.as_mut().ok_or_else(|| {
                                anyhow!("colored ladder requested but no RGB engine is configured")
                            })?;
                            tokio::task::block_in_place(|| {
                                mercuryrustlib::tesr::build_colored_ladder_auto(
                                    w,
                                    coin,
                                    &payee,
                                    &network,
                                    contract_id,
                                    amount,
                                    &f_spk_hex,
                                )
                            })
                        };
                        match draft {
                            // `cosign_tier` is the same blind SE round-trip either way — the SE
                            // never learns that anything is coloured.
                            Ok(draft) => {
                                mercuryrustlib::tesr::cosign_colored_ladder(
                                    &self.inner.cc,
                                    coin,
                                    draft,
                                )
                                .await
                            }
                            Err(e) => Err(e),
                        }
                    }
                    None => {
                        mercuryrustlib::tesr::establish_auto(&self.inner.cc, coin, &payee, &network)
                            .await
                    }
                };
                match established {
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
                    //
                    // [CTES-R] A CARRIER that could not be coloured — most often because it cannot
                    // afford three coloured rungs (`colored_ladder_floor`; every existing 1,500-sat
                    // token piece is in that class) — is recorded as `RgbCarrier`, NOT
                    // `EstablishFailed`. The distinction is load-bearing: `RgbCarrier` licenses flat
                    // conveyance, so the carrier keeps working exactly as it does today, whereas
                    // `EstablishFailed` licenses nothing and would leave it untransferable. The
                    // coloured attempt refuses BEFORE its first SE co-sign, so nothing was spent.
                    Err(e) => {
                        if was_colored {
                            eprintln!(
                                "coin {sid}: colouring the ladder failed ({e}); leaving the carrier \
                                 on the flat lane"
                            );
                            self.note_flat(&sid, 0, LadderSkipReason::RgbCarrier).await?;
                        } else {
                            self.note_flat(&sid, 0, LadderSkipReason::EstablishFailed).await?;
                        }
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
        // A withdrawn payment is money the user was expecting and will not get. It is reported on
        // its own channel because it is otherwise indistinguishable from silence: the receiving slot
        // stays empty either way. Emitted unconditionally, before the balance event, so an app that
        // only watches events still learns about it.
        if !receive.cancelled_statechain_ids.is_empty() {
            let _ = self.inner.events_tx.send(WalletEvent::TransferCancelled {
                statechain_ids: receive.cancelled_statechain_ids.clone(),
            });
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
            cancelled_transfers: receive.cancelled_statechain_ids.clone(),
        })
    }

    /// Withdraw an opened, unclaimed transfer so the coin is spendable again.
    ///
    /// The coordinator's pending-transfer lock is what stops a sender from co-signing a rival state
    /// while a recipient holds claimable material, so this is not a power the sender simply has. The
    /// rule is: if the mailbox message was never posted, the sender alone may withdraw it; once it
    /// was posted, the RECORDED recipient must co-sign. This method supplies that co-signature
    /// automatically only when this wallet holds the recipient key (a self-addressed transfer, or
    /// one to another address in the same wallet).
    ///
    /// When the recipient is someone else the call returns
    /// [`mercuryrustlib::transfer_sender::CancelNeedsRecipientConsent`], which carries the recipient
    /// auth key: hand it to the recipient, have them call [`Self::cancel_consent`], and finish with
    /// [`Self::cancel_transfer_with_consent`]. There is no force flag — one would reopen the
    /// two-victim break (convey to Bob, cancel, convey to Carol) the rule exists to close.
    ///
    /// On success the coin is restored to CONFIRMED and is selectable for a new spend.
    pub async fn cancel_transfer(&self, statechain_id: &str) -> Result<CancelOutcome> {
        let _guard = self.inner.wallet_lock.lock().await;
        let outcome = mercuryrustlib::transfer_sender::cancel(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?;
        self.after_cancel().await?;
        Ok(outcome)
    }

    /// Inspect a cancellation this wallet is being asked to CONSENT to, without signing anything.
    ///
    /// Call this before [`Self::cancel_consent`] and show the human the result. Consent to
    /// cancelling a conveyed transfer is consent to giving a payment back, and the amount, the coin
    /// and the colour are exactly the facts a person needs in order to give it meaningfully.
    ///
    /// Every field is established locally, by decrypting the mailbox message with this wallet's own
    /// key — nothing in it is asserted by the party asking. It errors, by name
    /// (`mercuryrustlib::transfer_sender::ConsentUnavailable`), when this wallet is not the recorded
    /// recipient or has already claimed the transfer, so a caller that previews successfully knows
    /// the signing call will not refuse for those reasons either.
    ///
    /// Read-only: takes no wallet lock and changes nothing.
    pub async fn preview_cancel_consent(
        &self,
        statechain_id: &str,
    ) -> Result<CancelConsentRequest> {
        mercuryrustlib::transfer_sender::preview_cancel_consent(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await
    }

    /// EVERY transfer this wallet could consent to cancelling.
    ///
    /// The strongest answer to a misdirected consent request, because the party asking names
    /// nothing: the recipient enumerates its own mailbox and picks. A request that describes a small
    /// coin has to match an entry here, and the entry carries the real branch-validated amount.
    ///
    /// Read-only. Transfers this wallet already claimed are omitted — they are not cancellable.
    pub async fn preview_all_cancellable_consents(&self) -> Result<Vec<CancelConsentRequest>> {
        mercuryrustlib::transfer_sender::preview_all_cancellable_consents(
            &self.inner.cc,
            &self.inner.config.wallet_name,
        )
        .await
    }

    /// Recipient half of a cooperative cancellation: the single-use consent token to hand back to
    /// the sender out of band.
    ///
    /// Read-only — it signs a fresh challenge and changes nothing locally.
    ///
    /// Note what it takes: the OBJECT returned by [`Self::preview_cancel_consent`], and nothing a
    /// counterparty can supply. There is no coin id and no recipient key in this signature, because
    /// a caller that supplies both can describe one transfer while naming another and the wallet
    /// would sign without ever showing an amount. `CancelConsentRequest`'s fields are private and
    /// its only constructor derives all of them — the amount and the binding together — from one
    /// message this wallet decrypted, so it cannot be assembled to say something that was never
    /// previewed.
    ///
    /// What was previewed is therefore what is signed: this call does not look at the mailbox again.
    /// If the transfer moved in between, the coordinator refuses the superseded consent rather than
    /// this wallet signing something it never showed.
    ///
    /// The token authorizes EXACTLY ONE cancellation of EXACTLY THIS transfer — single-use nonce,
    /// plus a digest binding it to the conveyed material currently in this wallet's hands. Without
    /// the second half a sender could take the consent, re-address the coin to this same receiving
    /// key (which the coordinator permits), let the replacement appear in this wallet's mailbox, and
    /// spend the consent against the transfer the recipient now believes is live.
    pub async fn cancel_consent(&self, approved: &CancelConsentRequest) -> Result<String> {
        mercuryrustlib::transfer_sender::cancel_consent(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            approved,
        )
        .await
    }

    /// Sender half of a cooperative cancellation, carrying a consent token obtained out of band from
    /// the recipient's [`Self::cancel_consent`]. This is what makes a cross-wallet cancellation of a
    /// CONVEYED transfer possible at all.
    ///
    /// `consent_token` is the whole opaque string the recipient produced. Do not split it: the
    /// signature and the statement of which transfer it covers travel together on purpose, and a
    /// token stripped back to the older two-field shape is refused (locally, and by the
    /// coordinator).
    ///
    /// On success the coin is restored to CONFIRMED and is selectable for a new spend.
    pub async fn cancel_transfer_with_consent(
        &self,
        statechain_id: &str,
        recipient_auth_pub_key: &str,
        consent_token: &str,
    ) -> Result<CancelOutcome> {
        let _guard = self.inner.wallet_lock.lock().await;
        let outcome = mercuryrustlib::transfer_sender::cancel_with_consent(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
            recipient_auth_pub_key,
            consent_token,
        )
        .await?;
        self.after_cancel().await?;
        Ok(outcome)
    }

    /// Re-sync statuses and tell the app the balance moved, after a successful cancellation.
    ///
    /// `update_coins` is what makes the released coin actually SPENDABLE from this wallet: the
    /// client layer restores IN_TRANSFER -> CONFIRMED locally, and this reconciles it against the
    /// coordinator so selection, `defend_ladders` and `get_balance` all see a live coin again.
    async fn after_cancel(&self) -> Result<()> {
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let after = self.record().await?;
        let _ = self.inner.events_tx.send(WalletEvent::BalanceUpdate {
            balance: compute_balance(&after),
        });
        Ok(())
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
    /// **[D31] Turn the configured fee source into a usable bump capability.**
    ///
    /// Returns the SIGNER and the CAPABILITY-BUILDER separately because `BumpCapability` borrows its
    /// signer: the caller keeps the signer alive on its own stack and hands a reference in. Clumsy,
    /// and the alternative — boxing the signer inside the capability — would put key material behind
    /// a trait object living for the whole pass, which is the opposite of what D31's bounded exposure
    /// asks for.
    ///
    /// ## The funding UTXO is SELECTED, not configured
    ///
    /// The first version of this took `funding_outpoint` and `funding_value` from config. That is
    /// wrong in a way that only shows up in production: **the outpoint is spent by the first bump**,
    /// so the config is stale from then on and every later rescue fails on a missing input — the
    /// tower works exactly once and then silently cannot pay, which is the failure D31 names.
    ///
    /// The fee ADDRESS is derived from the key instead, the float is read from the chain, and a
    /// suitable confirmed UTXO is chosen per rescue. Change returns to the same address, so the float
    /// is self-maintaining.
    ///
    /// `Ok(None)` = no fee source configured = **cannot bump**, the default. A malformed one is
    /// `Err`, not `None`: an operator who configured a fee wallet and got silence would reasonably
    /// believe their coins are covered.
    fn fee_bump_parts(
        &self,
    ) -> anyhow::Result<Option<(mercuryrustlib::tesr::P2trKeySpendBumpSigner, FeeBumpParts)>> {
        let Some(cfg) = self.inner.config.fee_bump.as_ref() else {
            return Ok(None);
        };
        let sk_bytes = hex::decode(&cfg.funding_secret_key_hex)
            .map_err(|e| anyhow::anyhow!("fee_bump.funding_secret_key_hex is not hex: {e}"))?;
        let sk = secp256k1_zkp::SecretKey::from_slice(&sk_bytes)
            .map_err(|e| anyhow::anyhow!("fee_bump.funding_secret_key_hex is not a key: {e}"))?;

        // The float address IS the key's P2TR key-spend address. Deriving it rather than configuring
        // it removes a way for the two to disagree — a mismatched pair would read the wrong float and
        // then fail to sign what it selected.
        let secp = secp256k1_zkp::Secp256k1::new();
        let kp = secp256k1_zkp::KeyPair::from_secret_key(&secp, &sk);
        let (xonly, _) = secp256k1_zkp::XOnlyPublicKey::from_keypair(&kp);
        let xonly = bitcoin::secp256k1::XOnlyPublicKey::from_slice(&xonly.serialize())
            .map_err(|e| anyhow::anyhow!("fee key is not a usable x-only pubkey: {e}"))?;
        let fee_addr = bitcoin::Address::p2tr(
            &bitcoin::secp256k1::Secp256k1::new(),
            xonly,
            None,
            self.inner.config.network,
        );
        let spk = fee_addr.script_pubkey();

        let float = mercuryrustlib::tower_float::TowerFloat::read(
            &self.inner.cc.electrum_client,
            spk.as_script(),
        )?;
        // Pessimistic, and deliberately so: size the requirement on the whole package at the target
        // rate. A tier that turns out cheaper simply leaves change.
        let needed = mercuryrustlib::tower_float::bump_cost_sats(
            cfg.target_fee_rate,
            TYPICAL_TIER_VSIZE,
        );
        let funding = float.select_funding(needed, spk.clone())?;

        let signer = mercuryrustlib::tesr::P2trKeySpendBumpSigner::new(sk);
        let parts = FeeBumpParts {
            core_rpc: mercuryrustlib::core_rpc::CoreRpcConfig::new(
                cfg.core_rpc_url.clone(),
                cfg.core_rpc_user.clone(),
                cfg.core_rpc_password.clone(),
            ),
            funding,
            change_script_pubkey: spk,
            target_fee_rate: cfg.target_fee_rate,
        };
        Ok(Some((signer, parts)))
    }

    /// **[D31] Is the fee float able to cover every coin this wallet defends?**
    ///
    /// Answers in BOTH units, because the interesting case passes one and fails the other: a float
    /// with plenty of sats in ONE utxo can fund exactly one simultaneous rescue, since a v3 fee child
    /// may have only one unconfirmed ancestor and the stuck tier is already it (measured —
    /// `live_tower_float.rs`).
    ///
    /// `Ok(None)` when no fee source is configured: a keyless wallet is not "underfunded", it is
    /// out of scope, and conflating the two would nag every ordinary user about a float they were
    /// never meant to have.
    pub async fn fee_float_solvency(
        &self,
    ) -> anyhow::Result<Option<mercuryrustlib::tower_float::Solvency>> {
        let Some(cfg) = self.inner.config.fee_bump.as_ref() else {
            return Ok(None);
        };
        let sk_bytes = hex::decode(&cfg.funding_secret_key_hex)
            .map_err(|e| anyhow::anyhow!("fee_bump.funding_secret_key_hex is not hex: {e}"))?;
        let sk = secp256k1_zkp::SecretKey::from_slice(&sk_bytes)
            .map_err(|e| anyhow::anyhow!("fee_bump.funding_secret_key_hex is not a key: {e}"))?;
        let secp = secp256k1_zkp::Secp256k1::new();
        let kp = secp256k1_zkp::KeyPair::from_secret_key(&secp, &sk);
        let (xonly, _) = secp256k1_zkp::XOnlyPublicKey::from_keypair(&kp);
        let xonly = bitcoin::secp256k1::XOnlyPublicKey::from_slice(&xonly.serialize())
            .map_err(|e| anyhow::anyhow!("fee key is not a usable x-only pubkey: {e}"))?;
        let spk = bitcoin::Address::p2tr(
            &bitcoin::secp256k1::Secp256k1::new(),
            xonly,
            None,
            self.inner.config.network,
        )
        .script_pubkey();

        let float = mercuryrustlib::tower_float::TowerFloat::read(
            &self.inner.cc.electrum_client,
            spk.as_script(),
        )?;
        // Obligations = the coins this wallet would actually have to defend, i.e. those carrying a
        // ladder. Counting every coin would overstate the float a tower needs and send an operator
        // shopping for sats they do not require.
        let obligations = self.laddered_coin_count().await;
        Ok(Some(float.assess(obligations, cfg.target_fee_rate, TYPICAL_TIER_VSIZE)))
    }

    /// Coins with a `tesr-` bundle — the ones a bump could ever be needed for.
    async fn laddered_coin_count(&self) -> usize {
        let Ok(rows) = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        else {
            // An unreadable DB must not report ZERO obligations — that is the answer that makes an
            // empty float look adequate.
            return usize::MAX;
        };
        rows.iter().filter(|(k, _)| k.starts_with("tesr-")).count()
    }

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
                // **[D40 / A.2] DEADLINE SAFETY IS UNCONDITIONAL. Routine re-anchoring is not.**
                //
                // These are two different jobs and they used to share one flag. Routine background
                // re-anchoring is an ECONOMICS choice — B4 folds the re-anchor cost into `transfer`
                // and pays it on demand, so a running wallet must not silently shrink a balance in
                // the background — and it stays opt-in. But a whole laddered coin carries one
                // absolute clock, `min(L_k)` over its flat chain, held by its PRIOR OWNERS; when
                // that height passes, any ancestor's matured rung spends `F` and takes the coin.
                //
                // That is a SAFETY property, and it sat behind the economics flag. The comment here
                // used to say "deadline safety for idle wallets is the `auto_exit` pass below" —
                // that is false: `auto_exit_due` protects sub-coins and materialises carriers. The
                // whole-coin clock had no scheduled defender at all on a default wallet.
                //
                // `deadline_safety_due` tries the cooperative re-anchor first and SEVERS FROM `F`
                // (broadcasts the already-co-signed trigger) when that fails — because the party
                // most interested in this deadline passing is the same party asked to co-sign the
                // re-anchor, and a defence its adversary can decline is not a defence (D40.1).
                // ═══ [D58] THE DEADLINE PASS RUNS ON EVERY PATH, NOT JUST THE `else` ═══
                //
                // This was an `if routine-maintenance { auto_refresh_due } else { deadline_safety_due }`,
                // so a wallet that OPTED INTO background maintenance got the COOPERATIVE half alone
                // and LOST THE SEVER. Enabling more maintenance bought less protection — the exact
                // inversion of what the flag reads as.
                //
                // `deadline_safety_due` is not the poor relation of `auto_refresh_due`: it CALLS it
                // first (the cooperative re-anchor is its route 1) and then severs whatever the
                // counterparty declined to co-sign. Running it unconditionally is therefore a strict
                // superset of the old `if` arm, not a duplicate pass — and it is what D40.1 requires,
                // because the party most interested in this deadline passing is the same party being
                // asked to co-sign the re-anchor.
                //
                // The `Err` is REPORTED, not discarded ([D51]): the pass now returns `Err` listing
                // every coin it could not defend, and `let _ =` here is what threw that away. A
                // background loop must not abort on it — the next tick should still run — so it is
                // logged rather than propagated, which is the one thing that makes an undefended coin
                // visible on a wallet nobody is watching.
                // [D66] The passes this tick will run are DECIDED as a value first, by
                // `maintenance_plan`, then executed. A source-scanning guard cannot prove "this runs
                // on every path" — `&&`, `if`, `match` and an early `return` all defeat it, and an
                // adversarial pass proved exactly that against the guard that claimed to (D64). A
                // pure function returning the plan CAN be proved, by executing it over every config.
                for pass in crate::wallet::maintenance_plan(&wallet.inner.config) {
                    match pass {
                        MaintenancePass::DeadlineSafety => {
                            if let Err(e) = wallet
                                .deadline_safety_due(wallet.inner.config.auto_refresh_margin_blocks)
                                .await
                            {
                                eprintln!("[deadline safety] {e:#}");
                            }
                        }
                    }
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

        // ---- [CTES-R] RECEIVED SPLIT CHILDREN — the ladder-lane form of the loop above -----------
        //
        // The loop above gates every carrier on a `branch-<id>` row, and a split child HAS none:
        // its exit material is the five-tier chain `T -> X_m -> SP -> ext_child -> state_child`
        // carried in its `ctesr-` bundle. `read_exit_branch` therefore answers VERIFIED-EMPTY for
        // it, which that loop reads as "issued/flat carrier, no clawback risk" — and for a RECEIVED
        // piece that is exactly wrong. The sender keeps one pre-signed, RGB-UNAWARE deposit backup
        // over the very funding output `F` the child's chain roots at; the moment it matures she can
        // spend `F` and the whole chain underneath it dies. On the flat lane that was a CLAWBACK
        // (she recovered the tokens); on this lane it only DESTROYS them — but the receiver loses
        // the allocation either way, and the answer is the same one `sdk34` has always tested:
        // spend `F` first.
        //
        // Two things differ from the flat lane, both forced by the shape of the walk:
        //
        //  * THE ACTION is `unilateral_exit`, not a branch broadcast. The child's chain IS its
        //    branch and every rung of it is RGB-aware; broadcasting any other spend of `F` destroys
        //    the allocation, which is precisely why the loop above must never be pointed at this
        //    coin. The walk is resumed by later passes as each relative timelock matures.
        //  * THE DEADLINE GETS A HEAD START. A flat branch is locktime-0 and lands in one block, so
        //    `L0` itself was a usable deadline. This walk is a chain of RELATIVE timelocks, so it
        //    must be STARTED at least `Σ csv` blocks before `L0` or it cannot finish in time. The
        //    head start is read off the bundle's own chain, never guessed.
        //
        // L1 — the same liveness allowlist `defend_ladders` applies: act only for a child this
        // wallet still holds CONFIRMED. A child conveyed onward belongs to its recipient, and
        // driving its chain would rival the state they now hold (the D1 class sdk79 pins).
        //
        // [D13] COVERS BOTH LANES. This loop once handled only COLOURED children, on the argument
        // that a plain child was "left to its own change". It was not left to anything — a plain
        // leaf had NO runtime deadline defence anywhere in the SDK, which under the RGB scope-out
        // is the whole normative protocol. Coloured and plain leaves have the identical exposure (a
        // pre-signed relative-timelock walk rooted at a funding output the splitter can still spend
        // via the parent's flat backup), so both are driven here. The one thing that made covering
        // plain rows unsafe — a leaf mid-split reading CONFIRMED with a stale row — is closed by the
        // split-journal guard below.
        let child_rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "near-deadline protection is BLIND on every adopted split child: the child-bundle \
                 rows could not be read ({e}) — refusing to report a protected wallet from a \
                 failed read"
            )
        })?;
        // [CATS/V4] Two row shapes, ONE protection. A coloured SPINE TIP is the sender's own change
        // leg and its allocation sits on the un-broadcast `SP.out[K]` — the identical exposure a
        // coloured child has to an ancestor's stale-backup clawback, differing only in whose money
        // it is and in the chain being one tier shorter. Keying this loop on `ctesr-` alone would
        // leave the sender's own coloured change as the single carrier class in the wallet with no
        // near-deadline protection at all.
        enum Row {
            Child(Box<mercuryrustlib::tesr::ChildTesrBundle>),
            Tip(Box<mercuryrustlib::tesr::SpineTipBundle>),
        }

        // [D13] THE SPLIT-JOURNAL GUARD — the one thing that makes covering PLAIN leaves safe.
        //
        // A leaf that has been TERMINALIZED by its own partial-payment split reads CONFIRMED, with a
        // stale `ctesr-`/`spinetip-` row still naming the state its CSP supersedes, for the whole
        // span from the CSP co-signature until the conveyance completes — and PERMANENTLY after any
        // error or crash in that span, because `child_in_ladder_split` does NOT park the coin's
        // status before its irreversible co-sign (unlike `child_retransfer` and `combine`), and
        // `resume_split_conveyance` repairs only the conveyed pieces, never the terminalized parent.
        // Force-exiting it here would broadcast `state_child`, rival the CSP over `ext_child.out[0]`,
        // and DESTROY the grandchild pieces already handed to payees.
        //
        // The journal is the durable evidence that closes this: it is written at `Planned` STRICTLY
        // BEFORE the co-signature, so any coin mid-split appears here before anything can be handed
        // out. Skip such a coin (report it as attention-needed, never drive it). An unreadable
        // journal is treated as blindness over EVERY child, never as permission — `journal_open_splits`
        // already errors on an unparseable row, and here that error must not become "no coin is
        // mid-split".
        // `None` = the journal could not be read, i.e. blindness over every child; `Some(set)` = the
        // set of statechain ids currently mid-split, which must not be driven.
        let terminalizing: Option<std::collections::HashSet<String>> =
            match mercuryrustlib::tesr::journal_open_splits(
                &self.inner.cc,
                &self.inner.config.wallet_name,
            )
            .await
            {
                Ok(recs) => {
                    Some(recs.into_iter().map(|r| r.terminalized_statechain_id).collect())
                }
                Err(e) => {
                    // Fail closed: without the journal we cannot prove any leaf is safe to drive.
                    blind.push(format!(
                        "near-deadline protection is BLIND on every adopted split child and spine \
                         tip: the split journal could not be read ({e}), so a leaf that is \
                         mid-split — which must NOT be force-exited — cannot be told apart from one \
                         that is safe to drive"
                    ));
                    None
                }
            };

        for (key, json) in child_rows.iter() {
            let (cid, what) = if let Some(cid) = key.strip_prefix("ctesr-") {
                (cid, "adopted split child")
            } else if let Some(tid) = key.strip_prefix(mercuryrustlib::tesr::SPINE_TIP_KEY_PREFIX) {
                (tid, "spine tip")
            } else {
                continue;
            };
            // L1. Absence from the record, or any status other than CONFIRMED, is a DECIDED answer
            // ("not ours to drive"), not blindness.
            let Some(coin) = record.coins.iter().find(|c| {
                c.duplicate_index == 0 && c.statechain_id.as_deref() == Some(cid)
            }) else {
                continue;
            };
            if coin.status != CoinStatus::CONFIRMED {
                continue;
            }
            // [D13] MID-SPLIT LEAVES ARE NOT OURS TO DRIVE. A CONFIRMED status here does NOT mean
            // "safe to force-exit": a leaf terminalized by its own split reads CONFIRMED with a
            // stale row, and driving it destroys the grandchild pieces its CSP already conveyed.
            match leaf_split_gate(&terminalizing, cid, what) {
                LeafSplitGate::Drive => {}
                // Journal unreadable — blanket blindness already recorded before the loop.
                LeafSplitGate::HoldSilently => continue,
                LeafSplitGate::Hold(msg) => {
                    blind.push(msg);
                    continue;
                }
            }
            let parsed = if key.starts_with("ctesr-") {
                serde_json::from_str(json).map(|c| Row::Child(Box::new(c)))
            } else {
                serde_json::from_str(json).map(|t| Row::Tip(Box::new(t)))
            };
            let parsed = match parsed {
                Ok(r) => r,
                // NOT a skip. An unparseable bundle is the one shape where "I could not tell" and
                // "nothing is due" look identical, and this coin's only protection is here.
                Err(e) => {
                    blind.push(format!(
                        "{cid} ({what}: its stored bundle will not parse ({e}), so its exit-race \
                         deadline cannot be computed)"
                    ));
                    continue;
                }
            };
            // [B1] THE **BOUND** CHAIN, NEVER THE DECLARED ONE.
            //
            // `child_exit_chain`'s own doc forbids what this loop used to do: "Anything that
            // computes a requirement, a deadline or a cap from these timelocks must use
            // `child_exit_chain_bound`" (`clients/libs/rust/src/tesr.rs:5489-5493`). The `csv` field
            // is plain serde and, on conveyed material, attacker-supplied. This loop computes a
            // DEADLINE from it, which is exactly the prohibited use. Binding parses each SIGNED
            // transaction and reads the timelock out of its `nSequence`, refusing by name if the two
            // disagree — so a sender cannot hand this tower a schedule of their choosing and move
            // the moment their victim's coin defends itself.
            // [D13-follow-up] AND IT MUST PAY US. A CONFIRMED status does not imply the stored row
            // pays this wallet: a failed child conveyance leaves the coin IN_TRANSFER with a
            // PAYEE-paying row, and `/transfer/cancel` lifts that back to CONFIRMED. Driving it
            // would force-exit the sender's own money into the payee's address. Reported, never
            // silently skipped — a false positive here means declining to defend a coin we DO own,
            // and that must be visible rather than quiet.
            //
            // [#133] THE ROOT CAUSE IS NOW FIXED, AND THIS CHECK STAYS ANYWAY. The cancel path runs
            // `reclaim_cancelled_child_conveyance` (`rust/src/tesr.rs`), which re-points the row at
            // the owner and discloses the orphaned co-sign — so a cancellation no longer strands a
            // payee-paying leaf. This remains because the repair can still FAIL (the SE refuses, the
            // rung is at the floor, the process dies mid-cancel) and because rows written before it
            // existed are still on disk. It is the difference between "the fix worked" and "we
            // assume the fix worked", and it is the one standing between a stranded leaf and paying
            // a stranger.
            // CHILDREN ONLY: a spine tip is the sender's own change leg and carries no
            // `child_owner_exit_address` — there is no conveyance-to-a-payee to be resurrected, so
            // the window does not exist for it.
            if let Row::Child(cb) = &parsed {
                let exit_addr = &cb.child_owner_exit_address;
                if !leaf_exit_pays_this_wallet(exit_addr, &record) {
                    blind.push(format!(
                        "{cid} ({what}): its stored exit pays {exit_addr}, which is not an address \
                         this wallet holds — refusing to drive it. Either a conveyance to that payee \
                         was cancelled without the row being re-pointed at the owner (exiting would \
                         hand them the coin), or this wallet's record is incomplete. Not driven, \
                         and not treated as idle"
                    ));
                    continue;
                }
            }

            // [D13] BOTH LANES, NOT ONLY COLOURED. This loop used to gate on `is_colored()` and drop
            // every plain child and plain tip with `_ => continue`. That left a PLAIN leaf with no
            // runtime deadline defence anywhere in the SDK — and under the RGB scope-out the plain
            // lane was to be the whole normative protocol. The exposure is real: a leaf carries no
            // flat backup of its own, so its clock is the ABSOLUTE height `min(L_k)` of the PARENT's
            // flat backups (the lowest rung belongs to the splitter — the adversary), and its exit
            // is a chain of RELATIVE timelocks that must be STARTED `Σ(csv+1)` blocks before that
            // height. Reacting to F being spent is too late by construction. The only thing that made
            // covering plain rows unsafe — a mid-split leaf reading CONFIRMED with a stale row — is
            // handled by the split-journal guard above; everything past it is safe to drive.
            let chain = match &parsed {
                Row::Child(cb) => mercuryrustlib::tesr::child_exit_chain_bound(cb),
                Row::Tip(tip) => mercuryrustlib::tesr::spine_tip_exit_chain_bound(tip),
            };
            let chain = match chain {
                Ok(c) => c,
                // NOT a skip. A chain whose timelocks cannot be bound to the signatures that
                // enforce them is a chain whose deadline cannot be computed, and this coin's only
                // protection is here.
                Err(e) => {
                    blind.push(format!(
                        "{cid} ({what}: its exit chain's timelocks could not be bound to \
                         the signatures enforcing them ({e}), so no deadline can be derived)"
                    ));
                    continue;
                }
            };
            if chain.is_empty() {
                blind.push(format!(
                    "{cid} ({what} has an EMPTY exit chain — it has no walk to protect \
                     it and no deadline can be derived)"
                ));
                continue;
            }
            // THE HEAD START — every block the walk must sit through before its last tier can
            // confirm. `exit_wait_blocks`, NOT a hand-rolled sum of the timelocks.
            //
            // The previous expression here was `chain.iter().filter_map(|(_, csv)| *csv).sum()`,
            // and it under-counted twice over: it charged each tier only its own CSV, omitting THE
            // ONE BLOCK ITS PARENT NEEDS TO CONFIRM, and `filter_map` silently dropped any tier
            // whose csv is `None` rather than charging it that block. So the head start was short
            // by at least the tier count, and this loop fired LATE by that many blocks.
            //
            // Late is the FAIL-OPEN direction on the one number the whole defence rests on. Worse,
            // it silently disagreed with `check_exit_headroom` — the claim-time gate that admitted
            // the coin in the first place, which uses `exit_wait_blocks`
            // (`lib/src/transfer/receiver.rs:720-722`). Two sites deciding the same quantity two
            // different ways, with the watchtower on the losing side. Calling the same function is
            // what makes them provably agree, rather than agreeing by inspection until someone
            // edits one of them.
            let csvs: Vec<Option<u16>> = chain.iter().map(|(_, csv)| *csv).collect();
            let head_start = mercurylib::transfer::receiver::exit_wait_blocks(&csvs);
            // [audit-17] READ `L_k` OFF THE SIGNED BACKUPS, do not recompute `L_0` from the chain.
            //
            // This used to call `deposit_anchored_deadline_of_root_tx`, which finds the deposit's
            // confirmation height and returns `h_deposit + initlock` — that is `L_0`, the k = 0 case
            // and the LATEST the ladder can be. The real deadline is `L_k = L_0 − k·interval` for a
            // parent transferred `k` times before the split, and nothing conveys `k`. So the number
            // was too LATE by `k·interval`, i.e. fail-open: this coin believed it had more time than
            // it had, and `AUDIT_17_K_MAX = 14` was a guess at `k` whose own comment called it the
            // weakest term in the margin.
            //
            // `k` never needed conveying. Each backup's nLockTime IS its rung, and the whole chain
            // is already conveyed, already signature-verified and already count-pinned by the
            // exact-equality census (see `epoch_deadline_from_flat_backups`). The minimum over it is
            // `L_k` exactly.
            //
            // It is also CHEAPER than what it replaces: two electrum round-trips and an
            // `/info/config` per coin per pass are gone, and with them a whole class of watchtower
            // blindness — a pass that could not reach the chain used to report every coin as
            // undecidable, and now reads a field it already holds.
            let backups = match &parsed {
                Row::Child(cb) => &cb.parent_flat_backups,
                Row::Tip(tip) => &tip.parent_flat_backups,
            };
            let deadline = match mercuryrustlib::tesr::epoch_deadline_from_flat_backups(backups) {
                Ok(d) => d.saturating_sub(head_start),
                Err(e) => {
                    blind.push(format!(
                        "{cid} ({what}: its exit-race deadline could NOT be computed \
                         ({e}) — it has a pre-signed chain rooted at a funding output an ancestor \
                         can still spend, so absence of a deadline here means blindness, not safety)"
                    ));
                    continue;
                }
            };
            if tip + margin_blocks < deadline {
                continue; // still comfortably ahead of the head-started deadline
            }
            // [D13] Report the RIGHT event for the coin's kind. A coloured row settles a token
            // allocation (TokenCarrierMaterialized); a plain leaf is driven to L1 to beat its
            // deadline (LeafExitForced). Emitting the token event for a plain coin would mis-report
            // it to any integrator watching the stream.
            let colored = match &parsed {
                Row::Child(cb) => cb.is_colored(),
                Row::Tip(tip) => tip.is_colored(),
            };
            let _ = self.inner.events_tx.send(near_deadline_exit_event(
                colored,
                cid.to_string(),
                deadline,
                tip,
            ));
            match self.unilateral_exit(Some(vec![cid.to_string()]), None).await {
                Ok(_) => exited.push(cid.to_string()),
                Err(e) => blind.push(format!(
                    "{cid} ({what} is DUE at block {deadline}, tip {tip}, but driving \
                     its exit walk failed: {e})"
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
        // [RE-ANCHOR] Coins whose `F` was spent by something OTHER than their own trigger. Kept in
        // its own vector, never merged into `blind`, because the two mean opposite things to a
        // caller: `blind` is "retry, I could not see" and clears on the next good pass; this is
        // "I saw, and every tier below the trigger is permanently unconfirmable". Merging them let
        // `note_watchtower_ok` erase a permanent loss the moment any other coin's pass succeeded.
        let mut lost: Vec<String> = Vec::new();
        // ---- [B2] WHAT THIS PASS MUST NOT BROADCAST -------------------------------------------
        //
        // `defend_ladders` is a BROADCAST path. It drove every coin that had a `tesr-` row,
        // including coins this wallet had already spent or given away — and a RETAINED ladder is a
        // rival spend of the very same payload output as the state its recipient now holds. An
        // in-ladder split terminalizes the parent with `SP` and supersedes `S_0`; both spend
        // `X_m.out[0]`, so a tower still driving `S_0` does not defend anything, it RACES the child
        // it just paid, and on the coloured lane the child's allocation lives on `SP`. A WHOLE-coin
        // conveyance is the same shape one level up: `transfer_colored_carrier` hands the recipient
        // a receiver-paying state `S'` over the same `X_m.out[0]`, and the sender keeps a co-signed
        // `S_sender` that rivals it.
        //
        // The first round closed only the SPLIT lane, with two filters: a DENYLIST on
        // WITHDRAWN/WITHDRAWING, and a supersession check driven by this wallet's `ctesr-` rows.
        // Neither closes the WHOLE-CARRIER CONVEYANCE lane. A whole-coin handover writes no
        // `ctesr-` row at all (there is no child), and `transfer_sender::execute*` leaves the
        // sender's coin IN_TRANSFER — which the denylist admits — and `coin_status::update_coins`
        // later promotes that to TRANSFERRED, which the denylist admits too. Same theft, different
        // route, and enumerating routes is exactly what failed.
        //
        // So the filter no longer enumerates lanes. It keys on evidence this tower can check for
        // itself, for every coin, on every lane that exists or will exist:
        //
        //  L1 — LIVENESS, AS AN ALLOWLIST (not a denylist). Broadcast only for a coin whose status
        //       is CONFIRMED. CONFIRMED is this wallet's own record of "mine, unspent, no
        //       counterparty holds anything over it", and every route that hands value away must
        //       move a coin OUT of it before the counterparty can hold rival material:
        //       `transfer_sender::execute`/`execute_colored` → IN_TRANSFER → TRANSFERRED,
        //       `withdraw` → WITHDRAWING → WITHDRAWN, the in-ladder split and `child_retransfer` →
        //       WITHDRAWN, an LN-latched piece → IN_TRANSFER. The allowlist form is the whole
        //       point: a lane added tomorrow parks its coin in SOME non-CONFIRMED status and is
        //       refused BY DEFAULT. It cannot re-arm this tower against its own recipient by
        //       forgetting to extend a list. (`unilateral_exit` already holds the same rule for the
        //       same coins — "exiting a withdrawn parent would invalidate the sub-coins funded by
        //       its split [B1]" — and this pass broadcasts the same transactions, so it must hold
        //       the same rule.) Nothing is left undefended by the refusal: a conveyed coin's
        //       segment is driven by the party that now owns it — `watch_child_pass` replays
        //       `T -> X_m -> SP` underneath a child chain, and a whole-carrier recipient's own
        //       `tesr-` row drives `T -> X_m -> S'`.
        //
        //       [A2 — THE KEY MUST BE DURABLE.] L1 is only as good as the field it reads, and it
        //       reads that field FROM THE WALLET DB on every pass. `create_backup_transactions`
        //       used to set `coin.status = IN_TRANSFER` in MEMORY only, on a copy the conveyance
        //       did not write back until `update_wallet` — the last statement of
        //       `transfer_sender::execute_ex`, long after `transfer/update_msg` had handed the
        //       recipient a co-signed `S'`. So the filter was keyed on precisely the field that was
        //       not yet on disk, and a pass landing in that window read a stale CONFIRMED and was
        //       admitted. Both conveyance lanes now make the transition DURABLE **before** the
        //       first step that can produce material for anybody else — `execute_ex` before the
        //       `S'` co-sign, `tesr::{child_retransfer, cosign_colored_child_retransfer}` before
        //       the `S'_child` co-sign — and restore it if they abort before anything is conveyed.
        //       Durable local evidence was chosen over asking the coordinator whether a transfer is
        //       open: a network round-trip on a per-block broadcast path would read a coordinator
        //       outage as blindness (disarming the tower during exactly the outage a griefer would
        //       exploit), and a coordinator that answered "no transfer is open" would induce this
        //       wallet to destroy its own recipient's allocation. sdk79 PART C races a real,
        //       lethal watchtower against a whole-coin conveyance; PART D does the same for the
        //       child re-transfer.
        //  L2 — SUPERSESSION. A coin can stay CONFIRMED and still have had its state replaced in
        //       place. If any child bundle in this wallet names this coin as its parent and carries
        //       a DIFFERENT current parent state than the `tesr-` row does, the row is provably
        //       stale — the child's copy is the one that was conveyed and co-signed last.
        //       `colored_in_ladder_pay` writes the terminalized segment back so the row names `SP`
        //       and this tower becomes the child's ALLY, but a watchtower must not depend on a
        //       write having succeeded. Reported as blindness rather than acted on: a tower that
        //       cannot tell which of two rival states is live must broadcast neither.
        let child_rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "ladder defence is BLIND on every adopted split child: the child-bundle rows could \
                 not be read ({e}) — refusing to report a defended wallet from a failed read"
            )
        })?;
        // L1's evidence in lookup form, for the second broadcast loop below (which iterates DB rows
        // rather than coins and so has no `Coin` in hand): the statechain ids this wallet can still
        // prove are its own live, unspent coins. Everything else — conveyed, in flight, withdrawn,
        // invalidated, or absent from the record entirely — is refused by default. Same predicate
        // the root loop applies directly to `c.status`.
        let live_sids: std::collections::HashSet<String> = record
            .coins
            .iter()
            .filter(|c| c.duplicate_index == 0 && c.status == CoinStatus::CONFIRMED)
            .filter_map(|c| c.statechain_id.clone())
            .collect();
        let mut conveyed_parent_states: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        for (key, json) in child_rows.iter() {
            // A row that will not parse is handled (loudly) by the loops below; here it simply
            // contributes no supersession evidence, which leaves L1 in charge.
            if key.starts_with("ctesr-") {
                if let Ok(cb) = serde_json::from_str::<mercuryrustlib::tesr::ChildTesrBundle>(json) {
                    conveyed_parent_states
                        .entry(cb.parent_statechain_id.clone())
                        .or_default()
                        .insert(cb.parent.current().state.txid.clone());
                }
                continue;
            }
            // [CATS/V4] **A SPINE TIP is the same evidence about the same fact**, and this is the
            // one place where omitting it would REMOVE an existing defence rather than fail to add
            // one. L2 fires only when this map has an entry for the parent. Today the sender's own
            // change child leaves a `ctesr-` row, so a root split always populates it. Under change
            // 2 that row becomes a `spinetip-` row — and if only the `ctesr-` prefix were read, a
            // sender who conveyed every piece would end with an EMPTY map for that parent, L2 would
            // stop firing, and the root loop would drive the wallet's stale `tesr-` row, racing the
            // very children it had just conveyed. That is the exact accident L2 was written to stop.
            if key.starts_with(mercuryrustlib::tesr::SPINE_TIP_KEY_PREFIX) {
                if let Ok(tip) = serde_json::from_str::<mercuryrustlib::tesr::SpineTipBundle>(json) {
                    conveyed_parent_states
                        .entry(tip.parent_statechain_id.clone())
                        .or_default()
                        .insert(tip.parent.current().state.txid.clone());
                }
            }
        }
        for c in &record.coins {
            if c.duplicate_index != 0 {
                continue;
            }
            let Some(id) = c.statechain_id.clone() else { continue };
            // L1 — LIVENESS ALLOWLIST. Anything that is not CONFIRMED is not provably ours, and
            // therefore not ours to broadcast. This is a DECIDED answer, not blindness: a wallet
            // with one pending transfer must not report a permanently blind watchtower, and the
            // coin is not left undefended — whoever now holds the live state defends it.
            if c.status != CoinStatus::CONFIRMED {
                continue;
            }
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
            // L2 — a child of ours disagrees with this row about which parent state is live.
            if let Some(conveyed) = conveyed_parent_states.get(&id) {
                let local = bundle.current().state.txid.clone();
                if !conveyed.contains(&local) {
                    blind.push(format!(
                        "{id} (this wallet's `tesr-` row still names state {local} as live, but the \
                         split child(ren) it funded were conveyed with {conveyed:?} as the parent's \
                         current state — the local row is SUPERSEDED, and broadcasting it would \
                         race those children rather than defend them, destroying a coloured child's \
                         allocation. Refusing to act on it; re-store the terminalized parent segment)"
                    ));
                    continue;
                }
            }
            // F4: `watch_pass` now reports whether it could SEE. An unreadable chain backend is
            // `Blind`, NOT an empty "nothing to do" — treating the two alike is what let a dead
            // electrum connection masquerade as a quiet, well-defended coin.
            // [D31] Bump only if the owner configured a fee source. Absent = the keyless pass,
            // which is what this has always been; `watch_pass` then reports a fee-stuck tier as a
            // stated limit rather than as one more retryable failure.
            let bump_parts = self.fee_bump_parts()?;
            let watched = match bump_parts.as_ref() {
                Some((signer, parts)) => mercuryrustlib::tesr::watch_pass_with_bump(
                    &self.inner.cc.electrum_client,
                    &bundle,
                    &parts.with(signer),
                ),
                None => mercuryrustlib::tesr::watch_pass(&self.inner.cc.electrum_client, &bundle),
            };
            match watched {
                mercuryrustlib::tesr::WatchState::Idle => {} // F unspent: verifiably nothing to do
                // [HIGH / F2+F4 join] `Acted { ids, .. }` DISCARDED `failures`. That field exists
                // precisely so a caller can tell a ladder that is WAITING (its next tier's
                // relative-CSV has not matured — the normal steady state of a contested exit) from
                // one that is being RACED or is dead (the tier's input was spent by a competing tx,
                // the backend rejected it, the stored hex is unusable). Both looked identical:
                // `ids` empty, no event, no fault, pass returns `Ok`. Classify, and treat anything
                // that is not a recognised "not yet" as blindness — an owner whose exit is being
                // out-raced learns about it on the NEXT pass instead of never.
                mercuryrustlib::tesr::WatchState::Acted { ids, failures, blind: unseen } => {
                    if !ids.is_empty() {
                        let _ = self.inner.events_tx.send(WalletEvent::LadderDefended {
                            statechain_id: id.clone(),
                            tiers_broadcast: ids.len() as u32,
                        });
                        acted.push(id.clone());
                    }
                    // Per-entry blindness. This tower passes ONE bundle, so today the list is always
                    // empty — bound and merged rather than `..`-ignored so that if a future pass can
                    // report it, it lands in the fault machinery instead of being dropped by a
                    // wildcard nobody revisits.
                    blind.extend(unseen);
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
                // [RE-ANCHOR] `Void` is NOT blindness, and must never be merged into it.
                // `blind` means "retry, I could not see"; this means "I saw, and the coin is gone".
                // Something other than this coin's own trigger spent `F`, so every tier below it is
                // permanently unconfirmable. Merging the two would let `note_watchtower_ok` clear a
                // permanent loss the moment some other coin's pass happened to succeed — which is
                // exactly how this used to disappear.
                mercuryrustlib::tesr::WatchState::Void { spender, detail } => {
                    lost.push(format!("{id} (spent by {spender} — {detail})"));
                }
            }
        }

        // ---- ADOPTED SPLIT CHILDREN (`ctesr-` rows) -------------------------------------------
        //
        // [CTES-R] **This is the pass that protects a coloured carrier, and `auto_exit_due` is not.**
        //
        // The instruction was "open `auto_exit_due` to coloured carriers". The evidence says the
        // other pass is the right one, so this is where the coverage went:
        //
        //  * `auto_exit_due` acts on `ExitCostEstimate::exit_deadline_block`, which is defined only
        //    for a coin with an off-chain ANCESTOR that could broadcast a stale ABSOLUTE-locktime
        //    backup — "`None` for a flat on-chain coin (no off-chain ancestor can race you)". A
        //    coloured ladder is root-only by construction (the claim pass refuses to ladder a coin
        //    whose `F` is not on chain, [B0]), so its `read_exit_branch` is verifiably empty and its
        //    deadline is genuinely `None`. There is no height at which anything expires: an idle
        //    laddered coin never ages. Opening `auto_exit_due` to it would not compute a deadline —
        //    it would have to invent one, and then act on it by broadcasting a three-tier exit that
        //    converts a live off-chain coin into on-chain sats for no reason. That is the opposite
        //    of protection.
        //  * What can actually hurt a coloured coin is someone SPENDING `F` — a prior owner or
        //    griefer broadcasting `T` — and, for a child, the parent's retained `S_0` racing the
        //    child's `SP` over `X_m.out[0]`. Both are reactive, per-block races that the owner wins
        //    because the adopted state carries the strictly-lowest CSV. Racing them is exactly what
        //    this pass does, and only this pass does it.
        //
        // Coloured ROOT carriers were already covered: their ladder lives in a `tesr-` row and the
        // loop above does not care whether a bundle is coloured. Children were not covered at all —
        // by anything — which is the real gap the instruction was pointing at, since the coloured
        // in-ladder split pays its recipient in exactly this shape. Plain children get the same
        // defence for free; they had the same hole.
        //
        // [D1] …and this loop had NO liveness filter of any kind — not even the denylist the root
        // loop carried. A `ctesr-` row is not deleted when the child leaves: `child_retransfer` /
        // `cosign_colored_child_retransfer` overwrite it with the bundle they conveyed, and the
        // sender's piece slot is left WITHDRAWN. That is safe only for exactly one hop. The moment
        // the RECIPIENT re-transfers the child onward, this wallet is holding a co-signed child
        // state that rivals the one the new owner now holds over `ext_child.out[0]` — and driving
        // it destroys a stranger's allocation, two hops away from anything this wallet did. Same
        // L1, same evidence, same allowlist: broadcast only for a child whose coin this wallet
        // still holds CONFIRMED. A `ctesr-` row with no live coin behind it is a receipt of a coin
        // that has moved on; it is not a coin to defend.
        for (key, json) in child_rows.iter() {
            let Some(cid) = key.strip_prefix("ctesr-") else { continue };
            // L1 — LIVENESS ALLOWLIST (see the root loop). A decided answer, not blindness.
            if !live_sids.contains(cid) {
                continue;
            }
            let cb: mercuryrustlib::tesr::ChildTesrBundle = match serde_json::from_str(json) {
                Ok(b) => b,
                // Fail CLOSED and LOUD, same contract as the `tesr-` arm: a child bundle we cannot
                // parse is a child we cannot defend, not a child with nothing to defend.
                Err(e) => {
                    blind.push(format!("{cid} (child bundle unreadable: {e})"));
                    continue;
                }
            };
            match mercuryrustlib::tesr::watch_child_pass(&self.inner.cc.electrum_client, &cb) {
                mercuryrustlib::tesr::WatchState::Idle => {}
                mercuryrustlib::tesr::WatchState::Acted { ids, failures, blind: unseen } => {
                    if !ids.is_empty() {
                        let _ = self.inner.events_tx.send(WalletEvent::LadderDefended {
                            statechain_id: cid.to_string(),
                            tiers_broadcast: ids.len() as u32,
                        });
                        acted.push(cid.to_string());
                    }
                    blind.extend(unseen); // same reasoning as the parent pass above
                    let hard: Vec<&String> = failures
                        .iter()
                        .filter(|f| !ladder_failure_is_waiting(f))
                        .collect();
                    if !hard.is_empty() {
                        blind.push(format!(
                            "{cid} (child tier broadcast REJECTED for a reason that is not an \
                             immature CSV — the child's exit may be raced or unusable: {})",
                            hard.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; ")
                        ));
                    }
                }
                mercuryrustlib::tesr::WatchState::Blind { reason } => {
                    blind.push(format!("{cid} ({reason})"));
                }
                mercuryrustlib::tesr::WatchState::Void { spender, detail } => {
                    // [RE-ANCHOR] NOT blindness: "I saw, and the coin is gone", not "retry".
                    // Kept out of `blind` so `note_watchtower_ok` can never clear a permanent loss.
                    lost.push(format!("{cid} (spent by {spender} — {detail})"));
                }
            }
        }

        // ---- SPINE TIPS (`spinetip-` rows) ----------------------------------------------------
        //
        // [CATS/V4] The sender's own CHANGE leg. It is defended by NOTHING without this loop: it has
        // no `tesr-` row (so the root loop above never sees it) and no `ctesr-` row (so the child
        // loop never sees it), and what it has to survive is precisely what a child survives — the
        // parent's retained state racing `SP` over `X_m.out[0]`. The difference is only whose money
        // it is. A wallet that has made one CATS payment holds most of its balance here.
        //
        // Same three properties as the loop above, deliberately identical: the L1 liveness allowlist
        // (drive only a coin this wallet still holds CONFIRMED — a tip handed onward belongs to its
        // recipient and racing it would destroy their state), fail-CLOSED on an unparseable row, and
        // `Idle`/`Blind` as different answers.
        for (key, json) in child_rows.iter() {
            let Some(tid) = key.strip_prefix(mercuryrustlib::tesr::SPINE_TIP_KEY_PREFIX) else {
                continue;
            };
            if !live_sids.contains(tid) {
                continue;
            }
            let tip: mercuryrustlib::tesr::SpineTipBundle = match serde_json::from_str(json) {
                Ok(t) => t,
                Err(e) => {
                    blind.push(format!("{tid} (spine-tip bundle unreadable: {e})"));
                    continue;
                }
            };
            match mercuryrustlib::tesr::watch_spine_tip_pass(&self.inner.cc.electrum_client, &tip) {
                mercuryrustlib::tesr::WatchState::Idle => {}
                mercuryrustlib::tesr::WatchState::Acted { ids, failures, blind: unseen } => {
                    if !ids.is_empty() {
                        let _ = self.inner.events_tx.send(WalletEvent::LadderDefended {
                            statechain_id: tid.to_string(),
                            tiers_broadcast: ids.len() as u32,
                        });
                        acted.push(tid.to_string());
                    }
                    blind.extend(unseen);
                    let hard: Vec<&String> =
                        failures.iter().filter(|f| !ladder_failure_is_waiting(f)).collect();
                    if !hard.is_empty() {
                        blind.push(format!(
                            "{tid} (spine-tip tier broadcast REJECTED for a reason that is not an \
                             immature CSV — the tip's exit may be raced or unusable: {})",
                            hard.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; ")
                        ));
                    }
                }
                mercuryrustlib::tesr::WatchState::Blind { reason } => {
                    blind.push(format!("{tid} ({reason})"));
                }
                mercuryrustlib::tesr::WatchState::Void { spender, detail } => {
                    // [RE-ANCHOR] NOT blindness: "I saw, and the coin is gone", not "retry".
                    // Kept out of `blind` so `note_watchtower_ok` can never clear a permanent loss.
                    lost.push(format!("{tid} (spent by {spender} — {detail})"));
                }
            }
        }

        // [RE-ANCHOR] REPORTED BEFORE BLINDNESS, because it is the graver and the more actionable
        // of the two. A blind pass says "look again"; this says "this coin is gone, and no later
        // pass will say otherwise". Surfacing it second, or folded into the blindness message,
        // would bury a permanent loss underneath a transient one.
        if !lost.is_empty() {
            return Err(anyhow!(
                "ladder defence found {} coin(s) whose funding output was spent by a transaction \
                 that is NOT their own trigger. Every tier below the trigger is permanently \
                 unconfirmable, so there is nothing left for this tower to broadcast and no later \
                 pass can recover them — for the expected case, a prior owner's flat backup, the \
                 value went to that prior owner. This is a LOSS, not a blind spot: {}",
                lost.len(),
                lost.join(", ")
            ));
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
            //
            // [CATS/V4] A SPINE TIP is the same case for the same reason — its funding `SP.out[K]`
            // is un-broadcast — so it takes the same route. Without this arm a tip falls through to
            // `withdraw::execute`, which looks for the coin's confirmed funding outpoint, does not
            // find it, and fails with a storage-shaped error about missing rows rather than doing
            // the one thing that works.
            if mercuryrustlib::tesr::load_child(&self.inner.cc, &self.inner.config.wallet_name, &id)
                .await?
                .is_some()
                || mercuryrustlib::tesr::load_spine_tip(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &id,
                )
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
        // Branch is stored root-first; the root's single input spends the on-chain deposit outpoint.
        let root = branch
            .iter()
            .min_by_key(|b| b.tx_n)
            .ok_or_else(|| anyhow!("exit branch is empty — no root tx to anchor the deadline to"))?;
        self.deposit_anchored_deadline_of_root_tx(&root.tx).await
    }

    /// The same computation keyed on the ROOT TRANSACTION alone, so the coloured lane can use it.
    /// A coloured child stores no `branch-` rows — its exit material is the five-tier chain in its
    /// `ctesr-` bundle — but the anchor is identical: the chain's root (`T`) spends the carrier's
    /// on-chain funding output, and every ancestor backup that could race it is anchored at that
    /// output's DEPOSIT height. Splitting this out keeps one definition of the deadline rather than
    /// two that can drift.
    async fn deposit_anchored_deadline_of_root_tx(&self, root_tx_hex: &str) -> Result<u32> {
        use electrum_client::ElectrumApi;
        let initlock = mercuryrustlib::utils::info_config(&self.inner.cc)
            .await
            .map_err(|e| anyhow!("SE config unreachable, so `initlock` is unknown: {e}"))?
            .initlock;
        let root_tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(root_tx_hex).map_err(|e| {
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

    /// **[CTES-R MIGRATION / D2] Settle an un-colourable carrier's ALLOCATION on chain. NOT an exit.**
    ///
    /// This is the action [`Self::unilateral_exit`] used to perform for this class under the wrong
    /// name, reporting `ExitStatus{complete:true}` for it. The action was always sound; the label
    /// was not. It is separated out here so that neither half has to lie about the other:
    ///
    ///  * WHAT IT DOES. Broadcasts the carrier's stored exit BRANCH — the `branch-<id>` rows, which
    ///    for a carrier are the un-broadcast COLOURED split/combine transactions, i.e. the RGB
    ///    witnesses that carved this allocation. Landing them settles the allocation on a confirmed
    ///    outpoint and spends the shared root, which is what defeats an ancestor's stale-backup
    ///    clawback. A carrier whose funding was confirmed to begin with has no branch, and then this
    ///    is a verified no-op — the allocation was already settled.
    ///  * WHAT IT DOES NOT DO, AND CANNOT. It does not exit the coin. The sats stay on the live
    ///    2-of-2 outpoint and still need the SE to move; the only SE-free spend of that outpoint
    ///    this wallet holds is the plain pre-signed backup, which is RGB-UNAWARE and would destroy
    ///    the allocation, so it is never broadcast here. A carrier in this class has no unilateral
    ///    exit at all, and no method in this SDK will claim one for it.
    ///  * WHAT IT REFUSES. Any carrier that is not in the migration class: a carrier that already
    ///    HAS a coloured ladder walks that ladder through `unilateral_exit`, and a carrier that
    ///    could still be coloured must WAIT for its ladder rather than be settled early. Both are
    ///    decided by the one shared definition, [`Self::carrier_is_permanently_flat`], so this
    ///    method and the exit's refusal can never disagree about which coins they are talking about.
    ///
    /// Returns `true` if a branch was broadcast, `false` if the carrier verifiably had none. Either
    /// way the allocation's settlement is VERIFIED against the chain before returning: an
    /// unreachable backend is an `Err`, never a quiet success.
    pub async fn materialise_carrier(&self, statechain_id: &str) -> Result<bool> {
        use electrum_client::ElectrumApi;
        let record = self.record().await?;
        let coin = record
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0)
            .ok_or_else(|| anyhow!("no coin found for statechain id {statechain_id}"))?
            .clone();
        let carriers = self.unspendable_as_btc_outpoints().await?;
        if !is_token_carrier(&coin, &carriers) {
            return Err(anyhow!(
                "coin {statechain_id} carries no RGB allocation — there is nothing to materialise. \
                 A plain coin exits through `unilateral_exit`."
            ));
        }
        // Fail CLOSED through the shared definition: an unreadable ladder row is an `Err` there, so
        // "I could not tell whether this coin could be coloured" never licenses an early settlement.
        if !self.carrier_is_permanently_flat(&coin).await? {
            return Err(anyhow!(
                "carrier {statechain_id} is NOT in the migration class — a COLOURED ladder either \
                 already exists for it or can still be built (coloured root floor {} sat). It must \
                 not be settled early: once it is laddered, `unilateral_exit` walks that ladder and \
                 the walk moves the allocation to your own key, which this method cannot do.",
                self.colored_root_floor()
            ));
        }
        let branch_broadcast = self.broadcast_branch_if_any(statechain_id).await?;
        // VERIFIED, not assumed. The allocation is settled only if the outpoint it is sealed on is
        // really known to the chain; an unreachable backend reads as NOT settled and is an error, so
        // a caller can never be told the tokens are safe on the strength of a failed lookup.
        let funding_txid = coin
            .utxo_txid
            .clone()
            .ok_or_else(|| anyhow!("carrier {statechain_id} has no funding txid recorded"))?;
        let parsed: bitcoin::Txid = funding_txid.parse().map_err(|e| {
            anyhow!("carrier {statechain_id} has an unparseable funding txid {funding_txid}: {e}")
        })?;
        self.inner.cc.electrum_client.transaction_get(&parsed).map_err(|e| {
            anyhow!(
                "carrier {statechain_id}: its exit branch was broadcast ({branch_broadcast}) but the \
                 funding transaction {funding_txid} its allocation is sealed on is not visible to \
                 the chain backend ({e}) — refusing to report the allocation as settled. Retry; the \
                 branch broadcast is idempotent."
            )
        })?;
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let _ = self.inner.events_tx.send(WalletEvent::TokenCarrierMaterialized {
            statechain_id: statechain_id.to_string(),
            deadline_block: tip,
            tip,
        });
        eprintln!(
            "[CTES-R migration] carrier {statechain_id} has no coloured ladder and none can be built \
             for it (coloured root floor {} sat), so it has NO unilateral exit. MATERIALISED \
             instead: exit branch broadcast = {branch_broadcast}, allocation sealed on \
             {funding_txid} and visible to the chain. Its plain backup was deliberately NOT \
             broadcast — that would destroy the tokens — and its sats remain on the 2-of-2 outpoint, \
             movable only with the SE or over the migration hatch (`transfer_tokens`).",
            self.colored_root_floor()
        );
        Ok(branch_broadcast)
    }

    /// Unilateral exit: broadcast the exit branch (immediately valid) and the latest pre-signed
    /// backup tx for each coin. Needs no SE cooperation. A backup whose locktime has not been
    /// reached is reported as `complete=false` with the remaining `wait_blocks` — call again
    /// once the chain advances (the branch stays out either way).
    /// [CTES-R] Tell the RGB engine where a COMPLETED coloured exit put the allocation.
    ///
    /// Deliberately non-fatal: by the time this runs the exit is already on chain and irreversible,
    /// so turning a bookkeeping failure into an `Err` would report a SUCCESSFUL exit as a failed one
    /// and send callers into a retry loop over a finished walk. It is not silent either — both
    /// outcomes are evented, and the failure event says plainly that the engine's views are stale.
    async fn register_exit_tip_best_effort(&self, id: &str) {
        match self.register_colored_exit_tip(id).await {
            Ok(Some(outpoint)) => {
                let _ = self.inner.events_tx.send(WalletEvent::ColoredExitTipRegistered {
                    statechain_id: id.to_string(),
                    outpoint,
                });
            }
            Ok(None) => {} // plain coin, non-token wallet, or already registered
            Err(e) => {
                let _ = self.inner.events_tx.send(WalletEvent::ColoredExitTipUnregistered {
                    statechain_id: id.to_string(),
                    detail: e.to_string(),
                });
            }
        }
    }

    /// **[D40.1] SEVER THIS COIN FROM `F` — the one B1 remedy the adversary cannot decline.**
    ///
    /// # What B1 is
    ///
    /// The statechain's irreducible trust unit. At every transfer the operator's enclave generates a
    /// fresh share and the old pair `(o_i, e_i)` is supposed to be destroyed. If it is retained, the
    /// operator holds a full key-path spend of `F` for every coin with at least one prior owner —
    /// and can take it **offline**, with no SE session, so no census, no attestation, no rate limit
    /// and no audit log sees anything. There is no value bound on this: not per coin, not per victim,
    /// not in aggregate. It is custodial-equivalent against a retaining operator.
    ///
    /// # Why this, and not a re-anchor
    ///
    /// Re-anchoring also clears the prior owners — and it needs one fresh SE co-signature. **The
    /// party being defended against is the party asked to sign**, and refusing to co-sign is already
    /// blessed as non-theft. A defence its adversary can decline is not a defence.
    ///
    /// Broadcasting the trigger needs nobody. `T` is already co-signed, carries `lock_time 0` and no
    /// relative timelock, spends `F` directly, and is 125 vB. It therefore beats every retained rung
    /// **by being valid first** rather than by winning a race. The moment it confirms, every
    /// historical `(o_i, e_i)` for this coin is dead: they authorise a spend of an output that no
    /// longer exists.
    ///
    /// That is a DURATION bound, not a value bound — it ends the exposure rather than capping it —
    /// and it is the only one available.
    ///
    /// # What it costs
    ///
    /// The coin's off-chain life. `T` on chain starts the CSV walk, so from here the coin exits on
    /// the ladder's schedule instead of transferring instantly, and undoing it needs the SE. Pay
    /// this when the alternative is losing the coin, not routinely.
    ///
    /// Mechanically this is [`Self::unilateral_exit`] on one coin — the exit pass broadcasts `T`
    /// first when `F` is unspent, which IS the sever — surfaced under the name of what it is for, so
    /// that a holder acting on the B1 disclosure has an action to take rather than a paragraph to
    /// read. It is also what [`Self::deadline_safety_due`] falls back to when the cooperative
    /// re-anchor is refused.
    pub async fn sever_from_f(&self, statechain_id: &str) -> Result<Vec<crate::types::ExitStatus>> {
        self.unilateral_exit(Some(vec![statechain_id.to_string()]), None).await
    }

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
        // [CTES-R] THE ONE CARRIER THAT MAY EXIT. Until CTES-R a carrier had no unilateral exit at
        // all: its only pre-signed spends of `F` were RGB-unaware, so broadcasting one destroyed the
        // allocation. A COLOURED ladder is the exception and the only one — every tier carries a
        // valid RGB state transition, so the walk MOVES the allocation to the owner's own key.
        //
        // Read once, fail CLOSED: an unreadable ladder table is an `Err` (not an empty set), and a
        // `tesr-` row that will not deserialize counts as NOT coloured, so the carrier is refused.
        // "I could not tell whether this ladder is coloured" must never license the walk.
        //
        // Deliberately NOT opened here: `withdraw` and `refresh` (`refresh.rs`) still refuse
        // carriers outright. Neither is fixed by CTES-R — there is still no coloured on-chain
        // re-anchor (CTESR-GATE §7) — so their refusals stand and only this one opens.
        let mut colored_sids = self.colored_ladder_sids().await?;
        // [CTES-R] …and the coloured SPLIT CHILDREN, which have no `tesr-` row at all. A coloured
        // child IS a carrier (its allocation sits on `SP.out[j]`), and its five-tier pre-signed walk
        // `T -> X_m -> SP -> ext_child -> state_child` moves the allocation to the owner's own key
        // exactly as a root ladder's three-tier walk does. Without this the piece a coloured
        // in-ladder split pays out would be the one carrier class that can never exit — refused by
        // the very guard CTES-R opened. Same fail-closed read (an unreadable table is an `Err`).
        colored_sids.extend(self.colored_child_sids().await?);
        let exitable_carrier = |c: &Coin| -> bool {
            c.statechain_id.as_deref().is_some_and(|id| colored_sids.contains(id))
        };
        let ids: Vec<String> = match statechain_ids {
            Some(ids) => {
                for id in &ids {
                    if let Some(c) = record.coins.iter().find(|c| c.statechain_id.as_deref() == Some(id)) {
                        if is_token_carrier(c, &carriers) && !exitable_carrier(c) {
                            // [CTES-R MIGRATION / D2] …and the un-colourable class gets a DIFFERENT
                            // refusal, not a different outcome.
                            //
                            // The blanket refusal below is right for a carrier that is merely
                            // WAITING for its ladder: it will be coloured by a later `claim()` and
                            // then walks. It says the wrong thing to a carrier that can NEVER be
                            // coloured — "move the asset off this coin first" is not advice to such
                            // an owner, it is a description of something they cannot do — so that
                            // class is answered separately, below.
                            //
                            // What it is NOT answered with any more is `ExitStatus{complete:true}`.
                            // The previous round routed this class into the exit loop, where a
                            // "materialise" arm broadcast the stored `branch-` rows and then
                            // reported the exit COMPLETE. The ACTION was right — those rows are the
                            // un-broadcast coloured split/combine transactions, i.e. the RGB
                            // witnesses themselves, so broadcasting them really does settle the
                            // allocation on chain and really does defeat an ancestor's clawback.
                            // The REPORT was a lie, twice over:
                            //
                            //  * `ExitStatus::complete` is documented as "branch (if any) and
                            //    backup both broadcast". The backup is deliberately never broadcast
                            //    here — it is an RGB-unaware spend of `F` and would destroy the
                            //    allocation — so `complete` could not be true by the field's own
                            //    definition, and the flag was in fact computed from "is the funding
                            //    txid known to the chain", which says nothing about an exit;
                            //  * and it is not an exit in any sense the caller means. A unilateral
                            //    exit ends with the value under the owner's sole key. This ends
                            //    with the ASSET sealed on a confirmed outpoint and the SATS still
                            //    parked on the live 2-of-2, movable only with the SE. There is no
                            //    pre-signed SE-free spend that could do better: colouring is what
                            //    makes a tier an RGB witness, and this is the class that cannot be
                            //    coloured. So no unilateral exit exists for it today.
                            //
                            // A false green on an escape hatch is worse than no hatch — it will be
                            // believed. Refuse, and name the two things that DO exist, both of them
                            // callable: `materialise_carrier` (settle the allocation on chain, the
                            // action this arm used to perform under the wrong name) and
                            // `transfer_tokens` (the migration hatch, which still moves the value).
                            if self.carrier_is_permanently_flat(c).await? {
                                let op = coin_outpoint(c).unwrap_or_else(|| "<unknown>".to_string());
                                return Err(anyhow!(
                                    "coin {id} carries an RGB allocation, no COLOURED ladder can be \
                                     built for it (coloured root floor {} sat), and therefore it has \
                                     NO unilateral, SE-free exit at all — this call will not report \
                                     one. Every pre-signed spend of its funding output {op} that \
                                     this wallet holds is RGB-UNAWARE, so broadcasting one would \
                                     destroy the allocation; that is refused, not deferred. The coin \
                                     is NOT stranded, and two named routes remain: (1) \
                                     `materialise_carrier(\"{id}\")` settles the ALLOCATION on chain \
                                     by broadcasting its stored RGB-aware exit branch — that is what \
                                     protects the tokens from an ancestor's stale-backup clawback, \
                                     and it is what this call used to do while mislabelling itself \
                                     an exit; (2) `transfer_tokens` still pays the asset onward over \
                                     the migration hatch, which opens the RGB-aware legacy \
                                     split/combine lane for exactly this class. The sats stay on the \
                                     2-of-2 outpoint either way and need the SE to move; if you need \
                                     them on chain, move the asset to a colourable carrier via (2) \
                                     first and withdraw cooperatively.",
                                    self.colored_root_floor()
                                ));
                            } else {
                                return Err(anyhow!(
                                    "coin {id} carries an RGB allocation and its ladder is not COLOURED (CTES-R); a plain unilateral exit would destroy the tokens — move the asset off this coin first"
                                ));
                            }
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
                // [CTES-R] A carrier is included ONLY if its ladder is coloured; every other
                // carrier stays excluded from the exit-everything default exactly as before.
                .filter(|c| !is_token_carrier(c, &carriers) || exitable_carrier(c))
                .filter_map(|c| c.statechain_id.clone())
                .collect(),
        };
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let mut statuses = Vec::new();
        for id in ids {
            // [CTES-R MIGRATION / D2] The un-colourable carrier never reaches this loop: it is
            // REFUSED, by name and with its routes spelled out, in the id-selection arm above. The
            // "materialise here and report complete" arm that used to sit at this point was removed
            // — see that refusal for why it was a no-op wearing a success flag.
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
                // [D31] The OWNER drives this pass and is the party that funds a bump, so this is
                // the call site that should use it whenever a fee source exists.
                let bump_parts = self.fee_bump_parts()?;
                let progress = match bump_parts.as_ref() {
                    Some((signer, parts)) => mercuryrustlib::tesr::exit_pass_with_bump(
                        &self.inner.cc.electrum_client,
                        &bundle,
                        &parts.with(signer),
                    )?,
                    None => {
                        mercuryrustlib::tesr::exit_pass(&self.inner.cc.electrum_client, &bundle)?
                    }
                };
                let done = progress.complete;
                if done {
                    self.register_exit_tip_best_effort(&id).await;
                }
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
                    // [B.5/D44] Same capability the root lane uses. The child lane had NO
                    // escalation at all — it broadcast raw — so a fee-stuck child tier died at the
                    // rate it was signed at, on the lane where the payee holds the coin.
                    match self.fee_bump_parts()?.as_ref() {
                        Some((signer, parts)) => mercuryrustlib::tesr::exit_child_pass_with_bump(
                            &self.inner.cc.electrum_client,
                            &cb,
                            &parts.with(signer),
                        )?,
                        None => mercuryrustlib::tesr::exit_child_pass(
                            &self.inner.cc.electrum_client,
                            &cb,
                        )?,
                    };
                let done = progress.complete;
                if done {
                    self.register_exit_tip_best_effort(&id).await;
                }
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

            // [CATS/V4] …and a SPINE TIP: the sender's own change leg, funded by the un-broadcast
            // `SP.out[K]` and capped by ONE tier. Same keyless walk, one tier shorter.
            //
            // This arm is the literal reason the tip has a record of its own. Without it a tip falls
            // through to the flat fallback below, which broadcasts the coin's `branch-` rows and its
            // latest absolute-locktime backup — an RGB-unaware spend of a funding output that does
            // not exist on chain. For a plain tip that is an opaque "missing inputs"; for a coloured
            // one it is the allocation.
            if let Some(tip) =
                mercuryrustlib::tesr::load_spine_tip(&self.inner.cc, &self.inner.config.wallet_name, &id)
                    .await?
            {
                let progress =
                    match self.fee_bump_parts()?.as_ref() {
                        Some((signer, parts)) => {
                            mercuryrustlib::tesr::exit_spine_tip_pass_with_bump(
                                &self.inner.cc.electrum_client,
                                &tip,
                                &parts.with(signer),
                            )?
                        }
                        None => mercuryrustlib::tesr::exit_spine_tip_pass(
                            &self.inner.cc.electrum_client,
                            &tip,
                        )?,
                    };
                let done = progress.complete;
                if done {
                    self.register_exit_tip_best_effort(&id).await;
                }
                let wait_blocks = if done {
                    0
                } else {
                    // AUDITED-SWALLOW: same as the two arms above — `?` covers the blind backend,
                    // `None` genuinely means no tier is pending.
                    mercuryrustlib::tesr::next_spine_tip_exit_tier(
                        &self.inner.cc.electrum_client,
                        &tip,
                    )?
                    .unwrap_or(0) as u32
                };
                statuses.push(crate::types::ExitStatus { statechain_id: id, complete: done, wait_blocks });
                continue;
            }

            // [CTES-R] FAIL CLOSED AT THE FLAT FALLBACK. Everything below broadcasts an RGB-UNAWARE
            // spend of `F` — the branch tx and the latest absolute-locktime backup. Those are
            // retained on a coloured coin too (they must be: `tx1` was co-signed at deposit-init and
            // the census counts it forever), and broadcasting one BURNS the allocation.
            //
            // The filters above should make this unreachable for a carrier — but "should" is how
            // recovery paths lose money. A coin whose `tesr-` row is missing or unreadable after a
            // restore-from-mnemonic reaches exactly here, and it is precisely the case where the
            // wallet cannot see that it is holding an asset. So the carrier test is repeated at the
            // point of no return, as a refusal rather than a filter.
            if record
                .coins
                .iter()
                .any(|c| c.statechain_id.as_deref() == Some(&id) && is_token_carrier(c, &carriers))
            {
                return Err(anyhow!(
                    "coin {id} carries an RGB allocation but has no coloured (CTES-R) ladder to \
                     walk; the only remaining exit is an RGB-unaware spend of its funding output, \
                     which would DESTROY the allocation. Refusing. If this coin was restored from a \
                     mnemonic, restore its `tesr-{id}` ladder row as well."
                ));
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
/// (test + INVALIDATION-SPEC (retired 2026-08-15) §6) to be updated in the same change.
pub(crate) fn deposit_anchored_deadline(h_deposit: u32, initlock: u32) -> u32 {
    h_deposit + initlock
}

/// Fold one receive poll's outcome into a result `claim()` can keep working with.
///
/// EXACTLY ONE error is recoverable here — [`mercuryrustlib::transfer_receiver::TransfersCancelledInPoll`]
/// — and it is recoverable only because everything that poll DID receive is already persisted before
/// it is raised, and because `claim()` reports it on `ClaimResult::cancelled_transfers` and
/// `WalletEvent::TransferCancelled`. This is NOT a swallow: the cancellation comes out louder than
/// it went in (an event, plus a field, instead of an opaque `Err` that also destroyed the pass).
///
/// Every other error propagates unchanged. That half is what keeps this safe — turning a
/// coordinator outage or a DB fault into "an empty, successful pass" would be exactly the
/// silent-degradation shape this repo has a CI guard for.
///
/// The call site deliberately uses the NON-raising entry point, so in the ordinary case this fold
/// is a pass-through: recovering from the raised form would lose `received_statechain_ids`, and a
/// poll that both received one payment and found another cancelled must report both. The downcast
/// arm is therefore the boundary contract rather than the hot path — it is what makes "no route into
/// `claim()` can lose a pass to a cancellation" true of a future edit that switches back to
/// `transfer_receiver::execute`, which is precisely the edit that caused this defect the first time.
fn fold_receive_outcome(
    outcome: Result<mercuryrustlib::transfer_receiver::TransferReceiveResult>,
) -> Result<mercuryrustlib::transfer_receiver::TransferReceiveResult> {
    let err = match outcome {
        Ok(result) => return Ok(result),
        Err(err) => err,
    };
    match err.downcast_ref::<mercuryrustlib::transfer_receiver::TransfersCancelledInPoll>() {
        Some(cancelled) => Ok(mercuryrustlib::transfer_receiver::TransferReceiveResult {
            is_there_batch_locked: false,
            received_statechain_ids: Vec::new(),
            cancelled_statechain_ids: cancelled.statechain_ids.clone(),
        }),
        None => Err(err),
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
    // **[D71 / M-4] The balance is a function of DISTINCT STATECHAIN IDS, not of rows.**
    //
    // A coin is identified by its statechain id; a second live row carrying the same id is the same
    // coin counted twice, and summing rows turns that into spendable-looking value. The mailbox
    // survey found the path: a duplicated ciphertext takes the same route as an honest re-serve, and
    // what refuses it today is `validate_tx0_output_pubkey` failing because the completed handover
    // rotated the SE's share — i.e. an unrelated subsystem's side effect, not a rule. If that
    // incidental protection ever lapsed, the failure here would be SILENT: a merchant crediting on
    // `get_balance` would over-credit, with nothing in the log.
    //
    // Deduping here does not make the replay safe — `claim()` refuses it by name now, which is the
    // actual fix. It removes the SILENT half, so a defect upstream shows up as a coin that fails to
    // arrive rather than as money that is not there.
    let mut counted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for c in &record.coins {
        if c.duplicate_index != 0 {
            continue;
        }
        if coin_outpoint(c).map_or(false, |o| carriers.contains(&o)) {
            continue;
        }
        // Only ids are deduped. A row with no id yet (INITIALISED, no tx0) contributes to no bucket
        // below anyway, and must not collapse other such rows into one.
        if let Some(sid) = c.statechain_id.as_deref() {
            if !counted.insert(sid) {
                continue;
            }
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

// =================================================================================================
// [K > 1 PREREQUISITE 4] THE DERIVED-SLOT BUDGET.
//
// A K-recipient batch needs K + 1 statechain slots, and every slot costs one derived token vouched
// by the coin being split. The SE caps those at `max_derived_tokens_per_statechain` (64) counted
// over the parent's LIFETIME, spent rows included — so the batch size is bounded, and a batch that
// is minted and then abandoned charges the allowance permanently. At K = 1 that is 2 slots an
// attempt and 31 attempts survive; at K = 20 it is 21 slots and 2 do.
// =================================================================================================
#[cfg(test)]
mod derived_slot_budget_tests {
    use super::*;

    #[test]
    fn the_bound_is_k_plus_one_slots_and_the_refusal_names_k() {
        // K + 1 == cap is the largest admissible batch: 63 payees plus the sender's change.
        refuse_oversized_slot_batch(MAX_BATCH_RECIPIENTS + 1).expect("K = 63 fits exactly");
        assert_eq!(MAX_BATCH_RECIPIENTS + 1, DERIVED_SLOTS_PER_STATECHAIN as usize);

        let e = refuse_oversized_slot_batch(MAX_BATCH_RECIPIENTS + 2)
            .expect_err("K = 64 needs 65 slots and must be refused");
        let msg = e.to_string();
        // A caller that batches automatically has to be able to act on this, so every number it
        // needs is IN the message: how many it asked for, how many slots that is, the cap, and the
        // largest K it may retry with.
        assert!(msg.contains("64 recipients"), "got: {msg}");
        assert!(msg.contains("65 statechain slots"), "got: {msg}");
        assert!(msg.contains("64 derived slots"), "got: {msg}");
        assert!(msg.contains("at most 63 recipients"), "got: {msg}");

        // …and it is the TYPED error, so a caller can match on it rather than parse prose.
        let typed = e.downcast_ref::<SdkError>().expect("a named refusal, not a bare anyhow");
        assert!(matches!(
            typed,
            SdkError::BatchTooManyRecipients { recipients: 64, slots: 65, cap: 64, max_recipients: 63 }
        ));
    }

    #[test]
    fn the_single_recipient_and_empty_cases_are_untouched() {
        // The guard bounds a batch; it must not become a new way for an ordinary payment to fail.
        refuse_oversized_slot_batch(2).expect("K = 1: piece + change");
        refuse_oversized_slot_batch(1).expect("a split with no change leg");
    }

    #[test]
    fn a_pooled_voucher_survives_a_transient_failure_and_dies_after_three() {
        // The two failure modes look identical from the client: a transient error (retry — that is
        // the whole point of the pool) and a token the SE has already consumed (retrying it forever
        // wedges every later batch behind a dead id). Counting attempts bounds the second without
        // parsing an error string to tell them apart.
        let mut pool = vec![
            SlotVoucher { token_id: "a".into(), failures: 0 },
            SlotVoucher { token_id: "b".into(), failures: 0 },
        ];
        for attempt in 1..SLOT_VOUCHER_FAILURE_LIMIT {
            assert!(record_voucher_failure(&mut pool, "a"));
            assert_eq!(pool.len(), 2, "still retryable after {attempt} failure(s)");
        }
        assert!(record_voucher_failure(&mut pool, "a"));
        assert_eq!(
            pool.iter().map(|v| v.token_id.as_str()).collect::<Vec<_>>(),
            vec!["b"],
            "a voucher that has failed {SLOT_VOUCHER_FAILURE_LIMIT} times is discarded, and only it"
        );

        // A token that is not in the pool is not an error and does not rewrite the pool — the
        // fallback/onboarding path hands out ids that were never pooled.
        assert!(!record_voucher_failure(&mut pool, "never-pooled"));
    }

    #[test]
    fn the_local_bound_mirrors_the_servers_default() {
        // Mirrored, not fetched: the guard must fire before the first SE call. Stated as a test so
        // a change to `max_derived_tokens_per_statechain` in server_config.rs is a change somebody
        // has to make here too, rather than a silently optimistic client.
        assert_eq!(
            DERIVED_SLOTS_PER_STATECHAIN, 64,
            "server/src/server_config.rs: max_derived_tokens_per_statechain"
        );
    }
}

#[cfg(test)]
mod claim_cancellation_tests {
    use super::*;

    fn cancelled(ids: &[&str]) -> anyhow::Error {
        anyhow::Error::new(mercuryrustlib::transfer_receiver::TransfersCancelledInPoll {
            statechain_ids: ids.iter().map(|s| s.to_string()).collect(),
        })
    }

    fn poll(received: &[&str], batch_locked: bool) -> mercuryrustlib::transfer_receiver::TransferReceiveResult {
        mercuryrustlib::transfer_receiver::TransferReceiveResult {
            is_there_batch_locked: batch_locked,
            received_statechain_ids: received.iter().map(|s| s.to_string()).collect(),
            cancelled_statechain_ids: Vec::new(),
        }
    }

    /// THE DEFECT, stated as a test. A cancelled incoming transfer used to `?` out of `claim()`
    /// before `ClaimResult` was built, so the deposits and transfers that DID arrive in the same
    /// poll lost their events and their report entirely — one withdrawn payment silently discarded
    /// the whole pass. The cancellation must be REPORTED, not thrown.
    #[test]
    fn a_cancellation_does_not_discard_the_rest_of_the_pass() {
        let folded = fold_receive_outcome(Err(cancelled(&["sid-cancelled"])))
            .expect("a cancellation must not abort the claim pass");
        assert_eq!(folded.cancelled_statechain_ids, vec!["sid-cancelled".to_string()]);
        assert!(folded.received_statechain_ids.is_empty());
    }

    /// ...and it is still not silent: the ids come back on the result so `claim()` can put them on
    /// `ClaimResult` and emit `TransferCancelled`. An empty list here would be the exact
    /// silent-degradation shape — a withdrawn payment reading as an idle mailbox.
    #[test]
    fn the_cancelled_ids_survive_the_fold() {
        let folded = fold_receive_outcome(Err(cancelled(&["a", "b"]))).unwrap();
        assert_eq!(folded.cancelled_statechain_ids, vec!["a".to_string(), "b".to_string()]);
    }

    /// Any OTHER error still aborts. This is the half that makes the fix safe: only the one typed,
    /// fully-persisted cancellation signal is recoverable; a DB fault or a coordinator outage must
    /// not be turned into "an empty successful pass".
    #[test]
    fn any_other_error_still_aborts_the_pass() {
        let err = match fold_receive_outcome(Err(anyhow!("the coordinator is unreachable"))) {
            Err(err) => err,
            Ok(_) => panic!("a genuine failure must still propagate, not fold to a clean pass"),
        };
        assert!(err.to_string().contains("coordinator is unreachable"));
    }

    /// The ordinary path is untouched.
    #[test]
    fn a_clean_poll_folds_to_itself() {
        let folded = fold_receive_outcome(Ok(poll(&["got-paid"], true))).unwrap();
        assert_eq!(folded.received_statechain_ids, vec!["got-paid".to_string()]);
        assert!(folded.cancelled_statechain_ids.is_empty());
        assert!(folded.is_there_batch_locked);
    }

    /// A cancellation is counted apart from a receipt in the pass's report, so no consumer can read
    /// one as the other. `claimed_transfers` must never absorb a cancelled id.
    #[test]
    fn claim_result_counts_cancellations_apart_from_receipts() {
        let r = ClaimResult {
            claimed_transfers: 1,
            confirmed_deposits: Vec::new(),
            token_results: Vec::new(),
            cancelled_transfers: vec!["sid-cancelled".to_string()],
        };
        assert_eq!(r.claimed_transfers, 1);
        assert_eq!(r.cancelled_transfers, vec!["sid-cancelled".to_string()]);
    }

    /// The SDK's re-exported refusal types are the SAME types the client layer raises, so an app
    /// can `downcast_ref` across the crate boundary. If these ever became SDK-local newtypes the
    /// downcast would silently start returning `None` and every caller would fall back to string
    /// matching — which is exactly what the typed refusal exists to eliminate.
    ///
    /// Characterization test: it pins a property that is already true, so it has no red phase.
    #[test]
    fn the_sdk_reexports_the_same_refusal_types_the_client_raises() {
        let raised: anyhow::Error =
            anyhow::Error::new(mercuryrustlib::transfer_sender::CancelRefused {
                statechain_id: "sid-1".to_string(),
                code: "already_claimed".to_string(),
                message: "transfer already claimed by the recipient; it cannot be cancelled"
                    .to_string(),
                decision: Some(mercurylib::transfer::cancel::CancelDecision::AlreadyClaimed),
            });
        // Downcast through the SDK's OWN path name, not the client's.
        let via_sdk = raised.downcast_ref::<crate::CancelRefused>();
        assert!(via_sdk.is_some(), "the SDK re-export must be the same type the client raises");
        assert_eq!(via_sdk.unwrap().code, "already_claimed");
        // ...and the two successes are re-exported as one enum, not duplicated.
        assert_eq!(crate::CancelOutcome::Cancelled, CancelOutcome::Cancelled);
    }

    /// The event exists and names the transfers, so an app driven by events alone (the background
    /// watcher's only channel) learns about a withdrawn payment.
    #[test]
    fn there_is_an_event_for_a_cancelled_incoming_transfer() {
        let ev = WalletEvent::TransferCancelled {
            statechain_ids: vec!["sid-cancelled".to_string()],
        };
        match ev {
            WalletEvent::TransferCancelled { statechain_ids } => {
                assert_eq!(statechain_ids, vec!["sid-cancelled".to_string()]);
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
}

#[cfg(test)]
mod recovery_bundle_version_tests {
    use super::*;

    #[test]
    fn rejects_unknown_version() {
        // Well-formed JSON whose only fault is an unknown version; the probe reads `version` alone,
        // so the other fields need not be valid wallet material.
        let json = r#"{"version":999,"wallet_name":"w","wallet":{},"backups":[],"rgb_mnemonic":null,"notes":""}"#;
        // Avoid `.unwrap_err()` here: it would require `RecoveryBundle: Debug`, and the frozen
        // struct intentionally has no Debug derive.
        let err = match parse_recovery_bundle(json) {
            Ok(_) => panic!("expected an error for an unknown recovery-bundle version"),
            Err(e) => e,
        };
        match err.downcast_ref::<SdkError>() {
            Some(SdkError::UnsupportedVersion { kind, found, supported }) => {
                assert_eq!(*kind, "recovery bundle");
                assert_eq!(*found, 999);
                assert_eq!(*supported, 1);
            }
            _ => panic!("expected SdkError::UnsupportedVersion, got {err:?}"),
        }
    }

    #[test]
    fn rejects_absent_version() {
        // No version field at all: the mandatory probe field makes this a refusal, not a silent parse.
        let json = r#"{"wallet_name":"w","wallet":{},"backups":[],"rgb_mnemonic":null,"notes":""}"#;
        assert!(parse_recovery_bundle(json).is_err());
    }
}

/// [D13] The two decisions the plain-leaf near-deadline port rests on, in isolation from the
/// integration loop (which needs a live coordinator, electrum and DB). These pin the SAFETY
/// property — a mid-split leaf is never driven, an unreadable journal blinds everything — and the
/// HONESTY property — a plain leaf reports as a leaf exit, not a token settlement.
#[cfg(test)]
/// [RGB STAGE 0] The claim-time laddering hole: a carrier the allocation set does not know about yet.
#[cfg(test)]
mod rgb_stage0_claim_laddering_tests {
    /// The source of the ladder loop, comments stripped, so the ORDER and the SCOPE of the guard can
    /// be asserted. Both matter and neither is visible to a behavioural test without a wallet, an
    /// RGB engine and a live coordinator.
    fn ladder_loop() -> String {
        let src = include_str!("wallet.rs");
        let at = src.find("[RGB STAGE 0]").expect("the guard exists");
        let end = src[at..].find("// ROOT-ONLY [B0]").expect("the loop continues to the F check");
        src[at..at + end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The guard reads the coin's OWN backup rows, which are authoritative at claim time — not the
    /// allocation set, which is populated by `book_incoming_token` AFTER this loop runs. Reading the
    /// set here is precisely the bug: a freshly received carrier is not in it.
    #[test]
    fn the_guard_reads_backup_rows_not_the_allocation_set() {
        let body = ladder_loop();
        assert!(body.contains("read_backup_rows("), "must consult the coin's own backup rows");
        assert!(
            body.contains("rgb_consignment.is_some()"),
            "a consignment on any row is what marks the coin a carrier at this instant"
        );
    }

    /// **THE REGRESSION THIS ALMOST INTRODUCED.** An UNCONDITIONAL skip would have been correct for
    /// the hole and wrong for everything else: a recognised coloured carrier also has consignment
    /// rows, so it would have been denied the CTES-R coloured ladder — the one lane whose entire
    /// purpose is to ladder carriers without destroying the allocation — forever, on every pass.
    /// The guard must therefore be scoped to coins the set does NOT already know are carriers.
    #[test]
    fn the_guard_does_not_intercept_a_recognised_carrier() {
        let body = ladder_loop();
        assert!(
            body.contains("if !is_token_carrier(coin, &carriers)"),
            "the guard must fire ONLY for a coin the allocation set has not booked; an \
             unconditional skip would permanently deny recognised carriers the coloured ladder"
        );
        let guard = body.find("if !is_token_carrier(coin, &carriers)").unwrap();
        let read = body.find("read_backup_rows(").unwrap();
        assert!(guard < read, "the scope test must gate the read, not follow it");
    }

    /// Absence and failure are different facts, and `get_backup_txs` is `fetch_one` — a missing row
    /// arrives as an `Err`. A coin with no rows is an ordinary coin and must proceed; a database that
    /// will not answer must NOT be read as "no consignment", because that reading is exactly how an
    /// RGB carrier gets laddered.
    #[test]
    fn an_unreadable_row_fails_closed_and_an_absent_one_does_not() {
        let body = ladder_loop();
        assert!(
            body.contains("Err(_) =>") && body.contains("RgbStateUnavailable"),
            "an unreadable row must skip the coin, under a reason distinct from a real carrier"
        );
        assert!(
            body.contains("Ok(_) => {}"),
            "a coin with NO rows is an ordinary coin and must fall through, not be skipped"
        );
    }
}

/// A TES-R tier's virtual size, used to SIZE a float before any particular tier is in hand.
///
/// 141 vB is the measured figure from the WP1 spike. It is an estimate by necessity — the float has
/// to be sized before a spike, not during one — and it is used only for budgeting; the actual child
/// is priced against the actual tier.
const TYPICAL_TIER_VSIZE: u64 = 141;

/// The non-borrowing half of a [`mercuryrustlib::tesr::BumpCapability`], so a caller can hold the
/// signer on its own stack and assemble the capability at the point of use.
struct FeeBumpParts {
    core_rpc: mercuryrustlib::core_rpc::CoreRpcConfig,
    funding: mercurylib::wallet::p2a_fee_child::FundingInput,
    change_script_pubkey: bitcoin::ScriptBuf,
    target_fee_rate: f64,
}

impl FeeBumpParts {
    fn with<'a>(
        &'a self,
        signer: &'a dyn mercuryrustlib::tesr::FeeBumpSigner,
    ) -> mercuryrustlib::tesr::BumpCapability<'a> {
        mercuryrustlib::tesr::BumpCapability {
            core_rpc: self.core_rpc.clone(),
            funding: self.funding.clone(),
            change_script_pubkey: self.change_script_pubkey.clone(),
            target_fee_rate: self.target_fee_rate,
            signer,
        }
    }
}

#[cfg(test)]
mod d13_leaf_gate_tests {
    use super::{wallet_holds_address, leaf_split_gate, near_deadline_exit_event, LeafSplitGate, WalletEvent};
    use std::collections::HashSet;

    fn set(ids: &[&str]) -> Option<HashSet<String>> {
        Some(ids.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_coin_named_by_an_open_split_is_never_driven() {
        // THE BLOCKER the safety probe found: this coin reads CONFIRMED with a stale row, and
        // driving it would broadcast the state its own split superseded, destroying pieces already
        // conveyed to payees. It must be held, with a message (not idle), never driven.
        let gate = leaf_split_gate(&set(&["dead", "beef"]), "beef", "adopted split child");
        match gate {
            LeafSplitGate::Hold(msg) => {
                assert!(msg.contains("mid-split"), "the reason must name why it is held: {msg}");
                assert!(msg.contains("beef"));
            }
            other => panic!("a mid-split coin must be Hold, got {other:?}"),
        }
    }

    #[test]
    fn a_coin_absent_from_every_open_split_is_drivable() {
        // The whole point of the port: a plain leaf NOT mid-split is now evaluated for exit, where
        // before it was dropped by the coloured-only gate.
        assert_eq!(
            leaf_split_gate(&set(&["dead", "beef"]), "cafe", "adopted split child"),
            LeafSplitGate::Drive
        );
        // And with no open splits at all.
        assert_eq!(
            leaf_split_gate(&set(&[]), "cafe", "spine tip"),
            LeafSplitGate::Drive
        );
    }

    #[test]
    fn an_unreadable_journal_holds_every_coin() {
        // Fail closed: if we cannot read the journal we cannot prove ANY leaf is safe, so none is
        // driven. HoldSilently because the blanket blindness is recorded once before the loop.
        assert_eq!(
            leaf_split_gate(&None, "cafe", "adopted split child"),
            LeafSplitGate::HoldSilently
        );
        assert_eq!(
            leaf_split_gate(&None, "beef", "spine tip"),
            LeafSplitGate::HoldSilently
        );
    }

    #[test]
    fn a_leaf_whose_exit_pays_someone_else_is_not_driven() {
        // THE CANCEL-RESURRECTION WINDOW. A failed child conveyance leaves a PAYEE-paying row;
        // /transfer/cancel lifts the coin back to CONFIRMED; reclaim_cancelled_conveyance returns
        // Ok(false) for a child so the row is never re-pointed. Driving it would force-exit the
        // sender's own money into the payee's address.
        let mine = ["bcrt1qmine", "bcrt1qmine_backup"];
        assert!(!wallet_holds_address("bcrt1qPAYEE", mine.iter().copied()),
            "a row paying an address we do not hold must NOT be driven");
        assert!(wallet_holds_address("bcrt1qmine_backup", mine.iter().copied()),
            "the owner's own backup address is exactly what an honest row pays");
        assert!(wallet_holds_address("bcrt1qmine", mine.iter().copied()));
    }

    #[test]
    fn a_leaf_reports_when_it_declines_rather_than_going_quiet() {
        // Declining to drive is itself a loss if the address IS ours and merely unknown to this
        // record, so the caller must surface it. This pins the predicate's shape; the reporting is
        // asserted by the loop's `blind` push at the call site.
        let none: [&str; 0] = [];
        assert!(!wallet_holds_address("anything", none.iter().copied()),
            "an empty record recognises no address — and must therefore report, not silently skip");
    }

    #[test]
    fn a_plain_leaf_reports_a_leaf_exit_a_coloured_one_a_token_settlement() {
        match near_deadline_exit_event(false, "abc".into(), 100, 90) {
            WalletEvent::LeafExitForced { statechain_id, deadline_block, tip } => {
                assert_eq!(statechain_id, "abc");
                assert_eq!(deadline_block, 100);
                assert_eq!(tip, 90);
            }
            other => panic!("a PLAIN leaf must emit LeafExitForced, not {other:?}"),
        }
        match near_deadline_exit_event(true, "xyz".into(), 200, 150) {
            WalletEvent::TokenCarrierMaterialized { statechain_id, .. } => {
                assert_eq!(statechain_id, "xyz");
            }
            other => panic!("a COLOURED leaf must emit TokenCarrierMaterialized, not {other:?}"),
        }
    }
}

/// **[D66] THE BEHAVIOURAL PROOF that [D58]'s fix holds — the one a source scan cannot give.**
///
/// [D64] established the ceiling by defeating seven guards. The defeat that matters here was against
/// `deny_optional_deadline_safety`: a source scan sees presence and brace depth, so
/// `let _ = cfg.background_auto_refresh && wallet.deadline_safety_due(..).await.is_ok();` passes it
/// while restoring the exact defect D58 fixed — a wallet that opts into background maintenance loses
/// the sever.
///
/// This test does not read source. It CALLS `maintenance_plan` over the entire config space that
/// could plausibly gate a maintenance pass, and asserts the deadline pass is in every plan. The
/// mutation above cannot be expressed against it: to make the pass conditional you must return a
/// plan without it, and then this goes red.
#[cfg(test)]
mod maintenance_plan_tests {
    use super::{maintenance_plan, MaintenancePass};
    use crate::config::SdkConfig;

    /// Every combination of the flags that have EVER gated a maintenance pass, plus the two
    /// shipped constructors themselves.
    #[test]
    fn every_config_still_schedules_the_deadline_pass() {
        let mut checked = 0usize;
        for base in [SdkConfig::regtest("d66"), SdkConfig::mainnet("d66", "http://se.invalid", "tcp://electrum.invalid:50001")] {
            for auto_refresh in [false, true] {
                for background_auto_refresh in [false, true] {
                    for auto_exit in [false, true] {
                        for colored_ladder in [false, true] {
                            let mut cfg = base.clone();
                            cfg.auto_refresh = auto_refresh;
                            cfg.background_auto_refresh = background_auto_refresh;
                            cfg.auto_exit = auto_exit;
                            cfg.colored_ladder = colored_ladder;
                            let plan = maintenance_plan(&cfg);
                            assert!(
                                plan.contains(&MaintenancePass::DeadlineSafety),
                                "[D58/D66] a background tick would NOT run the deadline pass with \
                                 auto_refresh={auto_refresh}, \
                                 background_auto_refresh={background_auto_refresh}, \
                                 auto_exit={auto_exit}, colored_ladder={colored_ladder}.\n\n\
                                 That is the defect D58 fixed: `deadline_safety_due` is a strict \
                                 SUPERSET of `auto_refresh_due` (it calls it as route 1 and then \
                                 severs from `F` for whatever the counterparty declined to sign), so \
                                 gating it on an ECONOMICS flag means opting into more maintenance \
                                 buys LESS protection. The whole-coin clock `min(L_k)` is held by \
                                 the coin's PRIOR OWNERS; when it passes, an ancestor's matured rung \
                                 spends `F`.\n\n\
                                 If a genuinely conditional pass is wanted, add a NEW variant and \
                                 gate that one — do not put a condition on this. plan: {plan:?}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 32, "the config space must actually be swept, not short-circuited");
    }

    /// NON-VACUITY. A plan that is always the full set would satisfy the rule above no matter what
    /// the loop does with it, so pin that the ENUM still has a shape a caller must match on — the
    /// mechanism by which a future conditional pass becomes visible rather than silent.
    #[test]
    fn the_plan_is_a_set_a_caller_must_match_on() {
        let plan = maintenance_plan(&SdkConfig::regtest("d66"));
        assert_eq!(
            plan.len(),
            1,
            "the plan gained or lost a pass. That is not automatically wrong — but the background \
             loop matches exhaustively on `MaintenancePass`, so a new variant is a COMPILE error \
             there until it is handled, which is the property this assertion protects. Update this \
             count deliberately, in the same commit as the loop arm. plan: {plan:?}"
        );
        assert_eq!(plan[0], MaintenancePass::DeadlineSafety);
    }
}
