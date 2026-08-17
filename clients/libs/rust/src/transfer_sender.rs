use std::{cmp::Ordering, str::FromStr};

use crate::{client_config::ClientConfig, deposit::create_tx1, sqlite_manager::{get_backup_txs, get_wallet, update_backup_txs, update_wallet}, transaction::new_transaction, transfer_receiver::PendingTransferInfo, utils::info_config};
use anyhow::{anyhow, Result};
use chrono::Utc;
use mercurylib::{decode_transfer_address, transfer::sender::{create_transfer_signature, create_transfer_update_msg_with_branch, TransferSenderRequestPayload, TransferSenderResponsePayload}, utils::get_blockheight, wallet::{get_previous_outpoint, Activity, BackupTx, Coin, CoinStatus, Wallet}};
use electrum_client::ElectrumApi;

pub async fn create_backup_transactions(
    client_config: &ClientConfig, 
    recipient_address: &str,
    wallet: &mut Wallet,
    statechain_id: &str,
    duplicated_indexes: Option<Vec<u32>>,
) -> Result<Vec<BackupTx>> {

    // throw error if duplicated_indexes contains an index that does not exist in wallet.coins
    // this can be moved to the caller function
    if duplicated_indexes.is_some() {
        for index in duplicated_indexes.as_ref().unwrap() {
            if *index as usize >= wallet.coins.len() {
                return Err(anyhow!("Index {} does not exist in wallet.coins", index));
            }
        }
    }  

    let mut coin_list: Vec<&mut Coin> = Vec::new();

    // A coin with NO flat backup rows is an expected shape, not a database failure: a split child's
    // funding output was never deposited and a spine tip's is un-broadcast, so neither ever acquired
    // one. Routing such a coin to the FLAT sender is a real condition and it gets a named refusal
    // here — before this, the bare `?` handed the caller sqlx's own sentence, which `chaos22`'s
    // oracle could only class as an unclassified breach.
    //
    // [#145] The message below used to say the caller's own dispatch had already routed away every
    // shape that legitimately lacks rows. That was TRUE OF ONE CALLER. `execute` is public and
    // `chaos22`'s `respend` calls it directly, so tips arrived here anyway and the sentence was
    // simply false — it named a guard the caller did not run. The tip refusal now lives in
    // `execute_ex` itself, which is what makes the residue below genuinely residual.
    let backup_transactions =
        crate::sqlite_manager::try_get_backup_txs(&client_config.pool, &wallet.name, &statechain_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "statechain id {statechain_id} has NO EXIT MATERIAL: no flat backup rows, and \
                     every shape that legitimately lacks them has already been routed away — a \
                     `spinetip-` row is refused by name in `execute_ex` above, and a `ctesr-` child \
                     goes to `child_retransfer`. So this is a slot the SE knows about that this \
                     wallet cannot exit from and cannot convey on any lane — most often a derived \
                     child slot whose split failed after the slot was created. It must not have been \
                     offered to coin selection. Restore it from a recovery bundle if its material \
                     exists elsewhere; otherwise it is spendable only cooperatively."
                )
            })?;

    // Get coins that already have a backup transaction
    for coin in wallet.coins.iter_mut() {
        // Check if coin matches any backup transaction and has one of the specified statuses
        let has_matching_tx = backup_transactions.iter().any(|backup_tx| {
            if let Ok(tx_outpoint) = get_previous_outpoint(backup_tx) {
                if let (Some(utxo_txid), Some(utxo_vout)) = (coin.utxo_txid.clone(), coin.utxo_vout) {
                    (coin.status == CoinStatus::DUPLICATED ||
                     coin.status == CoinStatus::CONFIRMED ||
                     coin.status == CoinStatus::IN_TRANSFER) &&
                    tx_outpoint.txid == utxo_txid &&
                    tx_outpoint.vout == utxo_vout
                } else {
                    false
                }
            } else {
                false
            }
        });

        let mut coin_to_add = false;

        if duplicated_indexes.is_some() {
            if coin.statechain_id == Some(statechain_id.to_string()) && 
            (coin.status == CoinStatus::CONFIRMED || coin.status == CoinStatus::IN_TRANSFER) {
                coin_to_add = true;
            }

            if coin.statechain_id == Some(statechain_id.to_string()) && coin.status == CoinStatus::DUPLICATED && 
                duplicated_indexes.is_some() && duplicated_indexes.as_ref().unwrap().contains(&coin.duplicate_index) {
                coin_to_add = true;
            }
        }

        if has_matching_tx || coin_to_add {
            if coin.locktime.is_none() {
                return Err(anyhow::anyhow!("coin.locktime is None"));
            }
        
            let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
            let current_blockheight = block_header.height as u32;
        
            if current_blockheight > coin.locktime.unwrap()  {
                return Err(anyhow::anyhow!("The coin is expired. Coin locktime is {} and current blockheight is {}", coin.locktime.unwrap(), current_blockheight));
            }

            coin_list.push(coin);
        }
    }

    // The backup transaction for the CONFIRMED coin is created when it is detected in the mempool
    // So it is exepcted that the coin with duplicate_index == 0 is in the list since it must have at least one backup transaction
    let coins_with_zero_index = coin_list
        .iter()
        .filter(|coin| coin.duplicate_index == 0 && (coin.status == CoinStatus::CONFIRMED || coin.status == CoinStatus::IN_TRANSFER))
        .collect::<Vec<_>>();

    if coins_with_zero_index.len() != 1 {
        return Err(anyhow!("There must be at least one coin with duplicate_index == 0"));
    }

    for coin in coin_list.iter_mut() {
        if coin.status == CoinStatus::DUPLICATED {
            let address = bitcoin::Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(client_config.network)?;
            let utxo_list =  client_config.electrum_client.script_list_unspent(&address.script_pubkey())?;

            for unspent in utxo_list {
                if coin.utxo_txid == Some(unspent.tx_hash.to_string()) && coin.utxo_vout == Some(unspent.tx_pos as u32) {
                    let mut is_confirmed =  false;

                    if unspent.height > 0 {
                        let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
                        let blockheight = block_header.height;

                        let confirmations = blockheight - unspent.height + 1;

                        if confirmations as u32 >= client_config.confirmation_target {
                            is_confirmed = true;
                        }
                    }

                    if !is_confirmed {
                        return Err(anyhow!("The coin with duplicated index {} has not yet been confirmed. This transfer cannot be performed.", coin.duplicate_index));
                    }

                    break;
                }
            }
        }
    }

    // Move the coin with CONFIRMED status to the first position
    coin_list.sort_by(|a, b| {
        match (&a.status, &b.status) {
            (CoinStatus::CONFIRMED, _) => Ordering::Less,
            (_, CoinStatus::CONFIRMED) => Ordering::Greater,
            _ => Ordering::Equal,
        }
    });

    let mut new_backup_transactions = Vec::new();

    // create backup transaction for every coin. Same absence-vs-failure rule as the caller above:
    // `new_tx_n` is derived from this length, so reading a failed read as "zero rows" would restart
    // the ladder at tx_n 0 and collide with every existing hop.
    let backup_transactions =
        crate::sqlite_manager::try_get_backup_txs(&client_config.pool, &wallet.name, &statechain_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "statechain id {statechain_id} has no flat backup rows to extend — refusing to \
                     start a new backup chain at tx_n 0 over a coin that should already have one"
                )
            })?;

    let mut new_tx_n = backup_transactions.len() as u32;

    for coin in coin_list {

        let mut filtered_transactions: Vec<BackupTx> = Vec::new();

        for backup_tx in &backup_transactions {
            if let Ok(tx_outpoint) = get_previous_outpoint(&backup_tx) {
                if let (Some(utxo_txid), Some(utxo_vout)) = (coin.utxo_txid.clone(), coin.utxo_vout) {
                    if tx_outpoint.txid == utxo_txid && tx_outpoint.vout == utxo_vout {
                        filtered_transactions.push(backup_tx.clone());
                    }
                }
            }
        }

        filtered_transactions.sort_by(|a, b| a.tx_n.cmp(&b.tx_n));

        if filtered_transactions.len() == 0 {
            new_tx_n = new_tx_n + 1;
            let bkp_tx1 = create_tx1(client_config, coin, &wallet.network, new_tx_n).await?;
            filtered_transactions.push(bkp_tx1);
        }

        let qt_backup_tx = filtered_transactions.len() as u32;

        new_tx_n = new_tx_n + 1;

        let bkp_tx1 = &filtered_transactions[0];

        let signed_tx = create_backup_tx_to_receiver(client_config, coin, bkp_tx1, recipient_address, qt_backup_tx, &wallet.network).await?;

        let backup_tx = BackupTx {
            tx_n: new_tx_n,
            tx: signed_tx.clone(),
            client_public_nonce: coin.public_nonce.as_ref().unwrap().to_string(),
            server_public_nonce: coin.server_public_nonce.as_ref().unwrap().to_string(),
            client_public_key: coin.user_pubkey.clone(),
            server_public_key: coin.server_pubkey.as_ref().unwrap().to_string(),
            blinding_factor: coin.blinding_factor.as_ref().unwrap().to_string(),
            rgb_consignment: None,
            rgb_blinding: None,
        };

        filtered_transactions.push(backup_tx);

        if coin.duplicate_index == 0 {
            new_backup_transactions.splice(0..0, filtered_transactions);
        } else {
            new_backup_transactions.extend(filtered_transactions);
        }

        coin.status = CoinStatus::IN_TRANSFER;
    }

    new_backup_transactions.sort_by(|a, b| a.tx_n.cmp(&b.tx_n));

    Ok(new_backup_transactions)
}

// =================================================================================================
// FLAT-LANE BOOKKEEPING — the "one coin type" step.
//
// Laddering is UNCONDITIONAL for every coin the SDK's `claim()` pass can ladder, so a coin WITHOUT a
// TES-R ladder is an exception that has to be EXPLAINABLE. There are exactly four structurally
// permanent explanations — the LICENCES (see [`PermanentLicence`]):
//   * RGB CARRIER — a tier spend carries no state transition and would destroy the allocation
//     (terminal freeze). Stays flat until CTES-R colouring lands.
//   * TERMINALIZED CARRIER (`single_use`) — the same terminal-freeze rule reached through the flag.
//   * [B0] — the coin's funding `F` is not on chain (a split sub-coin), so a trigger has no prevout.
//   * LEGACY NO-AGGREGATE — the coordinator recorded no `aggregate_xonly` for the sid (pre-0009), so
//     no receiver could bind a ladder built over it.
// Anything else conveyed flat is a BUG, and a silent flat conveyance is precisely how a coin would
// lose its census protection — so it is refused loudly instead (see `assert_flat_conveyance_is_legitimate`).
//
// ── HOW THIS IS WRITTEN, AND WHY IT IS WRITTEN THAT WAY ─────────────────────────────────────────
// THREE successive review rounds each found a NEW fail-open in the classifier, because it was
// written as "return Ok(()) unless something looks wrong": FLAT_RGB_STATE_UNAVAILABLE licensing the
// flat lane, a `let ... else` funding fallback, `Ok(None)` from the coordinator, a blanket
// `ladder_binding_precheck(..).is_err()` that read EVERY error cause as the legacy explanation, and
// a global scope gate armed by a best-effort write. Every arm was a potential hole, so patching arms
// one at a time never converged.
//
// So the polarity is INVERTED. The classifier computes an `Option<PermanentLicence>` from POSITIVE
// evidence and nothing else; `assert_flat_conveyance_is_legitimate` contains exactly ONE `Ok(())`
// statement, reached only by `Some(licence)`. Every other path — every `else`, every `Err`, every
// `None`, every unparseable row, every unreachable dependency — is a refusal. The property is
// auditable by counting: `grep -c 'Ok(())'` inside that function must print 1, and
// `the_classifier_has_exactly_one_ok_return` in the test module below asserts it on the source.
//
// The carrier half of the classification needs RGB state, which this crate deliberately does not
// have. So the SDK's ladder pass RECORDS its decision per coin (`ladderskip-<sid>`), and this crate
// reads it back — but ONLY as one of several positive witnesses, never as a blanket permission slip.
// =================================================================================================

/// Wallet-level row written by the SDK's `claim()` ladder pass: "this wallet ladders its coins, so
/// an unexplained un-laddered coin here is a bug".
pub const LADDER_MANAGED_KEY: &str = "ladder-managed";

/// Reason spellings persisted in `ladderskip-<sid>`. Stable — they are read back by
/// [`is_legitimate_flat_reason`]; do not change casually.
pub const FLAT_RGB_CARRIER: &str = "rgb-carrier";
pub const FLAT_TERMINALIZED_CARRIER: &str = "terminalized-carrier";
pub const FLAT_RGB_STATE_UNAVAILABLE: &str = "rgb-state-unavailable";
pub const FLAT_FUNDING_NOT_ONCHAIN: &str = "funding-not-onchain";
pub const FLAT_NOT_BINDABLE: &str = "not-bindable";
pub const FLAT_COORDINATOR_UNAVAILABLE: &str = "coordinator-unavailable";
pub const FLAT_ESTABLISH_FAILED: &str = "establish-failed";
pub const FLAT_DUPLICATE_DEPOSIT: &str = "duplicate-deposit";
/// [M2] The coin's `tesr-<sid>` row could not be parsed, so the pass could not tell whether it
/// already has a ladder. Recorded so the state is visible; never a licence (the conveyance path
/// refuses such a coin outright, before this record is even consulted).
pub const FLAT_LADDER_UNREADABLE: &str = "ladder-unreadable";
/// The coin's funding output could not be resolved this pass. Distinct from
/// [`FLAT_FUNDING_NOT_ONCHAIN`], which is a PERMANENT reason and therefore licenses the flat lane:
/// an electrum fault and a genuinely un-broadcast `F` are indistinguishable at the lookup, so the
/// permanent spelling is written only on positive evidence and this transient one otherwise.
pub const FLAT_FUNDING_UNRESOLVABLE: &str = "funding-unresolvable";
/// **[D69] No enclave attestation identity is pinned or configured for this network.**
///
/// PERMANENT for this build+configuration, and it licenses NOTHING. Without a key to verify the
/// enclave's attestation against, terminality cannot be established at all — and accepting the key
/// the coordinator serves beside its own signature is precisely the hole D69 closed (TRUST-MODEL
/// B11). Distinct from `coordinator-unavailable` on purpose: that one says "retry later", and
/// retrying a missing configuration forever is how a permanent fault wears a transient label.
pub const FLAT_ATTESTATION_UNPINNED: &str = "attestation-identity-unpinned";
/// **A pin IS configured, and the enclave's attestation does not verify against it.**
///
/// The sibling of [`FLAT_ATTESTATION_UNPINNED`], and the reason it had to exist. The classifier that
/// chose between "unpinned" and "coordinator-unavailable" asked only whether a pin RESOLVES, which
/// tests presence and not validity — so a present-but-wrong pin (the wrong network's key, a copied
/// JSON body instead of the bare x-only key, a redeployed enclave) fell through to
/// `coordinator-unavailable`. That reading is doubly wrong: the coordinator answered 200, and the
/// fault is local and permanent rather than something a retry can clear.
///
/// PERMANENT for this configuration, and it licenses NOTHING — an attestation that does not verify
/// is exactly the case D69 exists to refuse, so it must never be softened into "flat is fine".
pub const FLAT_ATTESTATION_INVALID: &str = "attestation-invalid";
/// The coin's ladder could not be bound for a reason that is NOT the legacy no-aggregate case — a
/// non-taproot funding output, a scriptPubKey that would not decode, or a coordinator aggregate that
/// does not control `F` (a decoy-shaped coin). Never a licence: only
/// [`crate::tesr::BindingRefusal::NoCoordinatorAggregate`] is the permanent, harmless explanation,
/// and folding the other causes into it was one of the fail-opens this module now forbids.
pub const FLAT_BINDING_UNRESOLVED: &str = "binding-unresolved";

/// Stable marker embedded in the refusal raised when the receiver-paying state `S'` could not be
/// pre-signed AFTER the SE co-sign stage was entered ([M1]). Named so a caller can branch on it
/// rather than string-matching prose: it is the one refusal that may leave the coin's `num_sigs`
/// raised, and the remedy is `renew`/`rollover` or an on-chain re-anchor (`refresh`).
pub const ERR_LADDER_COSIGN_INCOMPLETE: &str = "ladder-cosign-incomplete";

/// Wallet-DB key of a coin's recorded "left flat" reason. Duplicates are keyed apart so they can
/// never overwrite the index-0 coin's record (the sid's ladder belongs to index 0).
pub fn ladder_skip_key(statechain_id: &str, duplicate_index: u32) -> String {
    if duplicate_index == 0 {
        format!("ladderskip-{statechain_id}")
    } else {
        format!("ladderskip-{statechain_id}#{duplicate_index}")
    }
}

/// Does this recorded reason legitimise conveying the coin on the FLAT lane?
///
/// ⚠️ **This is a PREDICTION, not the decision.** The authority is
/// [`assert_flat_conveyance_is_legitimate`], which re-proves each licence from live evidence — the
/// `branch-`/`ctesr-` rows for [B0], the coin's own `single_use` flag for a terminalized carrier, the
/// coordinator's live answer for the legacy no-aggregate case. A recorded string on its own licenses
/// only [`FLAT_RGB_CARRIER`], because RGB state is the one thing this crate genuinely cannot see and
/// the recording component positively asserted it. Everything here that returns `true` is "the
/// classifier will very probably say yes"; it is exported so an app can warn ahead of a `send`.
///
/// **Only the STRUCTURALLY PERMANENT reasons predict a yes** — the four explanations named in the
/// module comment above (RGB carrier, terminalized carrier, [B0] off-chain funding, legacy
/// no-aggregate). A reason that merely says "this pass could not decide" or "this pass did not
/// succeed" is a DEFERRAL, and a deferral must never harden into a standing licence.
///
/// [B1] `rgb-state-unavailable` used to be on this list and was the worst entry on it. It is written
/// when a token wallet's RGB state was momentarily unreadable, which makes the pass skip laddering
/// for that pass and self-heal on the next one — a transient condition, recorded in exactly the
/// wallets where carriers matter. Accepting it as a licence meant one RGB blip let a coin convey
/// flat forever after. Refuse instead; the caller runs `claim()` again and the record clears.
///
/// `establish-failed` is out for the same reason, and for a second one: `establish_auto` may have
/// already landed one or more SE co-signs before failing, so the coin's `num_sigs` can be raised
/// while its bundle was never persisted. Such a coin's flat conveyance is REJECTED by the receiver's
/// census anyway ("num_sigs is not correct") after the sender's coin is already IN_TRANSFER —
/// refusing up-front converts that stuck transfer into an actionable error.
///
/// `coordinator-unavailable` is out ("we do not know" is not "flat is fine"), `duplicate-deposit` is
/// out (a duplicate is never the conveyed coin), `binding-unresolved` is out (a decoy-shaped or
/// unreadable funding output is not the harmless legacy case), and `ladder-unreadable` is out (the
/// conveyance path refuses such a coin before it ever reads this record).
pub fn is_legitimate_flat_reason(reason: &str) -> bool {
    matches!(reason, FLAT_TERMINALIZED_CARRIER | FLAT_NOT_BINDABLE)
}

/// Is this recorded reason one a LATER `claim()` pass can clear by itself? Drives the remedy named
/// in the refusal message: a transient reason means "retry after the next claim()", a permanent one
/// means the coin genuinely cannot be laddered and something else is wrong.
fn is_transient_flat_reason(reason: &str) -> bool {
    matches!(
        reason,
        FLAT_RGB_STATE_UNAVAILABLE
            | FLAT_COORDINATOR_UNAVAILABLE
            | FLAT_ESTABLISH_FAILED
            | FLAT_FUNDING_UNRESOLVABLE
    )
}

async fn read_raw_backup_row(client_config: &ClientConfig, wallet_name: &str, key: &str) -> Option<String> {
    crate::sqlite_manager::get_all_backup_txs(&client_config.pool, wallet_name)
        .await
        .ok()?
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, json)| json)
}

/// Remove a raw backup row (e.g. `tesr-<sid>` or `ladderskip-<sid>`). Absent row = no-op.
pub async fn delete_raw_backup_row(client_config: &ClientConfig, wallet_name: &str, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM backup_txs WHERE statechain_id = $1 AND wallet_name = $2")
        .bind(key)
        .bind(wallet_name)
        .execute(&client_config.pool)
        .await?;
    Ok(())
}

/// Mark this wallet as one whose coins are laddered by the SDK `claim()` pass (idempotent).
pub async fn mark_ladder_managed(client_config: &ClientConfig, wallet_name: &str) -> Result<()> {
    if read_raw_backup_row(client_config, wallet_name, LADDER_MANAGED_KEY).await.is_some() {
        return Ok(());
    }
    crate::sqlite_manager::insert_raw_backup_txs(&client_config.pool, wallet_name, LADDER_MANAGED_KEY, "1").await
}

/// Is this wallet ladder-managed? (Read-only; a DB error reads as "no", i.e. the pre-SDK lane.)
///
/// Convenience for callers that only want a hint. The CONVEYANCE path must not use it: swallowing a
/// DB error there would turn "the wallet DB is unreadable" into "the pre-SDK lane applies, convey it
/// flat" — a fail-OPEN on the scope gate itself. That path reads the rows once, through
/// [`read_backup_rows`], and propagates the error.
pub async fn is_ladder_managed(client_config: &ClientConfig, wallet_name: &str) -> bool {
    read_raw_backup_row(client_config, wallet_name, LADDER_MANAGED_KEY).await.is_some()
}

/// Every raw backup row of a wallet as `(key, json)`, propagating a DB error instead of swallowing
/// it. One read serves the whole flat-lane classification below (`ladder-managed`, `branch-<id>`,
/// `ctesr-<id>`, the coin's own backup row and `ladderskip-<id>`), so the classification cannot see
/// a half-read DB.
async fn read_backup_rows(
    client_config: &ClientConfig,
    wallet_name: &str,
) -> Result<Vec<(String, String)>> {
    crate::sqlite_manager::get_all_backup_txs(&client_config.pool, wallet_name)
        .await
        .map_err(|e| anyhow!("could not read wallet {wallet_name}'s backup rows ({e})"))
}

/// The reason a coin was last left un-laddered, if one was recorded.
pub async fn read_ladder_skip(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    duplicate_index: u32,
) -> Option<String> {
    let json = read_raw_backup_row(
        client_config,
        wallet_name,
        &ladder_skip_key(statechain_id, duplicate_index),
    )
    .await?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    v.get("reason")?.as_str().map(|s| s.to_string())
}

/// Record why a coin was left un-laddered. Returns `true` when the recorded reason CHANGED, which
/// is what the SDK uses to emit `WalletEvent::LadderSkipped` once rather than on every poll.
pub async fn record_ladder_skip(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    duplicate_index: u32,
    reason: &str,
) -> Result<bool> {
    let previous = read_ladder_skip(client_config, wallet_name, statechain_id, duplicate_index).await;
    if previous.as_deref() == Some(reason) {
        return Ok(false);
    }
    let json = serde_json::json!({ "reason": reason, "at": Utc::now().to_rfc3339() }).to_string();
    crate::sqlite_manager::insert_raw_backup_txs(
        &client_config.pool,
        wallet_name,
        &ladder_skip_key(statechain_id, duplicate_index),
        &json,
    )
    .await?;
    Ok(true)
}

/// Drop a coin's "left flat" record — it now HAS a ladder, so the record must not linger and later
/// excuse a flat conveyance.
pub async fn clear_ladder_skip(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    duplicate_index: u32,
) -> Result<()> {
    delete_raw_backup_row(
        client_config,
        wallet_name,
        &ladder_skip_key(statechain_id, duplicate_index),
    )
    .await
}

/// The ONLY things that may license conveying a coin on the FLAT (un-laddered) lane.
///
/// Each variant is a **structurally permanent** property, and each is established from POSITIVE
/// evidence by its own probe below. Nothing transient appears here by construction: there is no
/// variant for "the coordinator was down", "RGB state was unreadable" or "establish failed", so
/// those conditions cannot be spelled as a licence even by accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermanentLicence {
    /// The coin is flagged `single_use` — a terminalized/combine carrier, same terminal-freeze rule.
    /// Proven from the COIN's own flag, never from a record: this flag has no production setter
    /// today (its only setters have zero non-test callers), so assuming it would be assuming a state
    /// that does not exist.
    TerminalizedCarrier,
    /// The coordinator has no `aggregate_xonly` on record for the sid (a pre-migration-0009 legacy
    /// coin), so no receiver could bind a ladder built over it. Proven by the coordinator ANSWERING,
    /// this call, with a record whose aggregate is absent —
    /// [`crate::tesr::BindingRefusal::NoCoordinatorAggregate`] and no other cause.
    LegacyNoAggregate,
    /// **Wallet scope, not a coin property.** This wallet has provably never been through the SDK
    /// ladder pass, so it has no laddering invariant to violate and keeps its historical behaviour
    /// (the shipped Rust CLI and the pre-SDK lib clients never ladder anything; turning every one of
    /// their transfers into a hard error is a product deprecation decision, not a client-library
    /// one).
    ///
    /// "Provably" is the whole point. The gate used to be "the `ladder-managed` row is missing",
    /// which made a MISSING row a global off-switch for the entire classifier — and the write that
    /// arms that row is best-effort, so one failed insert disabled every check below for the life of
    /// the wallet. It is now [`wallet_is_provably_pre_sdk`]: the wallet must contain NO ladder
    /// artefact of any kind (`ladder-managed`, `tesr-*`, `ctesr-*`, `ladderskip-*`). A pass that got
    /// far enough to ladder or classify anything leaves one of those behind, so the gate arms itself
    /// from the pass's actual work rather than from a single best-effort marker.
    LegacyPreSdkWallet,
}

/// A wallet's raw backup rows, read once so the classification cannot see a half-read DB.
struct WalletRows(Vec<(String, String)>);

impl WalletRows {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    fn has_prefix(&self, prefix: &str) -> bool {
        self.0.iter().any(|(k, _)| k.starts_with(prefix))
    }
}

/// POSITIVE evidence that this wallet has never been through the SDK's `claim()` ladder pass: it
/// carries no ladder artefact whatsoever. See [`PermanentLicence::LegacyPreSdkWallet`].
fn wallet_is_provably_pre_sdk(rows: &WalletRows) -> bool {
    rows.get(LADDER_MANAGED_KEY).is_none()
        && !rows.has_prefix("tesr-")
        && !rows.has_prefix("ctesr-")
        // [CATS/V4] …and a SPINE TIP is a ladder artefact too. This licence is a wallet-wide
        // off-switch for the entire flat-conveyance classifier, so a missing prefix here does not
        // merely overlook one row — it hands every coin in the wallet a permanent licence to be
        // conveyed flat. A wallet whose only artefact was a spine tip (a sender who has made exactly
        // one CATS payment and holds only its change) would have qualified as "provably never
        // laddered" while holding a laddered coin.
        && !rows.has_prefix(crate::tesr::SPINE_TIP_KEY_PREFIX)
        && !rows.has_prefix("ladderskip-")
}

/// The SDK ladder pass's recorded verdict for this coin. `Ok(None)` = nothing recorded; a row that
/// exists but cannot be read is an ERROR, never "nothing recorded".
fn recorded_flat_reason(rows: &WalletRows, statechain_id: &str) -> Result<Option<String>> {
    let Some(json) = rows.get(&ladder_skip_key(statechain_id, 0)) else {
        return Ok(None);
    };
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .ok_or_else(|| {
            anyhow!(
                "statechain id {statechain_id} has no exit ladder and its flat-lane record is \
                 unreadable. Refusing to convey it on the flat lane. Run claim() to re-decide the \
                 coin's lane and retry; the coin is unaffected and still withdrawable."
            )
        })
        .map(Some)
}


/// LICENCE 2 — TERMINALIZED CARRIER, proven from the coin's own `single_use` flag.
///
/// The recorded `terminalized-carrier` spelling is NOT accepted on its own: `single_use` has no
/// production setter today, so a record claiming it without the flag is stale or wrong, and this
/// licence must be positively proven rather than assumed.
fn licence_terminalized_carrier(coin: &Coin) -> Option<PermanentLicence> {
    if coin.single_use {
        Some(PermanentLicence::TerminalizedCarrier)
    } else {
        None
    }
}


/// LICENCE 4 — LEGACY NO-AGGREGATE, re-proven LIVE against the coordinator.
///
/// Two collapses this deliberately undoes:
///   * `Ok(None)` from `/info/statechain` (HTTP 404 — "no such statechain id") used to license the
///     flat lane. It is not the legacy case and it is not an answer about the aggregate: the
///     coordinator is telling us it does not know this coin at all, which for a coin we are about to
///     transfer is an anomaly, not a permission slip;
///   * `ladder_binding_precheck(..).is_err()` used to license the flat lane on EVERY error cause, so
///     an unreadable scriptPubKey, a non-taproot funding output and a decoy-shaped coin all read as
///     "harmless pre-0009 legacy coin". Only
///     [`crate::tesr::BindingRefusal::NoCoordinatorAggregate`] means that.
async fn licence_legacy_no_aggregate(
    client_config: &ClientConfig,
    statechain_id: &str,
    coin: &Coin,
    network: &str,
) -> Result<Option<PermanentLicence>> {
    let Some(txid_str) = coin.utxo_txid.as_ref() else {
        return Err(anyhow!(
            "statechain id {statechain_id} has no exit ladder and no funding outpoint on record, so \
             this client cannot explain why it is un-laddered. Refusing to convey it on the flat \
             lane. Run claim() and retry."
        ));
    };
    let txid = txid_str.parse::<bitcoin::Txid>().map_err(|e| {
        anyhow!(
            "statechain id {statechain_id} has no exit ladder and its funding txid {txid_str} is \
             unparseable ({e}), so this client cannot explain why it is un-laddered. Refusing to \
             convey it on the flat lane."
        )
    })?;
    let Some(vout) = coin.utxo_vout else {
        return Err(anyhow!(
            "statechain id {statechain_id} has no exit ladder and no funding vout on record, so \
             this client cannot explain why it is un-laddered. Refusing to convey it on the flat \
             lane. Run claim() and retry."
        ));
    };
    // An un-broadcast `F` is a LEGITIMATE reason to be flat ([B0]) — but that is licence 3's job,
    // proven from exit material. A chain backend that cannot answer proves nothing: an electrum
    // fault and a genuinely-absent tx are indistinguishable here, so both refuse.
    let tx0 = client_config.electrum_client.transaction_get(&txid).map_err(|e| {
        anyhow!(
            "statechain id {statechain_id} has no exit ladder and its funding tx {txid} could not \
             be read from the chain backend ({e}), so this client cannot decide whether the coin \
             should have been laddered. Refusing to convey it on the flat lane — retry when the \
             chain backend is reachable, or run claim() to record the coin's lane. The coin is \
             unaffected and still withdrawable."
        )
    })?;
    let Some(f_out) = tx0.output.get(vout as usize) else {
        return Err(anyhow!(
            "statechain id {statechain_id} has no exit ladder and its funding outpoint \
             {txid}:{vout} does not exist in that transaction. Refusing to convey it on the flat \
             lane — this client cannot explain the coin's shape."
        ));
    };
    let f_spk_hex = hex::encode(f_out.script_pubkey.as_bytes());

    let info = match crate::utils::get_statechain_info(statechain_id, client_config).await {
        Ok(Some(info)) => info,
        // "We could not ask" — refuse. This is the arm a prior round collapsed with "the
        // coordinator said there is no aggregate".
        Ok(None) => {
            return Err(anyhow!(
                "statechain id {statechain_id} has no exit ladder and the coordinator has no record \
                 of it at all, so this client cannot establish that it is a legacy no-aggregate \
                 coin. Refusing to convey it on the flat lane — that is an anomaly for a coin about \
                 to be transferred, not a licence. The coin is unaffected and still withdrawable."
            ))
        }
        Err(e) => {
            return Err(anyhow!(
                "statechain id {statechain_id} has no exit ladder and the coordinator could not be \
                 reached to decide whether it should have one ({e}). Refusing to convey it on the \
                 flat lane — retry when the coordinator is reachable."
            ))
        }
    };
    match crate::tesr::ladder_binding_precheck_cause(
        statechain_id,
        &f_spk_hex,
        info.aggregate_pubkey.as_deref(),
        network,
    ) {
        // Bindable: the coin SHOULD have been laddered. No licence — the caller refuses.
        Ok(()) => Ok(None),
        Err(e) if e.cause == crate::tesr::BindingRefusal::NoCoordinatorAggregate => {
            Ok(Some(PermanentLicence::LegacyNoAggregate))
        }
        Err(e) => Err(anyhow!(
            "statechain id {statechain_id} has no exit ladder and its ladder binding could not be \
             established for a reason that is NOT the legacy no-aggregate case ({e}). Refusing to \
             convey it on the flat lane — only a coordinator that positively reports no aggregate \
             licenses the flat lane. The coin is unaffected and still withdrawable."
        )),
    }
}

/// Compute the coin's flat-lane licence from POSITIVE evidence, or refuse.
///
/// `Ok(Some(licence))` — proven; `Err` — a specific refusal; `Ok(None)` — nothing explains this coin
/// and the caller raises the generic "it should have been laddered" refusal. There is no fourth
/// outcome, and no path returns `Ok(Some(..))` without having established the evidence its variant
/// documents.
async fn flat_conveyance_licence(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    coin: &Coin,
    network: &str,
) -> Result<Option<PermanentLicence>> {
    let rows = WalletRows(read_backup_rows(client_config, wallet_name).await.map_err(|e| {
        anyhow!(
            "{e}. Refusing to convey statechain id {statechain_id} on the flat lane: without the \
             wallet's own records this client cannot tell a legitimately-flat coin from one that \
             lost its exit ladder. The coin is unaffected and still withdrawable."
        )
    })?);

    if wallet_is_provably_pre_sdk(&rows) {
        return Ok(Some(PermanentLicence::LegacyPreSdkWallet));
    }

    let recorded = recorded_flat_reason(&rows, statechain_id)?;
    let recorded = recorded.as_deref();

    // **[ONE COIN SHAPE] LICENCE 1 AND LICENCE 3 ARE RETIRED.**
    //
    // With `colored_ladder` shipping TRUE, a carrier is laddered like any other coin, so
    // "this coin is an RGB carrier" stops being a reason it may travel without a ladder. Licence 1
    // is therefore gone, not merely unused: leaving the probe in place would keep licensing every
    // carrier the coloured builder happens to refuse, which is the opposite of one coin shape.
    //
    // Licence 3 (`funding-not-onchain`) goes with it. Its three arms are NOT equivalent and the
    // retirement is deliberate for each:
    //   * `branch-` was the plain un-laddered split sub-coin — the shape this change exists to
    //     remove. Its producer, `ensure_exact_coin` -> `split_coin`, is retired below.
    //   * `ctesr-` was defensive: `UtexoWallet::transfer` routes a child to `child_retransfer`
    //     before the flat lane, and a child that reached here died on an absence anyway.
    //   * `spinetip-` was already dead: `execute_ex` refuses a tip BY NAME before this classifier
    //     runs, with a CI guard on the ordering.
    //
    // What remains is `licence_terminalized_carrier`, which is proven from the coin's own
    // `single_use` flag rather than from a recorded string, and `licence_legacy_no_aggregate`,
    // which is re-proven LIVE against the coordinator. Both are evidence about the coin; the two
    // retired here were evidence about a lane that no longer exists.
    if let Some(l) = licence_terminalized_carrier(coin) {
        return Ok(Some(l));
    }

    // A recorded verdict that none of the probes above could corroborate is a REFUSAL, and it is a
    // better-diagnosed one than the generic fallback: the SDK pass had strictly more information
    // (RGB state) and still did not record a licence this classifier can prove. The single exception
    // is `not-bindable`, which licence 4 can still prove LIVE — so that one falls through instead of
    // short-circuiting. [B1] Nothing here can turn a deferral into permission.
    if let Some(reason) = recorded {
        if reason != FLAT_NOT_BINDABLE {
            let remedy = if is_transient_flat_reason(reason) {
                "That condition is TRANSIENT: run claim() again and retry the transfer once the \
                 coin is laddered (or once the pass records a permanent reason)."
            } else {
                "Run claim() to re-decide the coin's lane and retry."
            };
            return Err(anyhow!(
                "statechain id {statechain_id} has no exit ladder and the last claim() pass did not \
                 establish that it may be conveyed flat (recorded reason: {reason}). Refusing to \
                 convey it on the flat lane — a silent flat conveyance is how a coin loses its \
                 census protection, and the receiver would reject it anyway. {remedy} The coin is \
                 unaffected and still withdrawable."
            ));
        }
    }

    licence_legacy_no_aggregate(client_config, statechain_id, coin, network).await
}

/// Refuse to convey a coin on the FLAT lane unless it is legitimately flat.
///
/// Called only when the coin has NO ladder (a coin that HAS one can no longer reach the flat lane at
/// all — see `execute`).
///
/// **THE STRUCTURAL PROPERTY OF THIS FUNCTION: it contains exactly ONE `Ok(())`, and that statement
/// is reachable only by a positive match on a proven [`PermanentLicence`].** Everything else — every
/// `else`, every `Err`, every `None`, every unparseable row, every unreachable dependency — refuses.
/// Three review rounds each found a NEW hole while this was written as "return `Ok(())` unless
/// something looks wrong", because under that shape every arm is a candidate hole and patching arms
/// one at a time does not converge. Under this shape a reader verifies the whole property by
/// counting, and `the_classifier_has_exactly_one_ok_return` below asserts the count on the source.
///
/// Consequently: RGB state unreadable, coordinator unreachable, coordinator answering "no such
/// coin", establish failed, funding unresolvable, DB read error, ladder unreadable — none of these
/// license anything. They refuse, and the message tells the caller to retry after the next
/// `claim()`. The coin is never touched by a refusal: it stays withdrawable and unilaterally
/// exitable.
pub async fn assert_flat_conveyance_is_legitimate(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    coin: &Coin,
    network: &str,
) -> Result<()> {
    let licence =
        flat_conveyance_licence(client_config, wallet_name, statechain_id, coin, network).await?;
    match licence {
        // ─── THE ONE AND ONLY SUCCESS RETURN IN THIS FUNCTION. Do not add a second one; the
        //     unit test below counts them on the source and fails if you do. ───
        Some(_) => Ok(()),
        None => Err(anyhow!(
            "statechain id {statechain_id} is an on-chain, bindable, non-carrier coin with NO exit \
             ladder — it should have been laddered by claim(). Refusing to convey it on the flat \
             lane: a silent flat conveyance is how a coin loses its census protection. Run claim() \
             to establish its ladder and retry."
        )),
    }
}

/// [M1] Everything about conveying a LADDERED coin that can be decided **without any SE co-sign and
/// without any network round-trip**. Called before the first irreversible co-sign of a transfer, so
/// a coin that cannot be conveyed is refused while it is still completely untouched.
///
/// It mirrors the pre-co-sign preconditions of [`crate::tesr::presign_receiver_state`] (state CSV
/// present, one `δ` of headroom above the floor, a usable payee) and adds a serializability smoke
/// test on the bundle — the augmented bundle differs only in already-serializable fields, so a
/// bundle that serializes here produces a payload that serializes there.
fn assert_receiver_state_is_presignable(
    bundle: &crate::tesr::TesrBundle,
    recipient_address: &str,
    statechain_id: &str,
) -> Result<()> {
    let p = bundle.params;
    let cur_csv = bundle.current().state.csv.ok_or_else(|| {
        anyhow!(
            "statechain id {statechain_id} is laddered but its current state carries no CSV, so the \
             receiver-paying state S' cannot be derived. Refusing to fall back to the flat lane — \
             the receiver would reject it on the signature census. Nothing has been co-signed: the \
             coin is untouched, still withdrawable and still unilaterally exitable."
        )
    })?;
    // [CANCEL] An outstanding conveyed co-sign makes the coin un-conveyable until it is settled —
    // decided here, with no SE call and no network, so the coin stays completely untouched.
    crate::tesr::refuse_outstanding_conveyance(bundle, "transfer").map_err(|e| {
        anyhow!(
            "{e} Refusing to fall back to the flat lane — the receiver would reject that on the \
             signature census. Nothing has been co-signed: the coin is untouched, still \
             withdrawable and still unilaterally exitable."
        )
    })?;
    // The SAME derivation `presign_receiver_state` will use, so this gate cannot pass a coin the
    // co-sign step would then refuse — nor refuse one it would have accepted.
    crate::tesr::next_rival_state_csv(bundle).map_err(|e| {
        anyhow!(
            "statechain id {statechain_id} is laddered but its receiver-paying state S' cannot be \
             built ({e}); its current state CSV is {cur_csv} (δ {}, floor {}). Refusing to fall \
             back to the flat lane — the receiver would reject it on the signature census. Renew \
             or roll the ladder over (or re-anchor the coin on-chain with refresh) and retry. \
             Nothing has been co-signed: the coin is untouched.",
            p.delta,
            p.d_floor
        )
    })?;
    mercurylib::tesr::payee_address(recipient_address, &bundle.network).map_err(|e| {
        anyhow!(
            "statechain id {statechain_id} is laddered but the recipient address cannot be used as \
             the payee of its receiver-paying state ({e}). Refusing to fall back to the flat lane. \
             Nothing has been co-signed: the coin is untouched."
        )
    })?;
    serde_json::to_string(bundle).map_err(|e| {
        anyhow!(
            "statechain id {statechain_id} is laddered but its bundle could not be serialized \
             ({e}). Refusing to fall back to the flat lane. Nothing has been co-signed: the coin is \
             untouched."
        )
    })?;
    Ok(())
}

/// Durably record `status` for the `duplicate_index == 0` coin of `statechain_id`, returning the
/// status it had before the write so an abort can put it back.
///
/// [D1 / A2] This exists because `defend_ladders`' liveness allowlist reads the coin's status from
/// the wallet DB, and the conveyance path used to set that status only in memory (see the arm-down
/// note in `execute_ex`). The row is RE-READ here rather than reusing the caller's copy: the write
/// replaces the whole `wallet_json` blob, so writing back a copy that was loaded before an SE
/// round-trip would clobber whatever else happened in the meantime. Re-reading narrows the write to
/// the single field this function owns.
pub(crate) async fn persist_coin_status(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    status: CoinStatus,
) -> Result<CoinStatus> {
    let mut wallet = get_wallet(&client_config.pool, wallet_name).await?;
    let coin = wallet
        .coins
        .iter_mut()
        .find(|c| c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0)
        .ok_or_else(|| {
            anyhow!("no duplicate_index 0 coin for statechain id {statechain_id} in wallet {wallet_name}")
        })?;
    let previous = coin.status.clone();
    if previous == status {
        return Ok(previous);
    }
    coin.status = status;
    update_wallet(&client_config.pool, &wallet).await?;
    Ok(previous)
}

pub async fn execute(
    client_config: &ClientConfig,
    recipient_address: &str,
    wallet_name: &str,
    statechain_id: &str,
    duplicated_indexes: Option<Vec<u32>>,
    force_send: bool,
    batch_id: Option<String>) -> Result<()>
{
    execute_ex(client_config, recipient_address, wallet_name, statechain_id, duplicated_indexes, force_send, batch_id, None).await
}

/// [CTES-R] [`execute`] for a COLOURED (CTES-R) ladder: the caller supplies a pre-built, pre-coloured
/// receiver-paying state `S'` and everything else is byte-identical to the plain path.
///
/// The draft is built by the caller — not here — for one hard reason: colouring needs the RGB engine,
/// whose resolver is `!Sync`, so a handle to it must never be alive across an `await`. This function
/// is `await`s from end to end. Splitting the build out is free, because a tier's txid is stable
/// across signing (`mercuryrustlib::tesr::build_colored_receiver_state` →
/// `cosign_colored_receiver_state`).
///
/// A coloured ladder conveyed WITHOUT a draft is still refused, and that refusal is the safety
/// property: `verify_bundle_bound` is colour-blind, so an uncoloured `S'` over a sealed output would
/// bind the receiver's sats while destroying the asset on first exit.
#[allow(clippy::too_many_arguments)]
pub async fn execute_colored(
    client_config: &ClientConfig,
    recipient_address: &str,
    wallet_name: &str,
    statechain_id: &str,
    duplicated_indexes: Option<Vec<u32>>,
    force_send: bool,
    batch_id: Option<String>,
    colored_state: crate::tesr::ColoredStateDraft) -> Result<()>
{
    execute_ex(client_config, recipient_address, wallet_name, statechain_id, duplicated_indexes, force_send, batch_id, Some(colored_state)).await
}

#[allow(clippy::too_many_arguments)]
async fn execute_ex(
    client_config: &ClientConfig,
    recipient_address: &str,
    wallet_name: &str,
    statechain_id: &str,
    duplicated_indexes: Option<Vec<u32>>,
    force_send: bool,
    batch_id: Option<String>,
    colored_state: Option<crate::tesr::ColoredStateDraft>) -> Result<()>
{
    let mut wallet: mercurylib::wallet::Wallet = get_wallet(&client_config.pool, &wallet_name).await?;

    let is_address_valid = mercurylib::validate_address(recipient_address, &wallet.network)?;

    if !is_address_valid {
        return Err(anyhow!("Invalid address"));
    }

    let is_coin_duplicated = wallet.coins.iter().any(|c| {
        c.statechain_id == Some(statechain_id.to_string()) &&
        c.status == CoinStatus::DUPLICATED
    });

    if is_coin_duplicated && !force_send {
        return Err(anyhow::anyhow!("Coin is duplicated. If you want to proceed, use the command '--force, -f' option. \
        You will no longer be able to move other duplicate coins with the same statechain_id and this will cause PERMANENT LOSS of these duplicate coin funds."));
    }

    // The second arm was `WITHDRAWING` twice — `X || X` — so this guard only ever fired while a
    // withdrawal was IN FLIGHT and stopped firing the moment it CONFIRMED. Found by clippy
    // (`equal expressions as operands to ||`), and it is a real hole rather than a style nit: the
    // variable is named `..._withdrawn`, the error below says "there have been withdrawals", and a
    // settled `WITHDRAWN` duplicate is precisely the case that makes the receiver reject the
    // transfer on signature count. As written, the longer you waited after withdrawing a duplicate,
    // the more likely you were to be allowed to send a coin that cannot be claimed.
    let are_there_duplicate_coins_withdrawn = wallet.coins.iter().any(|c| {
        c.statechain_id == Some(statechain_id.to_string()) &&
        (c.status == CoinStatus::WITHDRAWING || c.status == CoinStatus::WITHDRAWN) &&
        c.duplicate_index > 0
    });

    if are_there_duplicate_coins_withdrawn {
        return Err(anyhow::anyhow!("There have been withdrawals of other coins with this same statechain_id (possibly duplicates).\
        This transfer cannot be performed because the recipient would reject it due to the difference in signature count.\
        This coin can be withdrawn, however."));
    }

    let coin = &wallet.coins
        .iter()
        .filter(|c| 
            c.statechain_id == Some(statechain_id.to_string()) && 
            (c.status == CoinStatus::CONFIRMED || c.status == CoinStatus::IN_TRANSFER) && 
            c.duplicate_index == 0) // Filter coins with the specified statechain_id
        .min_by_key(|c| c.locktime.unwrap_or(u32::MAX)); // Find the one with the lowest locktime

    if coin.is_none() {
        return Err(anyhow!("No coins with status CONFIRMED or IN_TRANSFER associated with this statechain ID were found"));
    }

    let coin = coin.unwrap().clone();

    let statechain_id = coin.statechain_id.as_ref().unwrap().clone();

    // **[CATS change 2 / #145] A SPINE TIP IS REFUSED HERE, by name, whoever the caller is.**
    //
    // This refusal already existed — in ONE caller, `UtexoWallet::transfer`'s handover loop. That is
    // not where it belongs. `execute` is public, `chaos22`'s `respend` action calls it directly, and
    // a direct caller has no dispatch to route the tip away: the tip walked straight into the flat
    // lane, whose classifier LICENSES it (`PermanentLicence::FundingNotOnChain` — a tip's funding
    // `SP.out[K]` is un-broadcast, exactly like a `ctesr-` child's, and the licence exists to stop a
    // laddered coin being conveyed flat by accident).
    //
    // What stopped the conveyance was an ABSENCE: the tip has no flat backup rows, so
    // `create_backup_transactions` failed further down. That is refusal by accident, and it read to
    // `chaos22`'s oracle as an unclassified breach rather than a known limitation. Worse, it is one
    // missing guard away from being a money loss instead of an error — a flat conveyance would hand
    // the recipient a signed-once backup chain over an outpoint that does not exist on chain and
    // never will, i.e. a coin with no working exit, with no error on either side.
    //
    // Handing a tip over is a key handover PLUS a `spinetip-` conveyance, and that builder is not
    // landed. So the honest answer is a named refusal, sited where the danger is.
    //
    // The coin is untouched: still unilaterally exitable, and its cap already pays this wallet's key.
    if crate::tesr::load_spine_tip(client_config, wallet_name, &statechain_id)
        .await?
        .is_some()
    {
        return Err(anyhow!(
            "statechain id {statechain_id} is a SPINE TIP (the change leg of an earlier in-ladder \
             payment). Handing it over whole is a spine-tip conveyance, whose builder is not \
             landed. Refusing rather than conveying it on the flat lane, which would give the \
             recipient a backup chain over an un-broadcast funding output — a coin with no exit. \
             Pay FROM it instead (a spine batch carves a piece for the recipient and keeps the \
             change as the next tip), or exit it unilaterally."
        ));
    }

    let signed_statechain_id = coin.signed_statechain_id.as_ref().unwrap().clone();

    let (_, _, recipient_auth_pubkey) = decode_transfer_address(recipient_address)?;

    // ONE COIN TYPE — decide the coin's lane BEFORE anything irreversible happens.
    //
    // Both checks below must run ahead of `create_backup_transactions` (which co-signs a fresh
    // backup tx with the SE and therefore permanently raises the coin's `num_sigs`): refusing after
    // that point would leave the coin with an inflated signature count and brick its census — the
    // mirror-image of the brick this whole change exists to prevent.
    //
    //  * `Err` — a ladder we cannot READ is not a ladder we may assume away. The old code folded a
    //    DB read error into "no ladder" and conveyed flat; the receiver then rejected the message
    //    ("num_sigs is not correct") with the sender's coin stuck IN_TRANSFER. Fail closed instead.
    //  * `Ok(None)` — the coin claims to be legitimately flat; make it prove it.
    let tesr_bundle = match crate::tesr::load(client_config, wallet_name, &statechain_id).await {
        Ok(bundle) => bundle,
        Err(e) => {
            return Err(anyhow!(
                "could not read the exit ladder of statechain id {statechain_id} ({e}). Refusing to \
                 transfer: conveying a laddered coin on the flat lane would be rejected by the \
                 receiver's signature census and leave this coin stuck IN_TRANSFER. The coin is \
                 unaffected and still withdrawable."
            ))
        }
    };
    // [CTES-R] A COLOURED ladder may only be conveyed with a pre-built, pre-COLOURED receiver state.
    // `verify_bundle_bound` is entirely colour-blind, so an uncoloured `S'` over a sealed output
    // would bind the receiver's sats while destroying the allocation on first exit — and it would be
    // silent: the transaction is perfectly valid Bitcoin and every existing check passes. The
    // symmetric mistake is refused too: a coloured draft handed to a PLAIN ladder would attach a
    // transition to a coin whose other tiers carry none.
    match (tesr_bundle.as_ref().map(|b| b.is_colored()).unwrap_or(false), colored_state.is_some()) {
        (true, false) => {
            return Err(anyhow!(
                "statechain id {statechain_id} carries a COLOURED (CTES-R) ladder and must be \
                 conveyed through transfer_sender::execute_colored with a pre-coloured receiver \
                 state. A plain conveyance would co-sign an UNCOLOURED S' over a sealed output, \
                 destroying the RGB allocation. Refusing before any SE co-sign — the coin is \
                 unaffected."
            ))
        }
        (false, true) => {
            return Err(anyhow!(
                "statechain id {statechain_id} was handed a COLOURED receiver state but its ladder \
                 is not coloured — refusing before any SE co-sign."
            ))
        }
        _ => {}
    }
    if tesr_bundle.is_none() {
        assert_flat_conveyance_is_legitimate(
            client_config,
            wallet_name,
            &statechain_id,
            &coin,
            &wallet.network,
        )
        .await?;
    }

    // LIGHTNING-LATCHED TES-R TRANSFER — enabled via the HODL-latch pivot (LIGHTNING.md),
    // superseding the old blanket refusal. The Model A co-sign of the receiver-paying state S' is still
    // NOT preimage-gated (presign_receiver_state co-signs S' on a clone, unconditionally). What makes it
    // SAFE at the pre-existing operator-trust bar:
    //   * rob-SSP (a hidden lower-CSV S* the sender co-signed but omitted from the conveyed ladder) is
    //     blocked by the SSP's PRE-PAY CENSUS — peek_pending_transfers now runs verify_bundle
    //     (num_sigs == flat_backups + tiers, read from the enclave-authoritative sig_count) BEFORE
    //     send_payment (ssp.rs execute_pay), so an inflated count is refused before the LN leg.
    //   * rob-USER (the SSP broadcasting the conveyed, broadcastable S' without paying) rests on
    //     operator trust — identical to the already-shipping un-laddered LN lane (get_msg_addr already
    //     serves S' pre-pay).
    // RESIDUAL (recoverable, not theft): on ROLLBACK the orphan S' co-sign inflates the reclaimed coin's
    // sig_count, so a later verify_bundle bricks re-transfer — the coin stays fully exitable, and a
    // refresh() re-anchor restores re-transferability. Optional enclave latch-scoped terminalization
    // (LIGHTNING.md Phase 3) removes this residual but is not required to lift the guard. See sdk53
    // (guard test: a latched transfer of a laddered coin must now OPEN) and sdk63 (the LN PAY happy path).

    // NOTE: `get_new_x1` (which OPENS the transfer at the coordinator) is intentionally deferred to
    // AFTER all of the sender's own pre-signs below (transfer_signature, backup_transactions, and the
    // Model-A `presign_receiver_state` S'). None of those need `x1` (only `t1 = o1 + x1`, built inside
    // create_transfer_update_msg, does). Opening the transfer last means (a) a failed pre-sign never
    // orphans a pending statechain_transfer row, and (b) it is safe for the coordinator to refuse ANY
    // co-sign once a transfer is open (the pending-transfer lock, CHILDREN.md) — every
    // legitimate sender co-sign has already happened before the transfer is opened.
    let input_txid = coin.utxo_txid.as_ref().unwrap();
    let input_vout = coin.utxo_vout.unwrap();
    let client_seckey = coin.user_privkey.as_ref();

    let coin_amount = coin.amount.unwrap();

    let transfer_signature = create_transfer_signature(recipient_address, &input_txid, input_vout, &client_seckey)?;

    // ---- [D1 / A2] ARM THE WATCHTOWER DOWN **DURABLY**, BEFORE ANY RIVAL MATERIAL EXISTS. -------
    //
    // `defend_ladders`' liveness allowlist (L1) broadcasts a coin's retained exit chain only while
    // the coin reads CONFIRMED — and it reads that status from the wallet DB, re-loading the row on
    // every pass. Until now the only thing that moved a conveyed coin out of CONFIRMED was
    // `create_backup_transactions`, which sets `coin.status = IN_TRANSFER` **in memory**, on a copy
    // of the wallet that is not written back until `update_wallet` at the very end of this function
    // — after the coordinator transfer is open and after `transfer/update_msg` has handed the
    // recipient a co-signed, receiver-paying `S'`. So on EVERY whole-coin conveyance there was a
    // window in which the recipient held `S'` while this wallet's DISK still said CONFIRMED, and a
    // concurrent watchtower pass was admitted by L1 and would broadcast the sender's retained `S`.
    // The two spend the same `X_m.out[0]`; on the coloured lane the recipient's entire allocation
    // lives on `S'`. That is the D1 theft, one level up from the split lane, and the filter was
    // keyed on precisely the field that had not yet been written.
    //
    // WHY DURABILITY RATHER THAN A DIFFERENT KEY. The alternative was to key L1 on evidence the
    // tower can verify for itself. The only such evidence for a whole-coin hop is the COORDINATOR's
    // pending-transfer lock, and it fails on three counts: it puts a network round-trip on a
    // per-block broadcast path; an unreachable coordinator would have to be read as blindness,
    // disarming the tower during exactly the outage in which a griefer would trigger the carrier;
    // and the coordinator is not trusted for safety, so a lying "no transfer is open" would induce
    // the sender to destroy the recipient's allocation. Local durable state is checkable offline,
    // synchronously, and by the only party whose keys can produce the rival transaction. So the
    // write is made durable instead — hoisted here, ahead of every step that can produce material
    // capable of paying anybody else.
    //
    // The arm-down covers the whole irreversible region and is UNDONE if we abort before the
    // transfer is opened at the coordinator: up to `get_new_x1` nothing has escaped, so leaving the
    // coin IN_TRANSFER would strip a still-ours coin of its automatic defence for no reason. After
    // `get_new_x1` the recipient can complete the handover, so IN_TRANSFER stands — which is the
    // status the call already ended in on the success path.
    let status_before_conveyance = persist_coin_status(
        client_config,
        &wallet.name,
        &statechain_id,
        CoinStatus::IN_TRANSFER,
    )
    .await
    .map_err(|e| {
        anyhow!(
            "refusing to transfer statechain id {statechain_id}: its coin could not be durably \
             marked IN_TRANSFER before the conveyance ({e}). Proceeding would leave this wallet's \
             watchtower armed with a retained state that rivals the one the recipient is about to \
             hold over the same output — on a coloured coin that destroys their allocation. \
             Nothing has been co-signed and the coin is unchanged."
        )
    })?;
    // Keep the in-memory copy in step with the row we just wrote, so the `update_wallet` at the end
    // of this function cannot silently put CONFIRMED back.
    for c in wallet
        .coins
        .iter_mut()
        .filter(|c| c.statechain_id.as_deref() == Some(statechain_id.as_str()) && c.duplicate_index == 0)
    {
        c.status = CoinStatus::IN_TRANSFER;
    }

    let staged = async {

    // TES-R: if this coin has a persisted exit ladder, convey it so the receiver can run
    // the R′ verification (crate::tesr::verify_bundle) instead of the flat backup-count check.
    // Absent (a legitimately flat coin — carrier / [B0] / legacy-no-aggregate, proven above) →
    // protocol_version 0, and the receiver takes the un-laddered path.
    //
    // A LADDERED COIN CAN NO LONGER BE CONVEYED FLAT. Every arm below used to degrade silently to
    // `(0, None)`; each one is now a hard error. That is not a lost capability: establishing the
    // ladder permanently raised the SE's `num_sigs`, so the receiver's flat arm already rejected
    // such a message ("num_sigs is not correct") — the transfer failed anyway, mid-flight, with the
    // coin left IN_TRANSFER. Refusing here converts that into an actionable sender-side error, and
    // removes the shape in which a laddered coin could lose its census protection.
    //
    // [M1] ORDERING. This block is hoisted ABOVE `create_backup_transactions`, which co-signs a
    // fresh backup tx with the SE and thereby permanently raises the coin's `num_sigs`. Refusing
    // after that point would brick the coin's cooperative path in the act of "protecting" it — the
    // mirror image of the failure this change exists to prevent. So:
    //   (a) every precondition decidable with no SE call and no network is checked first
    //       (`assert_receiver_state_is_presignable`), and its refusals leave the coin untouched;
    //   (b) the S' co-sign runs next, still ahead of the backup co-sign, so it is the ONLY step here
    //       that can leave a co-sign behind — and it carries its own named error.
    let (protocol_version, tesr_ladder) = match tesr_bundle {
        Some(bundle) => {
            assert_receiver_state_is_presignable(&bundle, recipient_address, &statechain_id)?;
            // Model A: while we still own the coin, pre-sign the RECEIVER-paying state S' (pays the
            // recipient, one δ lower CSV) and convey the augmented bundle, so the receiver adopts a
            // complete exit chain paying them.
            //
            // A failure HERE happened at or after the SE round-trip, so the coin's `num_sigs` may
            // already have been raised by a partial co-sign. Silently degrading to the flat lane
            // would produce exactly the hard-failed transfer this change removes, so surface a
            // DISTINCT, NAMED error instead: the caller can branch on
            // `ERR_LADDER_COSIGN_INCOMPLETE` and drive the remedy.
            let presigned = match colored_state {
                // [CTES-R] The coloured sibling: one `cosign_tier` round-trip, the same as the plain
                // path, over a tier that already carries its RGB state transition.
                Some(draft) => crate::tesr::cosign_colored_receiver_state(
                    client_config,
                    &coin,
                    &bundle,
                    draft,
                    recipient_address,
                )
                .await,
                None => crate::tesr::presign_receiver_state(client_config, &coin, &bundle, recipient_address)
                    .await,
            };
            let augmented = presigned
                .map_err(|e| {
                    anyhow!(
                        "[{ERR_LADDER_COSIGN_INCOMPLETE}] statechain id {statechain_id} is laddered \
                         but its receiver-paying state S' could not be co-signed ({e}). Refusing to \
                         fall back to the flat lane — the receiver would reject it on the signature \
                         census. This transfer co-signed no backup transaction, so nothing about \
                         the coin changed on chain: it is still withdrawable and still unilaterally \
                         exitable from its existing ladder. To make it transferable again, renew or \
                         roll the ladder over, or re-anchor the coin on-chain with refresh, then \
                         retry."
                    )
                })?;
            // ---- [CANCEL] DURABLY RECORD THE CO-SIGN WE JUST HANDED OUT, BEFORE IT LEAVES. -----
            //
            // `presign_receiver_state` co-signs `S'` on a CLONE and persists nothing, so until now
            // the sender's own bundle never learned that an irreversible co-sign had been spent on
            // a state paying somebody else. On the happy path that is harmless — the coin is gone.
            // On a CANCELLATION it was two defects at once: the enclave's MONOTONIC `sig_count` had
            // been raised by a tier nobody could disclose, so the receiver's exact-equality census
            // came up short by one FOREVER (the coin stayed exitable but was dead for onward
            // payment); and the rung `S'` consumed was not consumed, so a re-address rebuilt the
            // replacement recipient's state at the SAME CSV over the SAME outpoint as the cancelled
            // one — a first-seen race, i.e. a payment-theft vector.
            //
            // This write is what `reclaim_cancelled_conveyance` reads. It happens BEFORE the
            // material can reach anyone: `get_new_x1` (which opens the transfer) and
            // `transfer/update_msg` (which posts the ciphertext) are both still below.
            //
            // A FAILURE HERE IS FATAL TO THE TRANSFER, deliberately. The co-sign has already
            // happened; continuing would convey the coin with the orphan unrecorded, which is
            // exactly the state this change exists to remove.
            let mut retained = bundle.clone();
            retained.conveyed_states.push(augmented.current().state.clone());
            crate::tesr::persist(client_config, &wallet.name, &retained)
                .await
                .map_err(|e| {
                    anyhow!(
                        "[{ERR_LADDER_COSIGN_INCOMPLETE}] statechain id {statechain_id}'s \
                         receiver-paying state S' was co-signed, but the record of it could not be \
                         written to this wallet ({e}). Refusing to convey: an unrecorded co-sign \
                         cannot be disclosed if the transfer is later cancelled, which would leave \
                         the coin unable to be claimed by anyone and would let a cancelled \
                         recipient tie with a replacement one. The coin is still withdrawable and \
                         still unilaterally exitable; renew or roll the ladder over, or re-anchor \
                         it on-chain with refresh, before retrying."
                    )
                })?;

            let json = serde_json::to_string(&augmented).map_err(|e| {
                anyhow!(
                    "[{ERR_LADDER_COSIGN_INCOMPLETE}] statechain id {statechain_id} is laddered but \
                     its augmented bundle could not be serialized ({e}). Refusing to fall back to \
                     the flat lane. The receiver-paying state was already co-signed, so renew or \
                     roll the ladder over (or re-anchor with refresh) before retrying."
                )
            })?;
            (2u32, Some(json))
        }
        None => (0u32, None),
    };

    let backup_transactions = create_backup_transactions(client_config, recipient_address, &mut wallet, &statechain_id, duplicated_indexes).await?;

    // Off-chain sub-coins carry an exit branch (stored under "branch-<id>"): fully-signed txs
    // linking the un-broadcast funding tx to an on-chain outpoint. Attach it so the receiver can
    // validate and later unilaterally exit.
    let branch_txs: Vec<String> = get_backup_txs(&client_config.pool, &wallet.name, &format!("branch-{}", statechain_id))
        .await
        .map(|txs| txs.iter().map(|b| b.tx.clone()).collect())
        .unwrap_or_default();

    // Structural ancestor statechain ids (stored under "parents-<id>", each row's tx field holds
    // one id) that the receiver must verify are terminal at the SE before accepting the sub-coin.
    let terminal_parents: Vec<String> = get_backup_txs(&client_config.pool, &wallet.name, &format!("parents-{}", statechain_id))
        .await
        .map(|txs| txs.iter().map(|b| b.tx.clone()).collect())
        .unwrap_or_default();

    Ok::<_, anyhow::Error>((protocol_version, tesr_ladder, backup_transactions, branch_txs, terminal_parents))
    }
    .await;

    // Everything above happened BEFORE the transfer was opened at the coordinator, so an abort here
    // has conveyed nothing and the coin is still wholly ours — put its status back rather than
    // leaving a live coin without a watchtower. A failed restore is NOT swallowed: it is appended to
    // the error, because it is the only remaining way the caller learns their coin is defenceless.
    let (protocol_version, tesr_ladder, backup_transactions, branch_txs, terminal_parents) =
        match staged {
            Ok(v) => v,
            Err(e) => {
                return Err(
                    match persist_coin_status(
                        client_config,
                        wallet_name,
                        &statechain_id,
                        status_before_conveyance,
                    )
                    .await
                    {
                        Ok(_) => e,
                        Err(restore_err) => anyhow!(
                            "{e}\n\nAND the coin's status could not be restored afterwards \
                             ({restore_err}): statechain id {statechain_id} is left marked \
                             IN_TRANSFER even though nothing was conveyed, so `defend_ladders` will \
                             not drive its ladder. Nothing was given away, but the coin is \
                             UNDEFENDED until the status is repaired — restore it, or exit the coin."
                        ),
                    },
                );
            }
        };

    // Open the transfer at the coordinator now, AFTER every sender pre-sign above (see the note by
    // `input_txid`). Returns x1 for the t1 blinding tweak.
    //
    // A FAILURE HERE IS NOT A NO-OP, AND THAT IS WHY IT IS NOT A BARE `?`. Every sender pre-sign is
    // already done: `S'` has been co-signed, the enclave's MONOTONIC `sig_count` has been raised,
    // and the record of it is persisted in `conveyed_states` (see the note by that write). The
    // coordinator refusing NOW — a cancelled recipient key, an open transfer, a batch clash, a dead
    // socket — leaves that co-sign orphaned with NO transfer to cancel, so the cancellation path
    // that would normally reconcile it can never be reached. The coin would be permanently
    // unconveyable: every later `transfer` refuses on the outstanding conveyed state, and the only
    // remedy left is an on-chain re-anchor.
    //
    // So reconcile it here, with the SAME primitive the cancellation uses — co-sign one replacement
    // state strictly below the orphan and demote the orphan into `superseded_states`, where the next
    // receiver's census counts it and the verifier proves it non-confirmable. Then report the
    // ORIGINAL refusal, because that is what the caller asked about; the reconciliation is
    // bookkeeping, and its own outcome is appended rather than substituted.
    //
    // Best-effort BY CONSTRUCTION, not by carelessness: the reclaim needs a fresh SE co-signature,
    // and the very condition that refused the open (an open transfer of this coin) is one the SE
    // also refuses to co-sign under. When that happens the coin stays wedged and the message says
    // so, naming the retry — which is strictly better than today's silent wedge.
    let x1 = match get_new_x1(&client_config, &statechain_id, &signed_statechain_id, &recipient_auth_pubkey.to_string(), batch_id).await {
        Ok(x1) => x1,
        Err(open_err) => {
            let reconciled =
                crate::tesr::reclaim_cancelled_conveyance(client_config, wallet_name, &coin).await;
            return Err(match reconciled {
                Ok(true) => anyhow!(
                    "the coordinator refused to open the transfer of statechain id \
                     {statechain_id} ({open_err}). The receiver-paying state co-signed for this \
                     attempt has been folded into the coin's disclosed superseded states, so the \
                     coin is transferable again — retry the transfer."
                ),
                Ok(false) => anyhow!(
                    "the coordinator refused to open the transfer of statechain id \
                     {statechain_id} ({open_err}). Nothing needed reconciling."
                ),
                Err(reclaim_err) => anyhow!(
                    "the coordinator refused to open the transfer of statechain id \
                     {statechain_id} ({open_err}), AND the receiver-paying state already co-signed \
                     for this attempt could not be reconciled afterwards ({reclaim_err}). That \
                     co-sign is orphaned: until it is folded into the bundle's superseded states \
                     this coin cannot be transferred onward, because the next receiver would refuse \
                     it on the signature census. It remains fully withdrawable and unilaterally \
                     exitable. Settle whatever the coordinator is refusing on and retry the \
                     transfer, or re-anchor the coin on-chain with refresh."
                ),
            });
        }
    };

    let transfer_update_msg_request_payload = create_transfer_update_msg_with_branch(&x1, recipient_address, &coin, &transfer_signature, &backup_transactions, &branch_txs, &terminal_parents, protocol_version, tesr_ladder)?;

    let endpoint = client_config.statechain_entity.clone();
    let path = "transfer/update_msg";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    let status = request.json(&transfer_update_msg_request_payload).send().await?.status();

    if !status.is_success() {
        return Err(anyhow::anyhow!("Failed to update transfer message".to_string()));
    }

    update_backup_txs(&client_config.pool, &wallet.name, &coin.statechain_id.as_ref().unwrap(), &backup_transactions).await?;

    let date = Utc::now(); // This will get the current date and time in UTC
    let iso_string = date.to_rfc3339(); // Converts the date to an ISO 8601 string

    let utxo = format!("{}:{}", input_txid, input_vout);

    let activity = Activity {
        utxo,
        amount: coin_amount,
        action: "Transfer".to_string(),
        date: iso_string
    };

    wallet.activities.push(activity);

    update_wallet(&client_config.pool, &wallet).await?;

    Ok(())
}

async fn create_backup_tx_to_receiver(client_config: &ClientConfig, coin: &mut Coin, bkp_tx1: &BackupTx, recipient_address: &str, qt_backup_tx: u32, network: &str) -> Result<String> {

    let block_height = Some(get_blockheight(bkp_tx1)?);

    let server_info = info_config(&client_config).await?;

    let fee_rate_sats_per_byte = if server_info.fee_rate_sats_per_byte > client_config.max_fee_rate {
        client_config.max_fee_rate
    } else {
        server_info.fee_rate_sats_per_byte
    };

    let is_withdrawal = false;
    let signed_tx = new_transaction(
        client_config, 
        coin, 
        recipient_address, 
        qt_backup_tx, 
        is_withdrawal, 
        block_height, 
        network, 
        fee_rate_sats_per_byte, 
        server_info.initlock,
        server_info.interval).await?;

    Ok(signed_tx)
}

pub async fn get_new_x1(client_config: &ClientConfig,  statechain_id: &str, signed_statechain_id: &str, recipient_auth_pubkey: &str, batch_id: Option<String>) -> Result<String> {
    
    let endpoint = client_config.statechain_entity.clone();
    let path = "transfer/sender";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    let transfer_sender_request_payload = TransferSenderRequestPayload {
        statechain_id: statechain_id.to_string(),
        auth_sig: signed_statechain_id.to_string(),
        new_user_auth_key: recipient_auth_pubkey.to_string(),
        batch_id,
    };

    let value = match request.json(&transfer_sender_request_payload).send().await {
        Ok(response) => {

            let status = response.status();
            let text = response.text().await.unwrap_or("Unexpected error".to_string());

            if status.is_success() {
                text
            } else {
                return Err(anyhow::anyhow!(format!("status: {}, error: {}", status, text)));
            }
        },
        Err(err) => {
            return Err(anyhow::anyhow!(format!("status: {}, error: {}", err.status().unwrap(),err.to_string())));
        },
    };

    // A response body is NETWORK INPUT. The status has already been checked above, so this is a 2xx
    // whose shape we did not expect — a coordinator we do not understand, or something in front of
    // it. That is an error the caller can handle; it is never a reason to abort its task, which is
    // what the `.expect()` that used to be here did.
    let response: TransferSenderResponsePayload = serde_json::from_str(value.as_str())
        .map_err(|e| anyhow!("transfer/sender returned a body this client cannot read ({e}): {}", crate::utils::server_message_from_body(&value)))?;

    Ok(response.x1)
}



/// Outcome of a successful [`cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The transfer was withdrawn by this call; the coin is spendable again.
    Cancelled,
    /// The transfer had already been cancelled. Reported separately so a retry after a dropped
    /// response is not mistaken for a fresh cancellation, but it is a success either way.
    AlreadyCancelled,
}

/// The coordinator refused the cancellation, and this is the rule that refused it.
///
/// A TYPE, not a formatted string, for the same reason [`crate::transfer_receiver::TransferWasCancelled`]
/// is: every one of these refusals means the pending-transfer lock is STILL HELD, and a caller that
/// can only string-match cannot reliably tell that from a success. `decision` is `None` only for a
/// code this client does not know — which is still a refusal, never a success.
#[derive(Debug, Clone)]
pub struct CancelRefused {
    pub statechain_id: String,
    /// The wire code, verbatim, even when this client cannot name it.
    pub code: String,
    /// The coordinator's message, verbatim. Never reworded locally: rewording hides which rule fired.
    pub message: String,
    /// Which row of the authorization table refused, when this client recognises the code.
    pub decision: Option<mercurylib::transfer::cancel::CancelDecision>,
}

impl std::fmt::Display for CancelRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cancellation of statechain id {} refused ({}): {}",
            self.statechain_id, self.code, self.message
        )
    }
}

impl std::error::Error for CancelRefused {}

/// The ONE refusal a caller can actually do something about: the mailbox message was conveyed, so
/// the recorded recipient must co-sign.
///
/// Distinct from [`CancelRefused`] because it is the only refusal with a REMEDY, and the remedy
/// needs the key programmatically: the recipient — who is usually in a different wallet, on a
/// different device — mints a consent token with [`cancel_consent`] and hands it back for
/// [`cancel_with_consent`]. Scraping the key out of prose is not a remedy.
#[derive(Debug, Clone)]
pub struct CancelNeedsRecipientConsent {
    pub statechain_id: String,
    /// The auth key the coordinator RECORDED as this transfer's recipient. `None` only if the
    /// coordinator declined to name it, in which case there is nothing to route the request to.
    pub recipient_auth_pub_key: Option<String>,
    /// The coordinator's verbatim message.
    pub message: String,
}

impl std::fmt::Display for CancelNeedsRecipientConsent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The coordinator's text comes FIRST and unaltered — the live-stack test pins
        // "the recipient must co-sign the cancellation" and the whole point of a named refusal is
        // that it reads the same everywhere.
        write!(f, "{}", self.message)?;
        if let Some(key) = &self.recipient_auth_pub_key {
            write!(
                f,
                " (statechain id {}, recipient auth key {}). This wallet does not hold that key, so \
                 it cannot produce the consent. Ask the recipient to co-sign with `cancel_consent` \
                 and pass the token to `cancel_with_consent`, or wait for the coordinator's \
                 transfer window to expire.",
                self.statechain_id, key
            )
        } else {
            write!(
                f,
                " (statechain id {}; the coordinator did not name the recipient key)",
                self.statechain_id
            )
        }
    }
}

impl std::error::Error for CancelNeedsRecipientConsent {}

/// This client's reading of a `POST /transfer/cancel` response body.
///
/// Deliberately a THREE-way split rather than `Result<CancelOutcome>`: the consent case is neither
/// a success nor a dead end, it is the point at which the cooperative flow starts. Keeping it
/// separate is what lets [`cancel`] retry in-wallet and a cross-wallet caller route it onward,
/// without either of them re-parsing a string.
#[derive(Debug)]
pub enum CancelReply {
    /// The lock is released (or was already).
    Done(CancelOutcome),
    /// Conveyed and unclaimed: the recorded recipient must co-sign.
    NeedsRecipientConsent(CancelNeedsRecipientConsent),
    /// Every other answer. The lock is still held.
    Refused(CancelRefused),
}

impl CancelReply {
    /// Collapse to the caller-facing result. Both refusals become TYPED errors carried by
    /// `anyhow::Error::new`, so `downcast_ref` still works after propagation.
    pub fn into_result(self) -> Result<CancelOutcome> {
        match self {
            CancelReply::Done(outcome) => Ok(outcome),
            CancelReply::NeedsRecipientConsent(c) => Err(anyhow::Error::new(c)),
            CancelReply::Refused(r) => Err(anyhow::Error::new(r)),
        }
    }
}

/// Read a cancel response body into a [`CancelReply`]. Pure — no I/O, so the mapping is exhaustively
/// testable without a coordinator.
///
/// EXHAUSTIVE BY CONSTRUCTION: the match walks the `CancelDecision` list and anything that is not on
/// it falls to `Refused`, never to a success. There is deliberately no `_ => Ok(Cancelled)` arm; an
/// old client meeting a newer coordinator's ninth decision must report a refusal, because telling
/// the sender its coin is free while the coordinator still holds the lock is exactly the
/// silent-degradation shape this repo has a CI guard for.
pub fn read_cancel_reply(
    statechain_id: &str,
    body: &mercurylib::transfer::cancel::TransferCancelResponsePayload,
) -> CancelReply {
    use mercurylib::transfer::cancel::CancelDecision::*;

    // Compare against the policy enum's own codes rather than string literals, so the client and the
    // coordinator cannot drift apart on the wire vocabulary.
    let decision = [
        Allow,
        AlreadyCancelled,
        NoSuchTransfer,
        AlreadyClaimed,
        Batched,
        ClaimInFlight,
        RecipientConsentRequired,
        RecipientSignatureInvalid,
        RecipientConsentStale,
    ]
    .into_iter()
    .find(|d| d.code() == body.code);

    match decision {
        Some(Allow) => CancelReply::Done(CancelOutcome::Cancelled),
        Some(AlreadyCancelled) => CancelReply::Done(CancelOutcome::AlreadyCancelled),
        Some(RecipientConsentRequired) => {
            CancelReply::NeedsRecipientConsent(CancelNeedsRecipientConsent {
                statechain_id: statechain_id.to_string(),
                recipient_auth_pub_key: body.recipient_auth_pub_key.clone(),
                message: body.message.clone(),
            })
        }
        other => CancelReply::Refused(CancelRefused {
            statechain_id: statechain_id.to_string(),
            code: body.code.clone(),
            message: body.message.clone(),
            decision: other,
        }),
    }
}

/// The refusal code stamped on a response body this client could not read as a decision.
///
/// Prefixed so it can never be confused with a coordinator decision code: every `CancelDecision`
/// code is a bare lower-snake word (`already_claimed`, `batched_transfer`, …) and none of them
/// begins with `http_`. A caller that branches on `code` therefore sees "this is the HTTP status,
/// not a rule" without having to know the decision vocabulary.
fn http_fallback_code(http_status: u16) -> String {
    format!("http_{http_status}")
}

/// Read a `POST /transfer/cancel` HTTP response — STATUS and RAW BODY — into a [`CancelReply`],
/// whatever shape the body arrived in. Pure, so every shape is testable without a coordinator.
///
/// # Why this exists rather than a bare `serde_json::from_str`
///
/// The endpoint has TWO body shapes. A DECISION is `{code, message, recipient_auth_pub_key}`.
/// Everything that happens before the decision is reached — the sender signature that did not
/// verify, a fail-closed DB fault — is the coordinator's generic `{"message": "…"}`, which carries
/// no `code`. Deserialising every response as a decision therefore turned a well-formed, meaningful
/// refusal into `could not read the cancel response (missing field 'code')`: the server's actual
/// sentence was dropped, and the failure read as a client parsing bug rather than as the refusal it
/// was. Whoever met it went looking in the wrong half of the system.
///
/// So an unreadable body degrades to what the server DID say — its `message` and its HTTP status —
/// and never to a parse error.
///
/// # It degrades to a REFUSAL, never to a success
///
/// Including on a 2xx. The lock this endpoint releases is the only thing standing between a
/// conveyed-but-unclaimed recipient and a sender who co-signs a rival state, so "I could not read
/// the answer" must resolve the same way every other unknown does in this file: the lock is still
/// held. There is deliberately no status-based success arm — a 200 whose body this client cannot
/// read is a coordinator this client does not understand, which is exactly the case
/// `an_unknown_code_is_refused_not_treated_as_success` already covers one layer up.
pub fn read_cancel_response(statechain_id: &str, http_status: u16, text: &str) -> CancelReply {
    if let Ok(body) =
        serde_json::from_str::<mercurylib::transfer::cancel::TransferCancelResponsePayload>(text)
    {
        return read_cancel_reply(statechain_id, &body);
    }

    CancelReply::Refused(CancelRefused {
        statechain_id: statechain_id.to_string(),
        code: http_fallback_code(http_status),
        // Verbatim, for the same reason `read_cancel_reply` keeps the coordinator's words: rewording
        // hides which rule fired, and here it is the ONLY thing left that says what happened. The
        // extraction is `crate::utils`' — ONE definition, shared with every other client read of a
        // coordinator refusal, so the sites cannot drift apart.
        message: crate::utils::server_message_from_body(text),
        // No decision was named, so this client must not name one.
        decision: None,
    })
}

/// The local status a coin should be moved to after a SUCCESSFUL cancellation, or `None` to leave
/// it exactly as it is.
///
/// Only `IN_TRANSFER` is restored, and only to `CONFIRMED`. Both halves matter. Leaving a cancelled
/// coin `IN_TRANSFER` hides it from selection and from `defend_ladders` forever — the coin is on
/// chain and safe, but the wallet will not spend it, which is the likeliest way to make a cancelled
/// coin unusable. Stamping `CONFIRMED` over anything ELSE is worse: a `WITHDRAWING` /`WITHDRAWN` /
/// `INVALIDATED` coin is not the sender's to spend, and resurrecting it would put a coin the wallet
/// cannot co-sign back into the selection set.
pub fn status_after_cancel(current: &CoinStatus) -> Option<CoinStatus> {
    match current {
        CoinStatus::IN_TRANSFER => Some(CoinStatus::CONFIRMED),
        _ => None,
    }
}

// ==============================================================================================
// THE CONSENT TOKEN
// ==============================================================================================

/// A recipient's consent, as it travels out of band between two wallets.
///
/// ONE opaque string, deliberately. The signature and the statement of what it is a signature FOR
/// must not be separable by whatever carries them — a chat message, a QR code, a support ticket —
/// because a consent whose subject has been detached is exactly the unbound token the coordinator
/// now refuses as [`CancelDecision::RecipientConsentStale`]. Wire form:
///
/// ```text
/// <nonce>:<sig>:<digest>
/// ```
///
/// `<nonce>:<sig>` is the ordinary single-use auth token the endpoint already understands; `<digest>`
/// is [`mercurylib::transfer::cancel::transfer_consent_digest`] over the mailbox ciphertext the
/// recipient downloaded. The two travel together and are submitted in separate payload fields, so
/// the server's `split_once(':')` parse is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentToken {
    /// The `"<nonce>:<sig>"` half, verbatim, for `TransferCancelRequestPayload::recipient_auth_sig`.
    pub nonce_sig: String,
    /// The transfer-instance digest this consent is bound to.
    pub transfer_digest: String,
}

impl ConsentToken {
    /// The single string to hand to the sender.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.nonce_sig, self.transfer_digest)
    }

    /// Read a token back, REFUSING anything that does not carry a binding.
    ///
    /// A legacy `"<nonce>:<sig>"` token parses to an error rather than to an unbound consent. It
    /// would be refused by the coordinator anyway (that is the whole fix), but refusing it here
    /// gives the sender a comprehensible local error instead of a confusing `recipient_consent_stale`
    /// from a server it will suspect of being broken.
    pub fn parse(token: &str) -> Result<Self> {
        let fields: Vec<&str> = token.split(':').collect();
        let [nonce, sig, digest] = fields.as_slice() else {
            return Err(anyhow!(
                "malformed consent token: expected exactly three colon-separated fields \
                 (<nonce>:<signature>:<transfer digest>), found {}. A two-field token is the legacy \
                 unbound shape and is no longer accepted — ask the recipient to mint a fresh one.",
                fields.len()
            ));
        };
        if nonce.is_empty() || sig.is_empty() {
            return Err(anyhow!("malformed consent token: empty nonce or signature"));
        }
        // A well-formed digest is the only thing that makes this a BOUND consent, and the
        // coordinator applies the same shape test — see `cancel::consent_binding_is_stale`. Reject
        // here so an obviously unusable token never burns the sender's own nonce on a round trip.
        if mercurylib::transfer::cancel::consent_binding_is_stale(Some(digest), Some(digest)) {
            return Err(anyhow!(
                "malformed consent token: '{digest}' is not a transfer digest (expected 64 hex characters)"
            ));
        }
        Ok(ConsentToken {
            nonce_sig: format!("{nonce}:{sig}"),
            transfer_digest: digest.to_string(),
        })
    }
}

// ==============================================================================================
// WHO MAY MINT A CONSENT
// ==============================================================================================

/// Why this wallet will not sign a consent for a named coin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentBlocked {
    /// No transfer of this coin is addressed to any key this wallet holds. This wallet is not the
    /// recorded recipient and has no standing to consent to anything about it.
    NoPendingTransfer,
    /// This wallet already completed the handover for this coin on that receiving slot. Consenting
    /// to cancel a payment you have already taken is a footgun; the coordinator refuses it too, but
    /// only after the signature has been given.
    AlreadyClaimed,
    /// This wallet previewed a transfer but no longer holds the receiving slot it was addressed to —
    /// a restore from an older backup, or a coin pruned between the preview and the consent. There
    /// is no key to sign with, and there must be no falling back to another one.
    ReceivingSlotGone,
}

/// This wallet declines to mint a consent, and why — a TYPE, so a caller (or a UI) can distinguish
/// "you were never the recipient" from "you already took this" without parsing prose.
#[derive(Debug, Clone)]
pub struct ConsentUnavailable {
    pub statechain_id: String,
    pub reason: ConsentBlocked,
}

impl std::fmt::Display for ConsentUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reason {
            ConsentBlocked::NoPendingTransfer => write!(
                f,
                "this wallet holds no pending transfer of statechain id {}, so it cannot consent to \
                 cancelling one. Only the recorded recipient's consent releases a conveyed transfer, \
                 and being asked for one by a sender is not evidence of being that recipient.",
                self.statechain_id
            ),
            ConsentBlocked::AlreadyClaimed => write!(
                f,
                "this wallet has already claimed statechain id {}; the payment is complete and \
                 cannot be cancelled. Do not consent to withdrawing a coin you already hold.",
                self.statechain_id
            ),
            ConsentBlocked::ReceivingSlotGone => write!(
                f,
                "this wallet decrypted a transfer of statechain id {} but no longer holds the \
                 receiving slot it was addressed to, so it cannot sign a consent for it. Signing \
                 with any other key is not consent — it is an invalid signature the coordinator \
                 would reject.",
                self.statechain_id
            ),
        }
    }
}

impl std::error::Error for ConsentUnavailable {}

/// The transfer-instance digest for a pending transfer THIS WALLET DECRYPTED — computed over the
/// ciphertext actually downloaded from `get_msg_addr`, so the recipient signs over material it has
/// seen rather than over a coordinator assertion.
pub fn consent_digest_for(pending: &PendingTransferInfo) -> String {
    mercurylib::transfer::cancel::transfer_consent_digest(
        &pending.statechain_id,
        &pending.recipient_auth_pub_key,
        &pending.encrypted_transfer_msg,
    )
}

/// Pick the pending transfer a consent would be for, or refuse BY NAME.
///
/// Pure, so the two refusals that make this primitive safe are testable without a coordinator.
///
/// `claimed` is `(statechain_id, auth_pubkey)` for every coin this wallet holds. The already-claimed
/// test is keyed on the RECEIVING SLOT, not on the coin: in a self-addressed transfer one wallet
/// holds both legs and the SENDER's coin carries the same statechain id, so matching on the coin
/// alone would report every self-addressed cancellation as already claimed.
pub fn select_consent_target<'a>(
    statechain_id: &str,
    pending: &'a [PendingTransferInfo],
    claimed: &[(String, String)],
) -> std::result::Result<&'a PendingTransferInfo, ConsentUnavailable> {
    let blocked = |reason| ConsentUnavailable { statechain_id: statechain_id.to_string(), reason };

    // Being addressed to us is established by DECRYPTION, upstream in `peek_pending_transfers` —
    // the message is in this list only because a private key this wallet holds opened it.
    let target = pending
        .iter()
        .find(|p| p.statechain_id == statechain_id)
        .ok_or_else(|| blocked(ConsentBlocked::NoPendingTransfer))?;

    if claimed
        .iter()
        .any(|(sid, key)| sid == statechain_id && *key == target.recipient_auth_pub_key)
    {
        return Err(blocked(ConsentBlocked::AlreadyClaimed));
    }

    Ok(target)
}

/// Fetch a single-use auth challenge and sign it with `coin`'s auth key, producing the
/// `"<nonce>:<sig>"` token the nonce-protected endpoints expect.
async fn nonce_auth_token(
    client_config: &ClientConfig,
    statechain_id: &str,
    endpoint: &str,
    coin: &Coin,
) -> Result<String> {
    let client = client_config.get_reqwest_client()?;
    let response = client
        .get(&format!("{}/auth/challenge/{}", client_config.statechain_entity, statechain_id))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(anyhow!("auth challenge failed: {}", response.text().await?));
    }
    let v: serde_json::Value = response.json().await?;
    let nonce = v
        .get("nonce")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow!("no nonce in auth challenge response"))?;
    let sig = mercurylib::transfer::receiver::sign_message(&format!("{nonce}|{endpoint}"), coin)?;
    Ok(format!("{nonce}:{sig}"))
}

/// Withdraw an opened transfer that nobody has claimed, so the coin can be spent again.
///
/// # When this works, and when it does not
///
/// The coordinator's pending-transfer lock is what stops a sender from co-signing a rival state
/// while a recipient holds claimable material, so cancellation is not a power the sender simply has.
/// The rule (`mercurylib::transfer::cancel`) is:
///
/// * the mailbox message was never posted — the sender's own authorization is enough;
/// * the message WAS posted — the recorded recipient must co-sign the cancellation.
///
/// This function supplies the recipient's co-signature automatically **only when this wallet holds
/// the recipient key** — a transfer addressed to one of our own addresses, or to another address in
/// the same wallet. That is not a loophole: it is the same rule, satisfied by a wallet that happens
/// to be both parties. When the recipient is someone else, the call returns the server's named
/// refusal, and the honest paths from there are to get the recipient to co-sign or to wait out the
/// coordinator's expiry window. There is no force flag, and adding one would reopen the two-victim
/// break the rule exists to close.
///
/// # Effects on local state
///
/// On success the coin's status is restored from `IN_TRANSFER` to `CONFIRMED` and a `TransferCancel`
/// activity is recorded. The backup transactions built for the abandoned transfer are deliberately
/// left in place: they are already co-signed, they are the coin's unilateral exit for the state it
/// is in, and the next transfer appends to the chain rather than replacing it.
pub async fn cancel(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
) -> Result<CancelOutcome> {
    let wallet = get_wallet(&client_config.pool, wallet_name).await?;
    let sender_coin = sender_coin_for_cancel(&wallet, wallet_name, statechain_id)?;

    // Attempt 1: sender authorization only. This succeeds outright when nothing was conveyed, and
    // otherwise comes back naming the recipient key whose consent is required.
    let reply = post_cancel(client_config, statechain_id, &sender_coin, None).await?;

    let needs = match reply {
        CancelReply::NeedsRecipientConsent(needs) => needs,
        // Success or a refusal with no remedy — settle it here.
        other => return finish_cancel(client_config, wallet_name, &sender_coin, other).await,
    };

    let Some(recipient_key) = needs.recipient_auth_pub_key.clone() else {
        return Err(anyhow::Error::new(needs));
    };

    // Do we hold the recipient key? Only then can this one wallet be both parties. If not, hand the
    // caller the TYPED refusal carrying the key, so it can route the consent request to whoever does
    // hold it (`cancel_consent` there, `cancel_with_consent` back here) instead of scraping prose.
    if !wallet.coins.iter().any(|c| c.auth_pubkey == recipient_key) {
        return Err(anyhow::Error::new(needs));
    }

    cancel_with_consent_inner(client_config, wallet_name, statechain_id, &recipient_key, None).await
}

// ==============================================================================================
// WHAT THE RECIPIENT IS SHOWN, AND THEREFORE WHAT IT SIGNS
//
// The type below used to be a `pub struct` with every field `pub`, under a docstring claiming
// "every field here is derived from a message this wallet decrypted". Anyone could write
// `CancelConsentRequest { amount: 10_000, transfer_digest: <the million-sat coin's>, .. }`, which
// is the misdirection attack with the loose identifiers merely reshuffled into one object: a
// number shown to a human, paired with a binding that abandons something else.
//
// `mod linked` (tesr.rs:83) and `mod gated` (combine.rs:230) are this repo's precedent, and their
// reasoning applies unchanged — an encapsulation claim a comment defends is not defended. Every
// field is private, the type lives in a private module, and the module's only constructor derives
// ALL of them, INCLUDING the digest, from ONE decrypted message. The fields therefore cannot be
// made to disagree with one another, which is the entire property a preview needs: the amount on
// screen and the bytes signed describe the same transfer, or there is no object at all.
// ==============================================================================================
mod previewed {
    use super::PendingTransferInfo;

    /// What a recipient is being asked to abandon — everything a person needs to decide, established
    /// LOCALLY.
    ///
    /// Every field is derived, by [`CancelConsentRequest::from_decrypted`], from a message this
    /// wallet DECRYPTED with its own key. Nothing in it is asserted by the sender. That is the
    /// difference between informed consent and a phishing primitive: an API that takes a coin id and
    /// a key from a counterparty and signs a nonce lets that counterparty describe the transaction
    /// however it likes ("consent to cancelling the small one") while naming a different one.
    ///
    /// # What this deliberately does NOT say
    ///
    /// **Whose transfer it is.** `TransferMsg` carries no sender identity — no name, no label, no
    /// memo — so there is nothing honest to put here. A UI built on this must not invent one. The
    /// previous owner's `user_public_key` is the only related value on the wire and it is not a
    /// meaningful answer to "who is asking me to do this".
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CancelConsentRequest {
        statechain_id: String,
        recipient_auth_pub_key: String,
        amount: u64,
        rgb_consignment: Option<String>,
        funding_txid: String,
        funding_vout: u32,
        transfer_digest: String,
    }

    impl CancelConsentRequest {
        /// The ONLY constructor. Takes the decrypted mailbox message and derives every field from
        /// it — the digest included, so a caller cannot pair one transfer's amount with another
        /// transfer's binding.
        pub fn from_decrypted(pending: &PendingTransferInfo) -> Self {
            Self {
                statechain_id: pending.statechain_id.clone(),
                recipient_auth_pub_key: pending.recipient_auth_pub_key.clone(),
                amount: pending.amount,
                rgb_consignment: pending.rgb_consignment.clone(),
                funding_txid: pending.funding_txid.clone(),
                funding_vout: pending.funding_vout,
                transfer_digest: super::consent_digest_for(pending),
            }
        }

        /// The coin.
        pub fn statechain_id(&self) -> &str {
            &self.statechain_id
        }

        /// The receiving slot the message was addressed to — DERIVED, by decryption, not supplied.
        pub fn recipient_auth_pub_key(&self) -> &str {
            &self.recipient_auth_pub_key
        }

        /// Sats, branch-validated (`PendingTransferInfo::amount`): 0 if the branch fails validation,
        /// so a hostile sender cannot inflate what the recipient is shown.
        pub fn amount(&self) -> u64 {
            self.amount
        }

        /// The coin's RGB consignment envelope, if it carries a token.
        pub fn rgb_consignment(&self) -> Option<&str> {
            self.rgb_consignment.as_deref()
        }

        /// True if abandoning this transfer abandons an RGB allocation as well as sats. A recipient
        /// deciding on sats alone would be deciding on the smaller half.
        pub fn is_coloured(&self) -> bool {
            self.rgb_consignment.is_some()
        }

        pub fn funding_txid(&self) -> &str {
            &self.funding_txid
        }

        pub fn funding_vout(&self) -> u32 {
            self.funding_vout
        }

        /// Claimable material IS posted — necessarily true, since this wallet downloaded and
        /// decrypted it. Carried explicitly because it is the fact that makes consent NECESSARY (an
        /// unconveyed transfer the sender can withdraw alone), and a UI should be able to say so.
        pub fn claimable_material_posted(&self) -> bool {
            true
        }

        /// The transfer-instance digest this consent is bound to.
        pub fn transfer_digest(&self) -> &str {
            &self.transfer_digest
        }
    }
}

pub use previewed::CancelConsentRequest;

/// Show a recipient what a consent would abandon, WITHOUT signing anything.
///
/// Read-only and side-effect free; safe to call before showing a confirmation prompt. It refuses,
/// by name, on [`ConsentBlocked::NoPendingTransfer`] and [`ConsentBlocked::AlreadyClaimed`] — the
/// two reasons a wallet has no standing to consent. [`cancel_consent`] adds exactly one more,
/// [`ConsentBlocked::ReceivingSlotGone`], for a wallet that stopped holding the slot in between.
///
/// Note the parameters: a coin id, and nothing else. The recipient key is DERIVED from the message
/// this wallet could decrypt, never accepted from the sender. Better still, call
/// [`preview_all_cancellable_consents`] and let the recipient choose from ITS OWN list, so the party
/// asking for the consent does not get to name the subject at all.
pub async fn preview_cancel_consent(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
) -> Result<CancelConsentRequest> {
    let target = consent_target_for(client_config, wallet_name, statechain_id).await?;
    Ok(CancelConsentRequest::from_decrypted(&target))
}

/// EVERY transfer this wallet could consent to cancelling, described in full.
///
/// The strongest answer to the misdirection attack, because the sender names nothing: the recipient
/// enumerates its own mailbox and picks. A request that describes a small payment has to match an
/// entry here, and the entry carries the real branch-validated amount.
///
/// Transfers this wallet has already claimed are omitted — they are not cancellable, and asking for
/// one by id yields the typed [`ConsentBlocked::AlreadyClaimed`] refusal.
pub async fn preview_all_cancellable_consents(
    client_config: &ClientConfig,
    wallet_name: &str,
) -> Result<Vec<CancelConsentRequest>> {
    let (pending, claimed) = mailbox_and_claimed(client_config, wallet_name).await?;
    Ok(pending
        .iter()
        .filter(|p| {
            select_consent_target(&p.statechain_id, &pending, &claimed)
                .is_ok_and(|t| t.recipient_auth_pub_key == p.recipient_auth_pub_key)
        })
        .map(CancelConsentRequest::from_decrypted)
        .collect())
}

/// This wallet's decrypted mailbox, and the `(statechain_id, auth_pubkey)` of every coin it holds.
///
/// A transport failure reaching the mailbox is PROPAGATED, never folded into "no pending transfer":
/// a coordinator we could not reach has told us nothing, and reporting that as "you are not the
/// recipient" would talk an honest recipient out of a cancellation they should agree to.
async fn mailbox_and_claimed(
    client_config: &ClientConfig,
    wallet_name: &str,
) -> Result<(Vec<PendingTransferInfo>, Vec<(String, String)>)> {
    let wallet = get_wallet(&client_config.pool, wallet_name).await?;
    let pending =
        crate::transfer_receiver::peek_pending_transfers(client_config, wallet_name).await?;
    let claimed: Vec<(String, String)> = wallet
        .coins
        .iter()
        .filter_map(|c| c.statechain_id.clone().map(|sid| (sid, c.auth_pubkey.clone())))
        .collect();
    Ok((pending, claimed))
}

/// Locate the transfer a consent would be for.
async fn consent_target_for(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
) -> Result<PendingTransferInfo> {
    let (pending, claimed) = mailbox_and_claimed(client_config, wallet_name).await?;
    Ok(select_consent_target(statechain_id, &pending, &claimed)
        .map_err(anyhow::Error::new)?
        .clone())
}

/// The coin whose auth key signs an APPROVED consent, or a typed refusal.
///
/// Pure, so the refusal is testable without a coordinator. The key came from decrypting the message,
/// so the wallet normally holds its private half — resolve it explicitly rather than assume, and
/// refuse by name rather than fall back to any other key: signing with the wrong key is not consent,
/// it is a signature the coordinator rejects after the recipient has already given it.
fn signing_slot_for<'a>(
    approved: &CancelConsentRequest,
    coins: &'a [Coin],
) -> std::result::Result<&'a Coin, ConsentUnavailable> {
    coins
        .iter()
        .find(|c| c.auth_pubkey == approved.recipient_auth_pub_key())
        .ok_or_else(|| ConsentUnavailable {
            statechain_id: approved.statechain_id().to_string(),
            reason: ConsentBlocked::ReceivingSlotGone,
        })
}

/// Recipient half of a COOPERATIVE cancellation: mint the single-use, transfer-bound consent token.
///
/// The recipient — normally a different wallet on a different device — signs
/// `sha256(nonce|"transfer/cancel/recipient|<digest>")` with the auth key of its receiving slot and
/// hands the resulting [`ConsentToken`] back to the sender out of band.
///
/// # It signs an OBJECT, not two identifiers
///
/// The only parameter that says WHAT is being abandoned is a [`CancelConsentRequest`], and the only
/// way to obtain one is [`preview_cancel_consent`] or [`preview_all_cancellable_consents`]. Neither
/// the coin id nor the recipient key can be supplied here, so a counterparty has nothing to aim.
/// That is deliberate and it is the shape `mod gated` uses in `combine.rs`: the argument is evidence
/// that a check ran, and its fields are private so it cannot be assembled to say something the check
/// never established.
///
/// It also means **what was previewed is what is signed**. The digest comes out of the approved
/// object; this function never looks at the mailbox again. Two independent peeks would let the row
/// move between the human's decision and the signature — the sender re-addresses, the second peek
/// sees the replacement, and the recipient signs material it was never shown. If the row HAS moved,
/// the coordinator refuses the now-superseded consent as
/// [`CancelDecision::RecipientConsentStale`](mercurylib::transfer::cancel::CancelDecision::RecipientConsentStale),
/// which is the correct outcome: a refusal, not a signature over the unexpected.
///
/// # What it refuses to sign
///
/// The preview refuses, by name, when no transfer of that coin is addressed to any key this wallet
/// holds ([`ConsentBlocked::NoPendingTransfer`]) or this wallet already claimed it
/// ([`ConsentBlocked::AlreadyClaimed`]); this call adds [`ConsentBlocked::ReceivingSlotGone`] for a
/// wallet that no longer holds the previewed slot. A consent primitive that signs an opaque nonce is
/// a phishing tool — show the human the previewed amount first.
///
/// # What the token authorizes
///
/// EXACTLY ONE cancellation of EXACTLY THIS transfer. The nonce is single-use, and the digest binds
/// the signature to the mailbox ciphertext currently in the recipient's hands. Both halves are load
/// bearing: without the digest a sender could take the consent, re-address the coin to the SAME
/// recipient key (which the coordinator permits, minting a fresh `x1` and destroying the old row),
/// let the recipient see the replacement in its mailbox and believe it is live, then spend the
/// consent against the replacement.
pub async fn cancel_consent(
    client_config: &ClientConfig,
    wallet_name: &str,
    approved: &CancelConsentRequest,
) -> Result<String> {
    let wallet = get_wallet(&client_config.pool, wallet_name).await?;
    let recipient_coin = signing_slot_for(approved, &wallet.coins).map_err(anyhow::Error::new)?;

    let nonce_sig = nonce_auth_token(
        client_config,
        approved.statechain_id(),
        &mercurylib::transfer::cancel::recipient_consent_endpoint(approved.transfer_digest()),
        recipient_coin,
    )
    .await?;

    Ok(ConsentToken {
        nonce_sig,
        transfer_digest: approved.transfer_digest().to_string(),
    }
    .encode())
}

/// Sender half of a COOPERATIVE cancellation, carrying a [`ConsentToken`] obtained out of band from
/// the recipient's [`cancel_consent`].
///
/// This is what makes row 2 of the authorization table reachable ACROSS wallets. It fetches its own
/// fresh sender nonce — the two legs are bound to different endpoint strings, so neither can stand in
/// for the other, and the sender leg burned by a previous attempt is never reused.
///
/// # Why this is not a `consent: Option<..>` parameter on [`cancel`]
///
/// Different clocks and different meanings. [`cancel`] is a DISCOVERY call: it attempts sender-alone
/// and, on refusal, returns [`CancelNeedsRecipientConsent`] carrying the key to route the request
/// to — an errand with no deadline. This one is a one-shot against a token whose nonce dies in
/// minutes. And `Err(CancelNeedsRecipientConsent)` from [`cancel`] is the START of the cooperative
/// flow, whereas from here it would mean the token you were handed did not work; one function
/// returning one error type for those two situations would be unreadable.
pub async fn cancel_with_consent(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    recipient_auth_pub_key: &str,
    consent_token: &str,
) -> Result<CancelOutcome> {
    let token = ConsentToken::parse(consent_token)?;
    cancel_with_consent_inner(
        client_config,
        wallet_name,
        statechain_id,
        recipient_auth_pub_key,
        Some(token),
    )
    .await
}

/// The coin this wallet is cancelling AS THE SENDER. Index 0 only: duplicates are deposits into the
/// same address, not transfer legs, and their auth keys do not authorize the transfer row.
fn sender_coin_for_cancel(wallet: &Wallet, wallet_name: &str, statechain_id: &str) -> Result<Coin> {
    wallet
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("no coin with statechain id {statechain_id} in wallet {wallet_name}"))
}

/// One `POST /transfer/cancel` round trip, read into a [`CancelReply`].
///
/// `recipient` is `Some((key, sig))` for the cooperative leg. A transport or parse failure is
/// PROPAGATED, never folded into "nothing to cancel": a coordinator this client could not reach has
/// not released the lock, and reporting success would leave the sender believing a still-locked coin
/// is spendable.
async fn post_cancel(
    client_config: &ClientConfig,
    statechain_id: &str,
    sender_coin: &Coin,
    recipient: Option<(String, ConsentToken)>,
) -> Result<CancelReply> {
    let client = client_config.get_reqwest_client()?;
    let url = format!("{}/transfer/cancel", client_config.statechain_entity);

    let auth_sig = nonce_auth_token(
        client_config,
        statechain_id,
        mercurylib::transfer::cancel::CANCEL_SENDER_ENDPOINT,
        sender_coin,
    )
    .await?;

    // The digest travels as its own field: the coordinator RECOMPUTES it from the row and refuses a
    // mismatch, so this only states which transfer instance the signature claims to be for.
    let (recipient_auth_pub_key, recipient_auth_sig, recipient_transfer_digest) = match recipient {
        Some((key, token)) => {
            (Some(key), Some(token.nonce_sig), Some(token.transfer_digest))
        }
        None => (None, None, None),
    };

    let payload = mercurylib::transfer::cancel::TransferCancelRequestPayload {
        statechain_id: statechain_id.to_string(),
        auth_sig,
        recipient_auth_sig,
        recipient_auth_pub_key,
        recipient_transfer_digest,
    };

    let response = client.post(&url).json(&payload).send().await?;
    // The status is read BEFORE the body is consumed: when the body turns out not to be a decision,
    // the status is half of what the caller has left. See `read_cancel_response` for why an
    // unreadable body must degrade to the server's own words rather than to a parse error.
    let http_status = response.status().as_u16();
    let text = response.text().await?;

    Ok(read_cancel_response(statechain_id, http_status, &text))
}

/// Turn a settled reply into the caller's result, applying the local bookkeeping on success.
async fn finish_cancel(
    client_config: &ClientConfig,
    wallet_name: &str,
    sender_coin: &Coin,
    reply: CancelReply,
) -> Result<CancelOutcome> {
    let outcome = reply.into_result()?;
    let statechain_id = sender_coin
        .statechain_id
        .as_deref()
        .ok_or_else(|| anyhow!("cancelled a coin with no statechain id"))?;

    // The coin is ours again. Put its status back so it is selectable for a new spend and so
    // `defend_ladders` treats it as a live coin rather than one mid-conveyance.
    if let Some(restored) = status_after_cancel(&sender_coin.status) {
        persist_coin_status(client_config, wallet_name, statechain_id, restored).await?;
    }

    // ---- [CANCEL] RECONCILE THE LADDER. -------------------------------------------------------
    //
    // The coordinator has released the pending-transfer lock, but the receiver-paying state `S'`
    // this wallet co-signed is still out there — the recipient may well hold it, which is the entire
    // reason a POSTED transfer needs their consent to cancel. Cancelling does not un-sign it, and
    // the enclave's `sig_count` cannot be decremented (every write to it, in the lockbox and in the
    // SGX twin, is `+ 1`). So the orphan is DISCLOSED instead: `reclaim_cancelled_conveyance`
    // co-signs one replacement state strictly below it and demotes it into the bundle's
    // `superseded_states`, where the next receiver's census counts it and the verifier proves it
    // non-confirmable. Without this the coin could never be claimed by anyone again, and a
    // replacement recipient would TIE with the cancelled one over the same outpoint.
    //
    // ORDER: after the status restore, and after the coordinator applied the cancellation — while a
    // transfer is open the SE refuses every co-signature of the coin.
    //
    // A FAILURE HERE IS REPORTED, NOT SWALLOWED, and it says the cancellation itself succeeded: the
    // lock is released either way, and the difference is whether the coin can be transferred onward.
    crate::tesr::reclaim_cancelled_conveyance(client_config, wallet_name, sender_coin)
        .await
        .map_err(|e| {
            anyhow!(
                "the transfer of statechain id {statechain_id} WAS cancelled and the coin is yours \
                 again, but its exit ladder could not be reconciled afterwards ({e}). The \
                 receiver-paying state co-signed for the cancelled recipient is still un-disclosed, \
                 so this coin cannot be transferred onward until the ladder is repaired — the \
                 receiver would refuse it on the signature census. It remains fully withdrawable \
                 and unilaterally exitable. Retry the cancellation (the coordinator reports it as \
                 already cancelled and this step runs again), or re-anchor the coin on-chain with \
                 refresh."
            )
        })?;

    // ---- [#133] AND THE SAME FOR A CANCELLED **CHILD** CONVEYANCE. ----------------------------
    //
    // The call above handles `tesr-` rows and returns `Ok(false)` for a leaf, because a leaf's ladder
    // lives under `ctesr-`. That is the whole of the gap: a cancelled child conveyance left the
    // wallet's own row naming the payee, permanently. `leaf_exit_pays_this_wallet` in the tower
    // (`rust-sdk/src/wallet.rs`) already refuses to DRIVE such a row — which stops the theft but
    // leaves the leaf undefended, so the refusal was a holding action, not the fix. This is the fix.
    //
    // Both are called unconditionally rather than switched on the coin's shape: each is a no-op when
    // its lane's record is absent, and asking "is this a child?" here would be a third place that has
    // to know, which is one more than can be kept right.
    crate::tesr::reclaim_cancelled_child_conveyance(client_config, wallet_name, sender_coin)
        .await
        .map_err(|e| {
            anyhow!(
                "the transfer of statechain id {statechain_id} WAS cancelled and the coin is yours \
                 again, but it is a SPLIT CHILD whose ladder could not be re-pointed at this wallet \
                 afterwards ({e}). Its stored exit still pays the cancelled recipient, so this \
                 wallet's watchtower will refuse to drive it rather than hand them the coin — which \
                 means the leaf is currently undefended against its parent-anchored deadline. Retry \
                 the cancellation (the coordinator reports it as already cancelled and this step \
                 runs again). If the leaf is near that deadline and the reclaim cannot be made to \
                 succeed, exit it."
            )
        })?;

    if let (Some(txid), Some(vout)) = (sender_coin.utxo_txid.clone(), sender_coin.utxo_vout) {
        let mut wallet = get_wallet(&client_config.pool, wallet_name).await?;
        wallet.activities.push(Activity {
            utxo: format!("{}:{}", txid, vout),
            amount: sender_coin.amount.unwrap_or(0),
            action: "TransferCancel".to_string(),
            date: Utc::now().to_rfc3339(),
        });
        update_wallet(&client_config.pool, &wallet).await?;
    }

    Ok(outcome)
}

/// The cooperative leg. `supplied` is the consent token when it came from another wallet; `None`
/// means mint it here, which only works when this wallet holds the recipient key.
async fn cancel_with_consent_inner(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    recipient_auth_pub_key: &str,
    supplied: Option<ConsentToken>,
) -> Result<CancelOutcome> {
    let wallet = get_wallet(&client_config.pool, wallet_name).await?;
    let sender_coin = sender_coin_for_cancel(&wallet, wallet_name, statechain_id)?;

    let token = match supplied {
        Some(token) => token,
        // Self-addressed: this wallet is both parties, so it mints its own consent through exactly
        // the same recipient-side path — including the preview's refusals. `recipient_auth_pub_key`
        // here came from the COORDINATOR's authenticated response, not from a counterparty, which is
        // why it is safe to cross-check against rather than to sign with.
        None => {
            let approved =
                preview_cancel_consent(client_config, wallet_name, statechain_id).await?;
            if approved.recipient_auth_pub_key() != recipient_auth_pub_key {
                return Err(anyhow!(
                    "the coordinator records {recipient_auth_pub_key} as the recipient of \
                     statechain id {statechain_id}, but the message this wallet decrypted is \
                     addressed to {}; refusing to submit a consent for a transfer this wallet \
                     cannot identify",
                    approved.recipient_auth_pub_key()
                ));
            }
            ConsentToken::parse(&cancel_consent(client_config, wallet_name, &approved).await?)?
        }
    };

    let reply = post_cancel(
        client_config,
        statechain_id,
        &sender_coin,
        Some((recipient_auth_pub_key.to_string(), token)),
    )
    .await?;

    finish_cancel(client_config, wallet_name, &sender_coin, reply).await
}

#[cfg(test)]
mod flat_lane_tests {
    use super::*;

    /// [B1] The set of reasons that LICENSE a flat conveyance is exactly the structurally-permanent
    /// ones. A transient reason is a deferral; if one ever creeps back onto this list, a single
    /// blip in a token wallet licenses that coin to convey flat forever after.
    #[test]
    fn only_permanent_reasons_license_the_flat_lane() {
        for permanent in [FLAT_TERMINALIZED_CARRIER, FLAT_NOT_BINDABLE] {
            assert!(
                is_legitimate_flat_reason(permanent),
                "'{permanent}' is a structural, permanent reason and must keep licensing the flat lane"
            );
            assert!(
                !is_transient_flat_reason(permanent),
                "'{permanent}' must not be classified as transient"
            );
        }
        for transient in [
            FLAT_RGB_STATE_UNAVAILABLE,
            FLAT_COORDINATOR_UNAVAILABLE,
            FLAT_ESTABLISH_FAILED,
            FLAT_FUNDING_UNRESOLVABLE,
        ] {
            assert!(
                !is_legitimate_flat_reason(transient),
                "[B1] '{transient}' is TRANSIENT — it must never license a flat conveyance"
            );
            assert!(is_transient_flat_reason(transient));
        }
        // **[ONE COIN SHAPE] RETIRED — these two used to license, and must not any more.**
        // `rgb-carrier` licensed a carrier travelling without a ladder; with `colored_ladder`
        // shipping true a carrier IS laddered, so the reason no longer describes a legitimate shape.
        // `funding-not-onchain` licensed the un-laddered split sub-coin, whose producer is retired
        // with it. Both probes are DELETED from the classifier, not merely dropped here — this
        // predicate never gated anything (it drives `flat_only_coins`' `transferable` flag), so a
        // change here alone would have been cosmetic. Asserting them in this group is what stops
        // either creeping back as a licence.
        for retired in [FLAT_RGB_CARRIER, FLAT_FUNDING_NOT_ONCHAIN] {
            assert!(
                !is_legitimate_flat_reason(retired),
                "'{retired}' was RETIRED with the one-coin-shape flip and must never license again"
            );
        }
        // Neither a licence nor a deferral: these are refusals with their own remedies.
        for other in [
            FLAT_DUPLICATE_DEPOSIT,
            FLAT_LADDER_UNREADABLE,
            FLAT_BINDING_UNRESOLVED,
            FLAT_ATTESTATION_UNPINNED,
            FLAT_ATTESTATION_INVALID,
            "some-future-spelling",
            "",
        ] {
            assert!(
                !is_legitimate_flat_reason(other),
                "'{other}' must not license a flat conveyance"
            );
        }
    }

    /// A pin that is PRESENT but WRONG must not be reported as a coordinator outage.
    ///
    /// The classifier used to decide between "unpinned" and "coordinator-unavailable" by asking
    /// whether a pin RESOLVES — presence, not validity. So a wrong pin (the wrong network's key, a
    /// pasted `/attestation_identity` JSON body instead of the bare x-only key, a redeployed
    /// enclave) was recorded as `coordinator-unavailable`, i.e. "retry later", while the coordinator
    /// was answering 200 and the fault was local and permanent. Measured on the live stack: the
    /// three pin states produce three DIFFERENT reasons, and only the middle one is transient.
    ///
    /// This test pins the classification of the spelling, which is what the conveyance path and the
    /// operator both read. If `attestation-invalid` is ever folded back into either neighbour, the
    /// distinction this records is lost silently.
    #[test]
    fn a_wrong_pin_is_permanent_and_is_not_a_coordinator_outage() {
        assert_ne!(
            FLAT_ATTESTATION_INVALID, FLAT_COORDINATOR_UNAVAILABLE,
            "a wrong pin is a local, permanent fault — it must not wear the 'retry later' spelling"
        );
        assert_ne!(
            FLAT_ATTESTATION_INVALID, FLAT_ATTESTATION_UNPINNED,
            "'no pin' and 'wrong pin' need different remedies: set one, versus fix the one you set"
        );
        assert!(
            !is_transient_flat_reason(FLAT_ATTESTATION_INVALID),
            "retrying does not make a wrong pin verify"
        );
        assert!(
            !is_legitimate_flat_reason(FLAT_ATTESTATION_INVALID),
            "an attestation that does NOT verify is precisely the case D69 exists to refuse; it \
             must never license a flat conveyance"
        );
    }

    /// Duplicates are keyed apart so a duplicate's record can never overwrite (and thereby excuse)
    /// the index-0 coin's — the index-0 coin is the one that owns the sid's ladder.
    #[test]
    fn duplicate_records_are_keyed_apart() {
        assert_eq!(ladder_skip_key("abc", 0), "ladderskip-abc");
        assert_eq!(ladder_skip_key("abc", 1), "ladderskip-abc#1");
        assert_ne!(ladder_skip_key("abc", 0), ladder_skip_key("abc", 1));
    }

    /// **THE STRUCTURAL INVARIANT, asserted on the source itself.**
    ///
    /// Three review rounds each found a NEW fail-open in the flat-lane classifier because it was
    /// written as "return `Ok(())` unless something looks wrong" — under that shape every arm is a
    /// candidate hole and patching arms one at a time does not converge. The fix was to invert the
    /// polarity so the function has exactly ONE `Ok(())`, reached only by a positive match on a
    /// proven [`PermanentLicence`]. That property is verifiable by COUNTING, so this test counts it:
    /// any future edit that adds a second success return fails here, loudly, with no live stack.
    #[test]
    fn the_classifier_has_exactly_one_ok_return() {
        const SIGNATURE: &str = "pub async fn assert_flat_conveyance_is_legitimate(";
        let src = include_str!("transfer_sender.rs");
        let start = src.find(SIGNATURE).expect("the classifier must exist");
        let rest = &src[start..];
        // Every brace inside the function is indented, so the first column-0 `}` ends it.
        let body = &rest[..rest.find("\n}\n").expect("the classifier must be terminated")];
        let count = body.matches("Ok(())").count();
        assert_eq!(
            count, 1,
            "assert_flat_conveyance_is_legitimate must have EXACTLY ONE `Ok(())` (found {count}). \
             Every success path has to go through the single `Some(licence) => Ok(())` arm, so that \
             a reader can verify 'nothing but a proven permanent licence conveys flat' by counting. \
             If you need a new success case, add a PermanentLicence variant with its own \
             positive-evidence probe — do not add a return.\n---\n{body}\n---"
        );
        // ...and that one arm is the licence match, not some other shape.
        assert!(
            body.contains("Some(_) => Ok(())"),
            "the single success return must be the licence match arm"
        );
    }

    /// The wallet-scope gate is armed by POSITIVE evidence of SDK laddering, not by the presence of
    /// one best-effort marker row. A pass that got far enough to ladder or classify ANYTHING leaves
    /// an artefact behind, and any one of them arms the classifier — so a failed `ladder-managed`
    /// insert can no longer act as a global off-switch.
    #[test]
    fn a_missing_scope_marker_is_not_a_global_off_switch() {
        let rows = |pairs: &[(&str, &str)]| {
            WalletRows(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
        };
        // A genuinely pre-SDK wallet: ordinary coin rows only, no ladder artefact anywhere.
        assert!(wallet_is_provably_pre_sdk(&rows(&[("some-sid", "[]"), ("branch-some-sid", "[]")])));
        assert!(wallet_is_provably_pre_sdk(&rows(&[])));
        // Each artefact on its own arms the gate, marker row or not.
        for artefact in [
            (LADDER_MANAGED_KEY, "1"),
            ("tesr-abc", "{}"),
            ("ctesr-abc", "{}"),
            // [CATS/V4] A spine tip is a ladder artefact. Without this the one wallet shape that
            // holds ONLY a tip — a sender whose single CATS payment left it holding the change —
            // would read as "provably never laddered" and every coin in it would be licensed flat.
            ("spinetip-abc", "{}"),
            ("ladderskip-abc", "{}"),
            ("ladderskip-abc#1", "{}"),
        ] {
            assert!(
                !wallet_is_provably_pre_sdk(&rows(&[("some-sid", "[]"), artefact])),
                "'{}' is evidence the SDK ladder pass has run — the classifier must stay armed",
                artefact.0
            );
        }
    }

    /// An unreadable record is an ERROR, never "nothing recorded" — otherwise corrupting one row
    /// would drop a coin into the no-record fallback and re-open the classifier.
    #[test]
    fn an_unreadable_skip_record_is_an_error_not_an_absence() {
        let rows = WalletRows(vec![("ladderskip-abc".into(), "{\"not\":\"a reason\"}".into())]);
        let e = recorded_flat_reason(&rows, "abc").expect_err("an unreadable record must refuse");
        assert!(e.to_string().contains("flat-lane record is unreadable"), "got: {e}");
        assert!(recorded_flat_reason(&WalletRows(vec![]), "abc").unwrap().is_none());
        let rows = WalletRows(vec![("ladderskip-abc".into(), "{\"reason\":\"rgb-carrier\"}".into())]);
        assert_eq!(recorded_flat_reason(&rows, "abc").unwrap().as_deref(), Some(FLAT_RGB_CARRIER));
    }

    /// A terminalized carrier must be POSITIVELY proven from the coin, never assumed from a record —
    /// `single_use` is dead in production today (its only setters have no non-test callers).
    #[test]
    fn terminalized_carrier_is_proven_from_the_coin_not_a_record() {
        let rows = WalletRows(vec![(
            "ladderskip-abc".into(),
            format!("{{\"reason\":\"{FLAT_TERMINALIZED_CARRIER}\"}}"),
        )]);
        // The record alone licenses NOTHING — and since the RGB-carrier probe was RETIRED with the
        // one-coin-shape flip, there is no probe left that would read a spelling out of a row at
        // all. The licence comes from the coin's own flag, which no test row can fake.
        assert_eq!(recorded_flat_reason(&rows, "abc").unwrap().as_deref(), Some(FLAT_TERMINALIZED_CARRIER));
    }
}

#[cfg(test)]
mod transfer_cancel_client_tests {
    use super::*;
    use mercurylib::transfer::cancel::{CancelDecision, TransferCancelResponsePayload};

    fn body(decision: CancelDecision, key: Option<&str>) -> TransferCancelResponsePayload {
        TransferCancelResponsePayload {
            code: decision.code().to_string(),
            message: decision.message().to_string(),
            recipient_auth_pub_key: key.map(|k| k.to_string()),
        }
    }

    /// The production half of this file — everything before the first `#[cfg(test)]`.
    fn production_source() -> &'static str {
        let src = include_str!("transfer_sender.rs");
        match src.find("\n#[cfg(test)]") {
            Some(at) => &src[..at + 1],
            None => src,
        }
    }

    // ==========================================================================================
    // AN ERROR RESPONSE THIS CLIENT CANNOT PARSE MUST STILL CARRY THE SERVER'S WORDS.
    //
    // `POST /transfer/cancel` answers a DECISION with `{code, message, recipient_auth_pub_key}`.
    // Everything that happens BEFORE the decision — a signature that did not verify, a DB fault —
    // is answered with the coordinator's generic `{"message": "..."}`, which has NO `code` field.
    // Deserialising every body as a decision therefore converts a well-formed, meaningful refusal
    // into "could not read the cancel response (missing field `code`)", and the server's actual
    // sentence never reaches the caller at all.
    //
    // That is the silent-degradation shape this repo has a CI guard for, wearing a disguise: the
    // failure reads as a CLIENT parsing bug, so whoever sees it goes looking in the wrong half of
    // the system and the coordinator's real reason — which is the only thing that says what to do
    // next — is discarded on the way past. The rule is therefore absolute and independent of any
    // server-side change: a body this client cannot read degrades to the server's `message` and
    // the HTTP status, NEVER to a parse error, and never to a success.
    // ==========================================================================================

    /// `post_cancel` must route the response through the degrading reader.
    ///
    /// Asserted on the source because `post_cancel` is I/O — it needs a live coordinator, which this
    /// crate's test suite does not have. The BEHAVIOUR of the reader is pinned properly, by calling
    /// it, in the tests below; this one pins only that the network path actually uses it rather than
    /// keeping a second, stricter parse of its own.
    #[test]
    fn post_cancel_reads_the_response_through_the_degrading_reader() {
        let src = production_source();
        let at = src.find("async fn post_cancel(").expect("`post_cancel` must exist");
        let rest = &src[at..];
        let body = &rest[..rest.find("\n}\n").expect("`post_cancel` must be terminated")];
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains("read_cancel_response("),
            "`post_cancel` must hand the HTTP STATUS and the RAW BODY to `read_cancel_response`, \
             which degrades an unreadable body to the server's own message. Parsing the body as a \
             decision and propagating the serde error discards the coordinator's refusal.\n---\n{code}\n---"
        );
        assert!(
            !code.contains("could not read the cancel response"),
            "a body this client cannot parse must not become a parse error: the server's `message` \
             and status are the answer, and this string is what hid them.\n---\n{code}\n---"
        );
        assert!(
            code.contains("response.status()"),
            "the HTTP STATUS must be captured before the body is consumed — it is half of what a \
             caller has left when the body does not parse.\n---\n{code}\n---"
        );
    }

    /// THE DEFECT, stated as behaviour. This is the exact body the coordinator's generic auth
    /// refusal produces, and the exact status it produces it with.
    #[test]
    fn a_body_with_only_a_message_surfaces_the_servers_message_and_status() {
        let reply = read_cancel_response(
            "sid-1",
            403,
            r#"{"message":"Signature does not match authentication key."}"#,
        );

        let refusal = match reply {
            CancelReply::Refused(r) => r,
            other => panic!("a server refusal must be a refusal, got {other:?}"),
        };
        assert_eq!(
            refusal.message, "Signature does not match authentication key.",
            "the server's own sentence is the answer — it must arrive verbatim, not be replaced by \
             a serde error about a missing field"
        );
        assert_eq!(refusal.code, "http_403", "the HTTP status must survive");
        assert_eq!(refusal.decision, None, "no rule was named, so none may be claimed");
        assert!(
            refusal.to_string().contains("Signature does not match authentication key."),
            "and it must still be there after the error is formatted: {refusal}"
        );
    }

    /// Every OTHER pre-decision answer this endpoint can give has the same shape, so pin them all —
    /// each one is a sentence that tells the caller what to do next, and each one used to be lost.
    #[test]
    fn every_pre_decision_refusal_keeps_its_words() {
        for (status, body, expected) in [
            (
                503u16,
                r#"{"message":"transfer-state lookup unavailable; refusing to cancel (fail-closed)"}"#,
                "transfer-state lookup unavailable; refusing to cancel (fail-closed)",
            ),
            (
                503,
                r#"{"message":"could not record the cancellation; the transfer is unchanged"}"#,
                "could not record the cancellation; the transfer is unchanged",
            ),
            // The two-field shape a few older endpoints use.
            (
                500,
                r#"{"error":"Internal Server Error","message":"Signature does not match authentication key."}"#,
                "Signature does not match authentication key.",
            ),
            // `error` alone still beats falling back to the raw body.
            (500, r#"{"error":"Internal Server Error"}"#, "Internal Server Error"),
        ] {
            match read_cancel_response("sid-1", status, body) {
                CancelReply::Refused(r) => {
                    assert_eq!(r.message, expected, "body {body} lost its message");
                    assert_eq!(r.code, format!("http_{status}"));
                }
                other => panic!("{body} must be a refusal, got {other:?}"),
            }
        }
    }

    /// A body that is not JSON at all — a proxy's error page, a truncated response — still has to
    /// produce something a human can act on, and must still be a REFUSAL.
    #[test]
    fn a_body_that_is_not_the_coordinators_json_still_refuses_legibly() {
        for (status, body) in [
            (502u16, "<html><body>502 Bad Gateway</body></html>"),
            (504, ""),
            (200, "{}"),
            (200, r#"{"code":42}"#), // right field name, wrong type
        ] {
            match read_cancel_response("sid-1", status, body) {
                CancelReply::Refused(r) => {
                    assert_eq!(r.code, format!("http_{status}"));
                    assert!(!r.message.is_empty(), "a refusal must say something");
                    assert!(
                        !r.message.contains("missing field"),
                        "a serde complaint is not an answer: {}",
                        r.message
                    );
                }
                other => panic!("({status}, {body:?}) must be a refusal, got {other:?}"),
            }
        }
    }

    /// **A 200 IS NOT A SUCCESS IF THE BODY DOES NOT SAY SO.** This is the direction the fallback
    /// must never fail in: the pending-transfer lock is the only thing standing between a conveyed
    /// recipient and a sender who co-signs a rival state, so an answer this client cannot read
    /// resolves as "still held", exactly like an unknown decision code does.
    #[test]
    fn an_unreadable_success_status_is_still_not_a_cancellation() {
        for status in [200u16, 201, 204] {
            let reply = read_cancel_response("sid-1", status, "not json at all");
            assert!(
                matches!(reply, CancelReply::Refused(_)),
                "HTTP {status} with an unreadable body must NOT be read as a cancellation: {reply:?}"
            );
            assert!(reply.into_result().is_err());
        }
    }

    /// A well-formed decision is untouched by the fallback — it still goes through
    /// `read_cancel_reply`, whatever the HTTP status says. The status is a HINT; the decision code
    /// is the authority, and the two must not be able to disagree into a success.
    #[test]
    fn a_real_decision_is_read_as_a_decision_whatever_the_status_says() {
        let text = serde_json::to_string(&body(CancelDecision::AlreadyClaimed, None)).unwrap();
        for status in [410u16, 200, 500] {
            match read_cancel_response("sid-1", status, &text) {
                CancelReply::Refused(r) => {
                    assert_eq!(r.decision, Some(CancelDecision::AlreadyClaimed));
                    assert_eq!(r.code, CancelDecision::AlreadyClaimed.code());
                }
                other => panic!("a decision body must be read as its decision, got {other:?}"),
            }
        }
        // And a genuine `cancelled` decision is still a success.
        let ok = serde_json::to_string(&body(CancelDecision::Allow, None)).unwrap();
        assert!(matches!(
            read_cancel_response("sid-1", 200, &ok),
            CancelReply::Done(CancelOutcome::Cancelled)
        ));
    }

    /// The fallback code can never be mistaken for a rule that fired.
    #[test]
    fn the_http_fallback_code_cannot_collide_with_a_decision_code() {
        for d in [
            CancelDecision::Allow,
            CancelDecision::AlreadyCancelled,
            CancelDecision::NoSuchTransfer,
            CancelDecision::AlreadyClaimed,
            CancelDecision::Batched,
            CancelDecision::ClaimInFlight,
            CancelDecision::RecipientConsentRequired,
            CancelDecision::RecipientSignatureInvalid,
            CancelDecision::RecipientConsentStale,
        ] {
            assert!(
                !d.code().starts_with("http_"),
                "{} would be indistinguishable from the HTTP fallback",
                d.code()
            );
        }
    }

    /// EXHAUSTIVE. Every `CancelDecision` the coordinator can answer with maps to exactly one
    /// client reading, and the two successes are the ONLY readings that are not an error. A
    /// catch-all that fell through to `Cancelled` would tell a sender its coin is free while the
    /// coordinator still holds the lock — and the sender would then try to spend a coin the SE
    /// refuses, or worse, believe a conveyed payment was withdrawn when it was not.
    #[test]
    fn every_cancel_decision_maps_to_exactly_one_client_reading() {
        for decision in [
            CancelDecision::Allow,
            CancelDecision::AlreadyCancelled,
            CancelDecision::NoSuchTransfer,
            CancelDecision::AlreadyClaimed,
            CancelDecision::Batched,
            CancelDecision::ClaimInFlight,
            CancelDecision::RecipientConsentRequired,
            CancelDecision::RecipientSignatureInvalid,
        ] {
            let key = matches!(decision, CancelDecision::RecipientConsentRequired)
                .then_some("02deadbeef");
            let reply = read_cancel_reply("sid-1", &body(decision, key));
            match (decision, &reply) {
                (CancelDecision::Allow, CancelReply::Done(CancelOutcome::Cancelled)) => {}
                (
                    CancelDecision::AlreadyCancelled,
                    CancelReply::Done(CancelOutcome::AlreadyCancelled),
                ) => {}
                (CancelDecision::RecipientConsentRequired, CancelReply::NeedsRecipientConsent(c)) => {
                    assert_eq!(c.recipient_auth_pub_key.as_deref(), Some("02deadbeef"));
                }
                (d, CancelReply::Refused(r)) => {
                    assert_eq!(r.decision, Some(d), "the refusal must name the rule that fired");
                    assert_eq!(r.code, d.code());
                }
                (d, other) => panic!("{d:?} was read as {other:?}"),
            }
        }
    }

    /// A code this client does not know is a REFUSAL, never a success. This is the
    /// silent-degradation shape for this endpoint: a future coordinator that grows a ninth decision
    /// must not have it read as "cancelled" by an old client, because the sender would then treat a
    /// still-locked coin as free.
    #[test]
    fn an_unknown_code_is_refused_not_treated_as_success() {
        let reply = read_cancel_reply(
            "sid-1",
            &TransferCancelResponsePayload {
                code: "some_future_decision".to_string(),
                message: "a rule this client has never heard of".to_string(),
                recipient_auth_pub_key: None,
            },
        );
        match reply {
            CancelReply::Refused(ref r) => {
                assert_eq!(r.decision, None, "an unknown code cannot name a known rule");
                assert_eq!(r.code, "some_future_decision");
            }
            ref other => panic!("an unknown code must be refused, got {other:?}"),
        }
        assert!(reply.into_result().is_err());
    }

    /// A CLAIMED transfer is never cancellable, and the refusal says so by name rather than as an
    /// opaque string the caller has to parse.
    #[test]
    fn a_claimed_transfer_is_refused_by_name() {
        let err = read_cancel_reply("sid-1", &body(CancelDecision::AlreadyClaimed, None))
            .into_result()
            .unwrap_err();
        let refusal = err
            .downcast_ref::<CancelRefused>()
            .expect("a refusal must survive as a typed error, not a formatted string");
        assert_eq!(refusal.decision, Some(CancelDecision::AlreadyClaimed));
        assert!(refusal.to_string().contains("already claimed"), "{refusal}");
    }

    /// An LN-BATCHED transfer is never cancellable: the latch's preimage decides whether the
    /// payment happened, and recipient consent is not the right authorization for it.
    #[test]
    fn an_ln_batched_transfer_is_refused_by_name() {
        let err = read_cancel_reply("sid-1", &body(CancelDecision::Batched, None))
            .into_result()
            .unwrap_err();
        let refusal = err.downcast_ref::<CancelRefused>().expect("typed refusal");
        assert_eq!(refusal.decision, Some(CancelDecision::Batched));
        assert!(refusal.to_string().contains("lightning-latch"), "{refusal}");
    }

    /// The recipient-consent refusal carries the key PROGRAMMATICALLY, so a cross-wallet caller can
    /// route it to the recipient instead of scraping prose — and it still contains the server's
    /// verbatim text, which the live-stack test pins.
    #[test]
    fn the_consent_refusal_is_actionable_and_keeps_the_servers_words() {
        let err = read_cancel_reply(
            "sid-1",
            &body(CancelDecision::RecipientConsentRequired, Some("02abc")),
        )
        .into_result()
        .unwrap_err();
        let needs = err
            .downcast_ref::<CancelNeedsRecipientConsent>()
            .expect("the consent refusal must be its own type");
        assert_eq!(needs.statechain_id, "sid-1");
        assert_eq!(needs.recipient_auth_pub_key.as_deref(), Some("02abc"));
        assert!(
            needs.to_string().contains("the recipient must co-sign the cancellation"),
            "the server's verbatim refusal must survive: {needs}"
        );
    }

    /// A consent-required answer with no key is still a consent refusal, not a success and not a
    /// generic one: the sender cannot act on it, but it must not be mistaken for anything else.
    #[test]
    fn consent_required_without_a_key_is_still_a_consent_refusal() {
        let reply =
            read_cancel_reply("sid-1", &body(CancelDecision::RecipientConsentRequired, None));
        match reply {
            CancelReply::NeedsRecipientConsent(ref c) => {
                assert!(c.recipient_auth_pub_key.is_none());
            }
            other => panic!("expected a consent refusal, got {other:?}"),
        }
        assert!(reply.into_result().is_err());
    }

    /// A recipient signature that did not verify is reported as INVALID, never as MISSING. Reading
    /// a wrong-key or replayed consent as "you forgot to attach one" would send an honest client
    /// into a retry loop and would hide a forgery attempt.
    #[test]
    fn an_invalid_consent_is_not_reported_as_a_missing_one() {
        let err = read_cancel_reply("sid-1", &body(CancelDecision::RecipientSignatureInvalid, None))
            .into_result()
            .unwrap_err();
        assert!(err.downcast_ref::<CancelNeedsRecipientConsent>().is_none());
        let refusal = err.downcast_ref::<CancelRefused>().expect("typed refusal");
        assert_eq!(refusal.decision, Some(CancelDecision::RecipientSignatureInvalid));
    }

    /// Only the two success readings restore the coin, and only from IN_TRANSFER. Getting this
    /// wrong is the likeliest way to make a cancelled coin unusable: leaving it IN_TRANSFER hides
    /// it from selection forever, and stamping CONFIRMED over a WITHDRAWING/INVALIDATED coin would
    /// resurrect a coin that is not the sender's to spend.
    #[test]
    fn a_successful_cancel_restores_only_an_in_transfer_coin_to_spendable() {
        assert_eq!(status_after_cancel(&CoinStatus::IN_TRANSFER), Some(CoinStatus::CONFIRMED));
        for untouched in [
            CoinStatus::CONFIRMED,
            CoinStatus::WITHDRAWING,
            CoinStatus::WITHDRAWN,
            CoinStatus::INVALIDATED,
            CoinStatus::INITIALISED,
            CoinStatus::IN_MEMPOOL,
            CoinStatus::TRANSFERRED,
            CoinStatus::DUPLICATED,
        ] {
            assert_eq!(
                status_after_cancel(&untouched),
                None,
                "{untouched:?} must be left exactly as it is"
            );
        }
    }

    /// The two consent legs are bound to DIFFERENT endpoint strings, so neither can stand in for
    /// the other and neither can be redirected at another nonce-protected endpoint.
    #[test]
    fn the_two_consent_legs_cannot_be_substituted_for_one_another() {
        assert_ne!(
            mercurylib::transfer::cancel::CANCEL_SENDER_ENDPOINT,
            mercurylib::transfer::cancel::CANCEL_RECIPIENT_ENDPOINT
        );
    }

    /// A STALE consent — one the recipient minted for a transfer the sender then re-addressed out
    /// from under it — must reach the caller as its own named rule.
    ///
    /// The distinction is not cosmetic. `RecipientConsentRequired` means "go and get a token";
    /// `RecipientSignatureInvalid` means "your recipient signed badly". A sender holding a perfectly
    /// good token for a superseded transfer would act on either of those by re-sending the same
    /// token, or by accusing an honest recipient. The truthful answer is that the transfer moved.
    #[test]
    fn a_stale_consent_is_named_apart_from_missing_and_invalid() {
        let reply =
            read_cancel_reply("sid-1", &body(CancelDecision::RecipientConsentStale, None));

        // Not the "go and get a token" reading — the sender already has one.
        assert!(
            !matches!(reply, CancelReply::NeedsRecipientConsent(_)),
            "a stale consent is not a MISSING one: {reply:?}"
        );

        let err = reply.into_result().unwrap_err();
        assert!(err.downcast_ref::<CancelNeedsRecipientConsent>().is_none());
        let refusal = err.downcast_ref::<CancelRefused>().expect("typed refusal");
        assert_eq!(
            refusal.decision,
            Some(CancelDecision::RecipientConsentStale),
            "this client must RECOGNISE the rule, not fall through to the unknown-code arm — \
             otherwise a sender cannot tell a re-addressed transfer from a coordinator it has never \
             met"
        );
        assert_ne!(refusal.decision, Some(CancelDecision::RecipientSignatureInvalid));
        assert!(
            refusal.to_string().contains("different transfer material"),
            "the server's verbatim words must survive: {refusal}"
        );
    }

    // ==========================================================================================
    // THE CONSENT TOKEN: one opaque string that carries what it is consent FOR.
    // ==========================================================================================

    const D1: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn a_consent_token_round_trips_and_carries_its_binding() {
        let token = ConsentToken { nonce_sig: "nonce-1:abcd".to_string(), transfer_digest: D1.to_string() };
        let wire = token.encode();
        let back = ConsentToken::parse(&wire).expect("a token this crate minted must parse");
        assert_eq!(back.nonce_sig, "nonce-1:abcd");
        assert_eq!(back.transfer_digest, D1);
    }

    /// The token is ONE string precisely so a human relaying it out of band cannot separate the
    /// signature from what it is a signature FOR. A token stripped back to the legacy
    /// `"<nonce>:<sig>"` shape must be REFUSED, not silently sent as an unbound consent — an unbound
    /// consent is exactly what the coordinator now treats as stale, and failing here gives the
    /// caller a comprehensible error instead of a confusing server refusal.
    #[test]
    fn a_token_that_carries_no_binding_is_refused_locally() {
        for stripped in [
            "nonce-1:abcd",                       // the legacy two-field shape
            "nonce-1",                            // nonce alone
            "",                                   // empty
            "nonce-1:abcd:",                      // present but empty digest
            "nonce-1::abcd",                      // empty signature
            ":abcd:1111",                         // empty nonce
            "nonce-1:abcd:not-a-digest",          // not 64 hex
            "nonce-1:abcd:1111111111111111111111111111111111111111111111111111111111111zz",
            "nonce-1:abcd:1111:2222",             // an extra field is not a token this client wrote
        ] {
            assert!(
                ConsentToken::parse(stripped).is_err(),
                "'{stripped}' must not parse as a bound consent token"
            );
        }
    }

    // ==========================================================================================
    // WHO MAY MINT A CONSENT — the phishing surface.
    // ==========================================================================================

    fn pending(sid: &str, key: &str, amount: u64) -> PendingTransferInfo {
        PendingTransferInfo {
            statechain_id: sid.to_string(),
            recipient_auth_pub_key: key.to_string(),
            encrypted_transfer_msg: format!("ciphertext-for-{sid}-{key}"),
            amount,
            rgb_consignment: None,
            funding_txid: "f".repeat(64),
            funding_vout: 0,
            branch_txs: vec![],
            child_witness_txids: vec![],
            ladder_census_ok: true,
            // `None` iff the census passed — the invariant the field's own doc states.
            ladder_census_refusal: None,
        }
    }

    /// A wallet that is not the recorded recipient CANNOT MINT. The refusal is local and by name:
    /// this wallet decrypted nothing addressed to it for that coin, so it has no business signing
    /// anything about it.
    ///
    /// This is the property that makes the primitive safe to expose. A sender that names some other
    /// party's coin gets a refusal here, before any key is touched — rather than a signature this
    /// wallet had no standing to give.
    #[test]
    fn a_wallet_that_is_not_the_recorded_recipient_cannot_mint() {
        let mine = [pending("sid-mine", "02aaa", 10_000)];
        let err = select_consent_target("sid-someone-elses", &mine, &[])
            .expect_err("a coin we hold no pending transfer for must not be consentable");
        assert_eq!(err.reason, ConsentBlocked::NoPendingTransfer);
        assert_eq!(err.statechain_id, "sid-someone-elses");
        assert!(err.to_string().contains("no pending transfer"), "{err}");

        // ... and an EMPTY mailbox is the same refusal, never a silent success.
        assert_eq!(
            select_consent_target("sid-mine", &[], &[]).unwrap_err().reason,
            ConsentBlocked::NoPendingTransfer
        );
    }

    /// Consenting to cancel something you have ALREADY TAKEN is a footgun: the coordinator will
    /// refuse it (`AlreadyClaimed`), but by then the recipient has already signed. Refuse locally,
    /// by name, before signing.
    #[test]
    fn an_already_claimed_transfer_refuses_locally_before_signing() {
        let mine = [pending("sid-1", "02aaa", 10_000)];
        // This wallet already booked sid-1 on that very receiving slot: the handover completed.
        let claimed = [("sid-1".to_string(), "02aaa".to_string())];

        let err = select_consent_target("sid-1", &mine, &claimed)
            .expect_err("a claimed transfer must not be consentable");
        assert_eq!(err.reason, ConsentBlocked::AlreadyClaimed);
        assert!(err.to_string().contains("already claimed"), "{err}");
    }

    /// The already-claimed check is keyed on the RECEIVING SLOT, not merely on the coin. In a
    /// self-addressed transfer one wallet holds both legs, and the SENDER's coin carries the same
    /// statechain id — matching on the coin alone would make every self-addressed cancellation
    /// report "already claimed" and break the path tb05 exercises.
    #[test]
    fn the_claimed_check_is_keyed_on_the_receiving_slot_not_the_coin() {
        let mine = [pending("sid-1", "02recipient", 10_000)];
        // The sender leg: same coin, DIFFERENT auth key, held by the same wallet.
        let claimed = [("sid-1".to_string(), "02sender".to_string())];

        let target = select_consent_target("sid-1", &mine, &claimed)
            .expect("the sender leg of a self-addressed transfer must not block the consent");
        assert_eq!(target.recipient_auth_pub_key, "02recipient");
    }

    /// The right coin is selected out of several, so a preview cannot show one coin's amount while
    /// consenting to another's.
    #[test]
    fn the_named_coin_is_the_one_selected() {
        let mine = [
            pending("sid-small", "02aaa", 10_000),
            pending("sid-big", "02bbb", 1_000_000),
        ];
        assert_eq!(select_consent_target("sid-big", &mine, &[]).unwrap().amount, 1_000_000);
        assert_eq!(select_consent_target("sid-small", &mine, &[]).unwrap().amount, 10_000);
    }

    /// The digest a recipient signs is derived from the ciphertext it DOWNLOADED, so two transfers
    /// of the same coin to the same key produce different consents. This is the client half of the
    /// instance binding; the coordinator half is `mercurylib::transfer::cancel`.
    #[test]
    fn the_consent_digest_follows_the_downloaded_ciphertext() {
        let mut t1 = pending("sid-1", "02aaa", 10_000);
        let d1 = consent_digest_for(&t1);
        // The sender re-addresses to the SAME key: fresh x1, so different ciphertext.
        t1.encrypted_transfer_msg = "a-freshly-blinded-replacement".to_string();
        let d2 = consent_digest_for(&t1);
        assert_ne!(d1, d2, "a re-addressed transfer must not inherit the old consent's digest");
        assert_eq!(d1.len(), 64);
    }

    /// A success ALWAYS restores the coin, and a refusal NEVER does.
    ///
    /// Both halves are failure modes. Not restoring after a success strands the coin: it stays
    /// IN_TRANSFER, which hides it from selection and from `defend_ladders` forever — the coin is on
    /// chain and safe, but the wallet will not spend it, and nothing surfaces the problem. Restoring
    /// after a refusal is worse: the coordinator still holds the pending-transfer lock, so the
    /// wallet would offer a coin it cannot co-sign.
    #[test]
    fn a_success_always_restores_the_coin_and_a_refusal_never_does() {
        use CancelDecision::*;
        for d in [
            Allow,
            AlreadyCancelled,
            NoSuchTransfer,
            AlreadyClaimed,
            Batched,
            ClaimInFlight,
            RecipientConsentRequired,
            RecipientSignatureInvalid,
            RecipientConsentStale,
        ] {
            let is_success = read_cancel_reply("sid-1", &body(d, None)).into_result().is_ok();
            assert_eq!(
                is_success,
                matches!(d, Allow | AlreadyCancelled),
                "{d:?} is on the wrong side of the success boundary"
            );
            if is_success {
                assert_eq!(
                    status_after_cancel(&CoinStatus::IN_TRANSFER),
                    Some(CoinStatus::CONFIRMED),
                    "{d:?} released the lock, so the coin must become spendable again"
                );
            }
        }
    }

    /// **THE ORDERING, asserted on the source itself.**
    ///
    /// `finish_cancel` must consume the reply through `into_result()?` BEFORE it touches the coin's
    /// status. That single `?` is the entire reason a refusal cannot resurrect a still-locked coin,
    /// and it is one line-reorder away from being wrong in a way no unit test on either piece would
    /// notice — both `read_cancel_reply` and `status_after_cancel` stay green while the composition
    /// silently unlocks coins the coordinator never released.
    #[test]
    fn the_status_restore_is_gated_behind_the_reply_check() {
        const SIGNATURE: &str = "async fn finish_cancel(";
        let src = include_str!("transfer_sender.rs");
        let start = src.find(SIGNATURE).expect("finish_cancel must exist");
        let rest = &src[start..];
        let body = &rest[..rest.find("\n}\n").expect("finish_cancel must be terminated")];

        let gate = body.find("into_result()?").expect(
            "finish_cancel must consume the reply with `into_result()?` — a refusal has to become an \
             error BEFORE any local bookkeeping runs",
        );
        let restore = body
            .find("persist_coin_status")
            .expect("finish_cancel must restore the coin status on success");
        assert!(
            gate < restore,
            "`into_result()?` must come BEFORE `persist_coin_status`, otherwise a REFUSED \
             cancellation would mark a still-locked coin spendable.\n---\n{body}\n---"
        );
        assert_eq!(
            body.matches("persist_coin_status").count(),
            1,
            "exactly one restore path, so the gate above covers all of them"
        );
    }

    /// Every `CancelDecision` maps to exactly one reading, and only the two successes are successes.
    /// A future decision landing in the success bucket is the silent-degradation shape this repo
    /// guards against, so the mapping is asserted exhaustively rather than case by case.
    #[test]
    fn every_decision_maps_to_a_distinct_reading_and_only_two_are_successes() {
        use CancelDecision::*;
        let all = [
            Allow,
            AlreadyCancelled,
            NoSuchTransfer,
            AlreadyClaimed,
            Batched,
            ClaimInFlight,
            RecipientConsentRequired,
            RecipientSignatureInvalid,
            RecipientConsentStale,
        ];
        let mut successes = 0usize;
        for d in all {
            let reply = read_cancel_reply("sid-1", &body(d, None));
            match (&reply, d) {
                (CancelReply::Done(CancelOutcome::Cancelled), Allow) => successes += 1,
                (CancelReply::Done(CancelOutcome::AlreadyCancelled), AlreadyCancelled) => {
                    successes += 1
                }
                (CancelReply::NeedsRecipientConsent(_), RecipientConsentRequired) => {}
                (CancelReply::Refused(r), other) => {
                    assert_eq!(
                        r.decision,
                        Some(other),
                        "{other:?} must be recognised by name, not fall through as unknown"
                    );
                }
                (reply, other) => panic!("{other:?} read as {reply:?}"),
            }
        }
        assert_eq!(successes, 2, "exactly Allow and AlreadyCancelled release the lock");
    }

    // ==========================================================================================
    // THE RECIPIENT SIGNS WHAT IT WAS SHOWN — the last of the blind-signing surface.
    //
    // Taking the recipient key out of `cancel_consent`'s parameters removed the sender's ability
    // to AIM a consent. It did not make the preview and the signature the same act. Those two
    // assertions below are what closes the remaining gap, and both of them are about SOURCE SHAPE
    // rather than about a value returned at runtime — stated plainly, because both concern
    // properties no value can exhibit:
    //
    //   * "this type cannot be constructed elsewhere" is a compile-time fact, and Rust has no
    //     built-in compile-fail test (`combine.rs`'s
    //     `a_combine_plan_cannot_be_built_outside_its_validating_constructor` says the same thing
    //     at length, and this is the same device applied to the same shape); and
    //   * "the bytes signed are the bytes previewed" is a property of a function whose every other
    //     step is a network call. This file already pins one composition this way
    //     (`the_status_restore_is_gated_behind_the_reply_check`), for the same reason: the pieces
    //     stay green while the composition goes wrong.
    // ==========================================================================================

    /// Everything before the first top-level `#[cfg(test)]` — the production half of this file, so
    /// the string literals in this very module cannot satisfy the assertions below.
    fn production_half() -> &'static str {
        let src = include_str!("transfer_sender.rs");
        match src.find("\n#[cfg(test)]") {
            Some(at) => &src[..at + 1],
            None => src,
        }
    }

    /// The `mod previewed { … }` span, by brace counting from its opening line.
    fn previewed_span(src: &str) -> (usize, usize) {
        let at = src.find("\nmod previewed {").expect(
            "the previewed-transfer type must live in a PRIVATE module — `mod linked` (tesr.rs:83) \
             and `mod gated` (combine.rs:230) are this repo's precedent for making an encapsulation \
             claim a compile error instead of a docstring",
        );
        let open = at + src[at..].find('{').expect("the module opens");
        let mut depth = 0usize;
        for (i, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (at, open + i);
                    }
                }
                _ => {}
            }
        }
        panic!("`mod previewed` never closes");
    }

    /// **THE DOCSTRING'S CLAIM, MADE TRUE.** `CancelConsentRequest` says "Every field here is
    /// derived from a message this wallet DECRYPTED with its own key. Nothing in it is asserted by
    /// the sender" — while being a `pub struct` with every field `pub`. Anyone can write
    /// `CancelConsentRequest { amount: 10_000, transfer_digest: <the 1M coin's>, .. }`, which is
    /// the misdirection attack restated: an amount shown to a human paired with a digest that
    /// abandons something else. A claim that holds only while nobody writes the obvious literal is
    /// not a claim.
    ///
    /// The fix is `combine.rs`'s, verbatim: private fields inside a private module whose only
    /// export is the constructor that derives EVERY field — the amount, the coin, the colour and
    /// the digest — from ONE decrypted message. The fields then cannot be made to disagree with
    /// each other, which is the whole property a preview needs to have.
    #[test]
    fn a_previewed_consent_cannot_be_assembled_field_by_field() {
        let src = production_half();
        let (start, end) = previewed_span(src);
        let previewed = &src[start..end];

        // 1. The type is DECLARED inside the module…
        assert!(
            previewed.contains("struct CancelConsentRequest {"),
            "`CancelConsentRequest` must be declared inside `mod previewed`"
        );

        // 2. …with no `pub` field. One `pub` field would not on its own permit a literal, but it is
        //    the first step back to one, and the accessors give reads without giving construction.
        let at = previewed.find("pub struct CancelConsentRequest {").expect("declared");
        let decl =
            &previewed[at..at + previewed[at..].find("\n    }\n").expect("the struct closes")];
        for line in decl.lines().skip(1) {
            assert!(
                !line.trim_start().starts_with("pub "),
                "`CancelConsentRequest` must keep every field PRIVATE — `{}` re-opens the struct \
                 literal, and with it the ability to pair one transfer's amount with another \
                 transfer's digest",
                line.trim()
            );
        }

        // 3. …and no struct literal exists anywhere OUTSIDE the module. This is the assertion that
        //    fires if somebody moves the type back to file scope or exports a raw constructor.
        let outside: String = format!("{}{}", &src[..start], &src[end..])
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !outside.contains("CancelConsentRequest {"),
            "a `CancelConsentRequest {{ … }}` literal outside `mod previewed` is a preview nobody \
             previewed"
        );

        // 4. The deriving constructor lives inside the module too, so it cannot be called with a
        //    hand-built set of fields from anywhere else in this file.
        assert!(
            previewed.contains("fn from_decrypted("),
            "the ONLY constructor must live INSIDE `mod previewed` and take the decrypted message, \
             so every field is derived from the same material"
        );
    }

    /// **WHAT WAS PREVIEWED IS WHAT IS SIGNED.**
    ///
    /// `preview_cancel_consent` and `cancel_consent` used to be two independent peeks at the
    /// coordinator's mailbox: the preview showed a human one transfer's amount, and the signing
    /// call then went back to the network, re-derived a digest from whatever the mailbox held by
    /// then, and signed THAT. Nothing linked the two. A sender that can move the row between the
    /// two calls — the batched-then-non-batched ordering the re-address guard does not cover is one
    /// way — gets a signature over material the recipient was never shown, which is the
    /// blind-signing defect with the coordinator's row in place of the sender's out-of-band key.
    ///
    /// So `cancel_consent` must take the APPROVED OBJECT and sign the digest carried in it. Asserted
    /// on the source because the only thing left in the function is a network call: the assertion is
    /// that the function has no second source of truth to drift towards.
    #[test]
    fn the_consent_signs_the_previewed_object_and_never_re_derives_it() {
        let src = production_half();
        let at = src
            .find("pub async fn cancel_consent(")
            .expect("`cancel_consent` must exist");
        let rest = &src[at..];
        let body = &rest[..rest.find("\n}\n").expect("`cancel_consent` must be terminated")];
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains("approved: &CancelConsentRequest"),
            "`cancel_consent` must take the PREVIEWED OBJECT, not a bare coin id it re-resolves \
             itself — a recipient consents to a transfer it was shown, and an id is not a \
             transfer.\n---\n{code}\n---"
        );
        for re_derivation in ["peek_pending_transfers", "consent_target_for", "consent_digest_for"]
        {
            assert!(
                !code.contains(re_derivation),
                "`cancel_consent` must not reach for `{re_derivation}`: a second look at the \
                 mailbox is a second transfer, and the one it would sign is not the one the human \
                 approved.\n---\n{code}\n---"
            );
        }
        assert!(
            code.contains("approved.transfer_digest()"),
            "the digest signed must come OUT OF the approved object.\n---\n{code}\n---"
        );
    }

    /// The third local refusal, TYPED like the other two.
    ///
    /// A wallet can hold a preview whose receiving slot it no longer has — a restore from an older
    /// backup, a coin pruned between the two calls. Signing is then impossible, and the answer must
    /// be a name a caller can match on rather than prose, exactly as `NoPendingTransfer` and
    /// `AlreadyClaimed` are. An untyped `anyhow!` here is the one refusal in this family a UI would
    /// have to string-match.
    #[test]
    fn the_missing_receiving_slot_refusal_is_typed_like_the_others() {
        let src = production_half();
        assert!(
            src.contains("ReceivingSlotGone"),
            "the 'this wallet no longer holds that receiving slot' case must be a named \
             `ConsentBlocked` variant, not a bare `anyhow!`"
        );
        let at = src.find("fn signing_slot_for").expect(
            "the slot lookup must be a PURE function taking the approved object and the wallet's \
             coins, so its refusal is testable without a coordinator",
        );
        let sig = &src[at..at + src[at..].find(" {").expect("the signature ends")];
        assert!(
            sig.contains("ConsentUnavailable"),
            "`signing_slot_for` must refuse with the same typed error as the other two \
             refusals: {sig}"
        );
    }

    // ------------------------------------------------------------------------------------------
    // BEHAVIOUR on the surface the three assertions above forced into existence. Written after the
    // fix and green on their first run — stated plainly, because they did not drive it. What they
    // do is stop it silently rotting: the census pins the SHAPE, these pin what the shape is for.
    // ------------------------------------------------------------------------------------------

    fn coin_with_auth(auth_pubkey: &str) -> Coin {
        Coin {
            index: 0,
            user_privkey: String::new(),
            user_pubkey: String::new(),
            auth_privkey: String::new(),
            auth_pubkey: auth_pubkey.to_string(),
            derivation_path: String::new(),
            fingerprint: String::new(),
            address: String::new(),
            backup_address: String::new(),
            server_pubkey: None,
            aggregated_pubkey: None,
            aggregated_address: None,
            utxo_txid: None,
            utxo_vout: None,
            amount: None,
            statechain_id: None,
            signed_statechain_id: None,
            locktime: None,
            secret_nonce: None,
            public_nonce: None,
            blinding_factor: None,
            server_public_nonce: None,
            tx_cpfp: None,
            tx_withdraw: None,
            withdrawal_address: None,
            status: CoinStatus::CONFIRMED,
            duplicate_index: 0,
            single_use: false,
            epoch_deadline: None,
        }
    }

    /// **THE MISDIRECTION ATTACK, ON THE OBJECT.** Bob is receiving a 10k coin on one slot and a 1M
    /// coin on another. Alice describes the small one. Whatever coin id reaches the preview, the
    /// object that comes back describes THAT coin truthfully — its own branch-validated amount, its
    /// own receiving key, its own digest — and every one of those fields comes from the same
    /// decrypted message. There is no arrangement of the API in which the amount shown belongs to
    /// one transfer and the binding to another.
    #[test]
    fn a_preview_cannot_show_one_transfers_amount_while_binding_anothers() {
        let small = CancelConsentRequest::from_decrypted(&pending("sid-small", "02aaa", 10_000));
        let big = CancelConsentRequest::from_decrypted(&pending("sid-big", "02bbb", 1_000_000));

        assert_eq!(small.amount(), 10_000);
        assert_eq!(big.amount(), 1_000_000);
        assert_eq!(small.recipient_auth_pub_key(), "02aaa");
        assert_ne!(
            small.transfer_digest(),
            big.transfer_digest(),
            "two transfers must not share a binding, or one consent would release either"
        );
        // The digest is a function of the SAME message the amount came from — recomputing it from
        // the small transfer's material must reproduce the small object's binding and nothing else.
        assert_eq!(
            small.transfer_digest(),
            consent_digest_for(&pending("sid-small", "02aaa", 10_000)),
            "the binding must be derivable from the material the recipient decrypted"
        );
    }

    /// A recipient that no longer holds the previewed receiving slot must be told so BY NAME, and
    /// must not fall back to any other key it happens to have. Signing with the wrong key is not a
    /// weaker consent — it is a signature the coordinator rejects, given away for nothing.
    #[test]
    fn a_missing_receiving_slot_refuses_by_name_rather_than_signing_with_another_key() {
        let approved = CancelConsentRequest::from_decrypted(&pending("sid-1", "02aaa", 10_000));
        // The wallet holds a DIFFERENT slot — the tempting fallback.
        let coins = [coin_with_auth("02bbb")];

        let err = signing_slot_for(&approved, &coins)
            .expect_err("a slot this wallet does not hold must not resolve to another one");
        assert_eq!(err.reason, ConsentBlocked::ReceivingSlotGone);
        assert_eq!(err.statechain_id, "sid-1");
        assert!(err.to_string().contains("no longer holds"), "{err}");

        // …and the right slot resolves to exactly that coin.
        let coins = [coin_with_auth("02bbb"), coin_with_auth("02aaa")];
        assert_eq!(signing_slot_for(&approved, &coins).unwrap().auth_pubkey, "02aaa");
    }

    /// The colour is part of what is being abandoned. A recipient shown sats alone, for a coin
    /// carrying an RGB allocation, has been shown the smaller half of the decision.
    #[test]
    fn the_preview_says_whether_an_rgb_allocation_is_being_abandoned_too() {
        let plain = CancelConsentRequest::from_decrypted(&pending("sid-1", "02aaa", 10_000));
        assert!(!plain.is_coloured());
        assert_eq!(plain.rgb_consignment(), None);

        let mut with_token = pending("sid-2", "02bbb", 10_000);
        with_token.rgb_consignment = Some("consignment-bytes".to_string());
        let coloured = CancelConsentRequest::from_decrypted(&with_token);
        assert!(coloured.is_coloured(), "a token carrier must not preview as a plain sat payment");
        assert_eq!(coloured.rgb_consignment(), Some("consignment-bytes"));
    }

    /// Claimable material is ALWAYS posted for anything previewable: the object exists only because
    /// this wallet downloaded and decrypted the message. That is the fact which makes the
    /// recipient's consent necessary rather than optional, so it is stated rather than inferred.
    #[test]
    fn everything_previewable_has_claimable_material_posted() {
        let p = CancelConsentRequest::from_decrypted(&pending("sid-1", "02aaa", 10_000));
        assert!(p.claimable_material_posted());
        assert_eq!(p.statechain_id(), "sid-1");
        assert_eq!(p.funding_txid(), "f".repeat(64));
        assert_eq!(p.funding_vout(), 0);
    }
}
