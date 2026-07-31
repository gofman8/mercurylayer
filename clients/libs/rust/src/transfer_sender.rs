use std::{cmp::Ordering, str::FromStr};

use crate::{client_config::ClientConfig, deposit::create_tx1, sqlite_manager::{get_backup_txs, get_wallet, update_backup_txs, update_wallet}, transaction::new_transaction, utils::info_config};
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

    let backup_transactions = get_backup_txs(&client_config.pool, &wallet.name, &statechain_id).await?;

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

    // create backup transaction for every coin
    let backup_transactions = get_backup_txs(&client_config.pool, &wallet.name, &statechain_id).await?;

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
    matches!(
        reason,
        FLAT_RGB_CARRIER | FLAT_TERMINALIZED_CARRIER | FLAT_FUNDING_NOT_ONCHAIN | FLAT_NOT_BINDABLE
    )
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
    /// The coin carries an RGB allocation — a plain tier spend carries no state transition and would
    /// DESTROY it (terminal freeze). Proven by a consignment on the coin's own backup row, or by the
    /// SDK ladder pass positively recording `rgb-carrier` (RGB state is the one authority this crate
    /// structurally cannot consult itself).
    RgbCarrier,
    /// The coin is flagged `single_use` — a terminalized/combine carrier, same terminal-freeze rule.
    /// Proven from the COIN's own flag, never from a record: this flag has no production setter
    /// today (its only setters have zero non-test callers), so assuming it would be assuming a state
    /// that does not exist.
    TerminalizedCarrier,
    /// [B0] The coin's funding `F` is not on chain, so a ladder trigger would have no prevout to
    /// spend. Proven by exit material that PARSES — a non-empty `branch-<id>` chain, or a
    /// `ctesr-<id>` child bundle that parses AND names this coin. Never by a failed lookup, and
    /// never by mere row existence.
    FundingNotOnChain,
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

/// LICENCE 1 — RGB CARRIER, from a consignment on the coin's own backup row or from the SDK's
/// positive `rgb-carrier` record.
fn licence_rgb_carrier(
    rows: &WalletRows,
    statechain_id: &str,
    recorded: Option<&str>,
) -> Result<Option<PermanentLicence>> {
    if let Some(json) = rows.get(statechain_id) {
        let txs: Vec<BackupTx> = serde_json::from_str(json).map_err(|e| {
            anyhow!(
                "statechain id {statechain_id} has a backup row that could not be parsed ({e}). \
                 Refusing to convey it on the flat lane — this client cannot tell whether the coin \
                 is an RGB carrier (legitimately flat) or a coin that lost its exit ladder."
            )
        })?;
        if txs.iter().any(|b| b.rgb_consignment.is_some()) {
            return Ok(Some(PermanentLicence::RgbCarrier));
        }
    }
    // A booked-but-consignment-less carrier (a fresh issuance) is invisible to this crate, so the
    // component that CAN read RGB state asserts it for us. This is the one licence a recorded string
    // can carry on its own, and it is deliberately the narrowest possible acceptance: one exact
    // spelling, and only that spelling.
    if recorded == Some(FLAT_RGB_CARRIER) {
        return Ok(Some(PermanentLicence::RgbCarrier));
    }
    Ok(None)
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

/// LICENCE 3 — [B0] FUNDING NOT ON CHAIN, proven by exit material that PARSES.
///
/// A prior round accepted `ctesr-<id>` on row EXISTENCE alone while requiring its `branch-<id>`
/// sibling to parse — so any row under that key, valid or not, licensed the flat lane. Both must now
/// parse, and the child bundle must additionally name THIS coin.
fn licence_funding_not_onchain(
    rows: &WalletRows,
    statechain_id: &str,
) -> Result<Option<PermanentLicence>> {
    if let Some(json) = rows.get(&format!("branch-{statechain_id}")) {
        let txs: Vec<BackupTx> = serde_json::from_str(json).map_err(|e| {
            anyhow!(
                "statechain id {statechain_id} has an exit-branch row that could not be parsed \
                 ({e}). Refusing to convey it on the flat lane — its exit material is unreadable, \
                 so this client cannot tell whether the coin is legitimately un-laddered."
            )
        })?;
        // An EMPTY branch chain proves nothing about `F`; fall through rather than license it.
        if !txs.is_empty() {
            return Ok(Some(PermanentLicence::FundingNotOnChain));
        }
    }
    if let Some(json) = rows.get(&format!("ctesr-{statechain_id}")) {
        let cb: crate::tesr::ChildTesrBundle = serde_json::from_str(json).map_err(|e| {
            anyhow!(
                "statechain id {statechain_id} has a split-child bundle row that could not be \
                 parsed ({e}). Refusing to convey it on the flat lane — a row we cannot read is not \
                 evidence that the coin's funding is off chain."
            )
        })?;
        if cb.child_statechain_id != statechain_id {
            return Err(anyhow!(
                "statechain id {statechain_id} has a split-child bundle row that names a different \
                 coin ({}). Refusing to convey it on the flat lane — that row is not evidence about \
                 this coin.",
                cb.child_statechain_id
            ));
        }
        return Ok(Some(PermanentLicence::FundingNotOnChain));
    }
    Ok(None)
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

    if let Some(l) = licence_rgb_carrier(&rows, statechain_id, recorded)? {
        return Ok(Some(l));
    }
    if let Some(l) = licence_terminalized_carrier(coin) {
        return Ok(Some(l));
    }
    if let Some(l) = licence_funding_not_onchain(&rows, statechain_id)? {
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
    cur_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| {
            anyhow!(
                "statechain id {statechain_id} is laddered but its state CSV ({cur_csv}) is at the \
                 floor (δ {}, floor {}), so the receiver-paying state S' cannot be built one δ \
                 lower. Refusing to fall back to the flat lane — the receiver would reject it on \
                 the signature census. Renew or roll the ladder over (or re-anchor the coin \
                 on-chain with refresh) and retry. Nothing has been co-signed: the coin is \
                 untouched.",
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

    let are_there_duplicate_coins_withdrawn = wallet.coins.iter().any(|c| {
        c.statechain_id == Some(statechain_id.to_string()) &&
        (c.status == CoinStatus::WITHDRAWING || c.status == CoinStatus::WITHDRAWING) &&
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

    // Open the transfer at the coordinator now, AFTER every sender pre-sign above (see the note by
    // `input_txid`). Returns x1 for the t1 blinding tweak.
    let x1 = get_new_x1(&client_config, &statechain_id, &signed_statechain_id, &recipient_auth_pubkey.to_string(), batch_id).await?;

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

    let response: TransferSenderResponsePayload = serde_json::from_str(value.as_str()).expect(&format!("failed to parse: {}", value.as_str()));

    Ok(response.x1)
}



#[cfg(test)]
mod flat_lane_tests {
    use super::*;

    /// [B1] The set of reasons that LICENSE a flat conveyance is exactly the structurally-permanent
    /// ones. A transient reason is a deferral; if one ever creeps back onto this list, a single
    /// blip in a token wallet licenses that coin to convey flat forever after.
    #[test]
    fn only_permanent_reasons_license_the_flat_lane() {
        for permanent in [
            FLAT_RGB_CARRIER,
            FLAT_TERMINALIZED_CARRIER,
            FLAT_FUNDING_NOT_ONCHAIN,
            FLAT_NOT_BINDABLE,
        ] {
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
        // Neither a licence nor a deferral: these are refusals with their own remedies.
        for other in [
            FLAT_DUPLICATE_DEPOSIT,
            FLAT_LADDER_UNREADABLE,
            FLAT_BINDING_UNRESOLVED,
            "some-future-spelling",
            "",
        ] {
            assert!(
                !is_legitimate_flat_reason(other),
                "'{other}' must not license a flat conveyance"
            );
        }
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

    /// [B0] is proven by exit material that PARSES. A prior round accepted `ctesr-<id>` on row
    /// EXISTENCE alone while requiring its `branch-<id>` sibling to parse.
    #[test]
    fn offchain_funding_needs_material_that_parses_and_names_this_coin() {
        let row = |k: &str, v: &str| WalletRows(vec![(k.to_string(), v.to_string())]);
        // Existence is not evidence.
        assert!(licence_funding_not_onchain(&row("ctesr-abc", "not json"), "abc").is_err());
        assert!(licence_funding_not_onchain(&row("ctesr-abc", "{}"), "abc").is_err());
        assert!(licence_funding_not_onchain(&row("branch-abc", "not json"), "abc").is_err());
        // An EMPTY branch chain proves nothing — no licence, and no error either.
        assert_eq!(licence_funding_not_onchain(&row("branch-abc", "[]"), "abc").unwrap(), None);
        // No rows at all: nothing learned here.
        assert_eq!(licence_funding_not_onchain(&WalletRows(vec![]), "abc").unwrap(), None);
    }

    /// A terminalized carrier must be POSITIVELY proven from the coin, never assumed from a record —
    /// `single_use` is dead in production today (its only setters have no non-test callers).
    #[test]
    fn terminalized_carrier_is_proven_from_the_coin_not_a_record() {
        let rows = WalletRows(vec![(
            "ladderskip-abc".into(),
            format!("{{\"reason\":\"{FLAT_TERMINALIZED_CARRIER}\"}}"),
        )]);
        // The record alone licenses NOTHING: no carrier probe accepts that spelling...
        assert_eq!(
            licence_rgb_carrier(&rows, "abc", Some(FLAT_TERMINALIZED_CARRIER)).unwrap(),
            None
        );
        // ...and the licence comes from the coin's own flag, which no test row can fake.
        assert_eq!(recorded_flat_reason(&rows, "abc").unwrap().as_deref(), Some(FLAT_TERMINALIZED_CARRIER));
    }
}
