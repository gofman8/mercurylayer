use std::{collections::{HashMap, HashSet}, str::FromStr};

use crate::{sqlite_manager::{get_wallet, update_wallet, insert_or_update_backup_txs}, client_config::ClientConfig, utils};
use anyhow::{anyhow, Ok, Result};
use bitcoin::{Txid, Address};
use chrono::Utc;
use electrum_client::ElectrumApi;
use mercurylib::{utils::{get_network, InfoConfig}, wallet::{get_previous_outpoint, Activity, BackupTx, Coin, CoinStatus}};
use reqwest::StatusCode;

pub async fn new_transfer_address(client_config: &ClientConfig, wallet_name: &str) -> Result<String>{

    let wallet = get_wallet(&client_config.pool, &wallet_name).await?;
    
    let mut wallet = wallet.clone();

    let coin = wallet.get_new_coin()?;

    wallet.coins.push(coin.clone());

    update_wallet(&client_config.pool, &wallet).await?;

    Ok(coin.address)
}

pub struct TransferReceiveResult {
    pub is_there_batch_locked: bool,
    pub received_statechain_ids: Vec<String>,
    /// Transfers this poll found had been CANCELLED. Non-empty means a payment this wallet was
    /// expecting was withdrawn — [`execute`] also returns `Err` in that case, so a caller that only
    /// inspects the `Result` still cannot read a cancellation as "nothing arrived".
    pub cancelled_statechain_ids: Vec<String>,
}

/// A transfer this wallet was claiming had been cancelled (coordinator answered 410 with
/// `TransferCancelledError`).
///
/// A distinct error type, not a formatted string, because the receive loop's `println!` + `continue`
/// treats every claim failure as a transient miss. This one is terminal and is the user's money: it
/// has to be distinguishable by `downcast_ref` so the loop can record it and surface it instead of
/// swallowing it. Both claim paths therefore propagate the error UNCHANGED rather than re-wrapping
/// it in a fresh `anyhow!("Error: {}")`, which would erase the type.
#[derive(Debug, Clone)]
pub struct TransferWasCancelled {
    pub statechain_id: String,
    pub message: String,
}

impl std::fmt::Display for TransferWasCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transfer of statechain id {} was cancelled: {}", self.statechain_id, self.message)
    }
}

impl std::error::Error for TransferWasCancelled {}

pub struct DuplicatedCoinData {
    pub txid: String,
    pub vout: u32,
    pub amount: u64,
    pub index: u32,
}

pub struct MessageResult {
    pub is_batch_locked: bool,
    pub statechain_id: Option<String>,
    pub duplicated_coins: Vec<DuplicatedCoinData>,
}

pub fn sort_coins_by_statechain(coins: &mut Vec<Coin>) {
    // Create a map to store the position of first occurrence of each statechain_id
    let mut first_positions: HashMap<String, usize> = HashMap::new();

    // Record the position of the first occurrence of each statechain_id
    for (idx, coin) in coins.iter().enumerate() {
        if let Some(id) = &coin.statechain_id {
            first_positions.entry(id.clone()).or_insert(idx);
        }
    }

    // Sort the vector maintaining original order of different statechain_ids
    coins.sort_by(|a, b| {
        match (&a.statechain_id, &b.statechain_id) {
            (None, None) => a.duplicate_index.cmp(&b.duplicate_index),
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(id_a), Some(id_b)) => {
                if id_a == id_b {
                    // Same statechain_id: sort by duplicate_index
                    a.duplicate_index.cmp(&b.duplicate_index)
                } else {
                    // Different statechain_ids: compare their first positions
                    first_positions[id_a].cmp(&first_positions[id_b])
                }
            }
        }
    });
}

/// One or more transfers this wallet was expecting had been CANCELLED, named by statechain id.
///
/// The TYPED form of [`execute`]'s refusal. `execute` still returns `Err` — that is the loud signal,
/// and a caller which only inspects the `Result` must never read a withdrawn payment as an idle
/// mailbox. But a caller that reports cancellations PROPERLY needs the ids, and its only other
/// options are to lose the whole poll or to scrape them out of prose. The SDK's `claim()` downcasts
/// this to put the ids on `ClaimResult::cancelled_transfers` and emit
/// `WalletEvent::TransferCancelled`, instead of `?`-ing away the deposits and receipts that landed
/// in the very same pass.
///
/// Everything this poll DID receive is already persisted before this is constructed
/// ([`execute_reporting_cancellations`] returns after `update_wallet`), so recovering from it loses
/// nothing.
#[derive(Debug, Clone)]
pub struct TransfersCancelledInPoll {
    pub statechain_ids: Vec<String>,
}

impl std::fmt::Display for TransfersCancelledInPoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a transfer to this wallet was CANCELLED and will never complete: statechain id(s) {}. \
             The payment did not arrive. Any other transfers in this poll were received normally \
             and have been saved.",
            self.statechain_ids.join(", ")
        )
    }
}

impl std::error::Error for TransfersCancelledInPoll {}

/// One receive poll. Cancellations are REPORTED on the result rather than raised.
///
/// This is the whole body; [`execute`] is this plus the `Err` for callers that want a cancellation
/// to be impossible to overlook. Prefer this one only when you actually surface
/// `cancelled_statechain_ids` — reading it and dropping it is the silent-degradation shape this
/// module's `TransferWasCancelled` exists to prevent.
pub async fn execute_reporting_cancellations(
    client_config: &ClientConfig,
    wallet_name: &str,
) -> Result<TransferReceiveResult> {
    execute_inner(client_config, wallet_name).await
}

/// One receive poll, with a cancellation raised as [`TransfersCancelledInPoll`].
pub async fn execute(client_config: &ClientConfig, wallet_name: &str) -> Result<TransferReceiveResult>{
    let result = execute_inner(client_config, wallet_name).await?;

    // Persist FIRST, then fail — `execute_inner` has already saved everything that did arrive this
    // poll (including the TransferCancel activities). A cancellation must not be returnable as an
    // ordinary success, because "no coin appeared" is exactly what an idle mailbox looks like and
    // the user would read it as nothing having happened.
    if !result.cancelled_statechain_ids.is_empty() {
        return Err(anyhow::Error::new(TransfersCancelledInPoll {
            statechain_ids: result.cancelled_statechain_ids,
        }));
    }

    Ok(result)
}

async fn execute_inner(client_config: &ClientConfig, wallet_name: &str) -> Result<TransferReceiveResult>{

    let mut wallet = get_wallet(&client_config.pool, &wallet_name).await?;

    let info_config = utils::info_config(&client_config).await.unwrap();

    let mut unique_auth_pubkeys: HashSet<String> = HashSet::new();
    
    for coin in wallet.coins.iter() {
        unique_auth_pubkeys.insert(coin.auth_pubkey.clone());
    }

    let mut enc_msgs_per_auth_pubkey: HashMap<String, Vec<String>> = HashMap::new();

    for auth_pubkey in unique_auth_pubkeys {

        let enc_messages = get_msg_addr(&auth_pubkey, &client_config).await?;
        if enc_messages.len() == 0 {
            continue;
        }

        enc_msgs_per_auth_pubkey.insert(auth_pubkey.clone(), enc_messages);
    }

    let mut is_there_batch_locked = false;

    let mut received_statechain_ids =  Vec::<String>::new();

    let mut cancelled_statechain_ids = Vec::<String>::new();

    let mut temp_coins = wallet.coins.clone();
    let mut temp_activities = wallet.activities.clone();

    let mut duplicated_coins: Vec<Coin> = Vec::new();

    let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
    let blockheight = block_header.height as u32;

    for (key, values) in &enc_msgs_per_auth_pubkey {

        let auth_pubkey = key.clone();

        for enc_message in values {

            let coin: Option<&mut Coin> = temp_coins.iter_mut().find(|coin| coin.auth_pubkey == auth_pubkey && coin.status == CoinStatus::INITIALISED);

            if coin.is_some() {

                let mut coin = coin.unwrap();

                let is_msg_valid = validate_encrypted_message(client_config, &coin, enc_message, &wallet.network, &wallet.name, &info_config, blockheight).await;

                if is_msg_valid.is_err() {
                    println!("Validation error: {}", is_msg_valid.err().unwrap().to_string());
                    continue;
                }

                let message_result = process_encrypted_message(client_config, &mut coin, enc_message, &wallet.network, &wallet.name, &mut temp_activities).await;

                if message_result.is_err() {
                    let err = message_result.err().unwrap();
                    if let Some(cancelled) = err.downcast_ref::<TransferWasCancelled>() {
                        record_cancelled_transfer(
                            cancelled,
                            &coin,
                            &mut temp_activities,
                            &mut cancelled_statechain_ids,
                        );
                        continue;
                    }
                    println!("Processing error: {}", err.to_string());
                    continue;
                }

                let message_result = message_result.unwrap();

                if message_result.is_batch_locked {
                    is_there_batch_locked = true;
                }

                if message_result.statechain_id.is_some() {
                    received_statechain_ids.push(message_result.statechain_id.unwrap());
                }

                if message_result.duplicated_coins.len() > 0 {

                    assert!(!message_result.is_batch_locked);

                    for duplicated_coin_data in message_result.duplicated_coins {
                        let mut duplicated_coin = coin.clone();
                        duplicated_coin.status = CoinStatus::DUPLICATED;
                        duplicated_coin.utxo_txid = Some(duplicated_coin_data.txid);
                        duplicated_coin.utxo_vout = Some(duplicated_coin_data.vout);
                        duplicated_coin.amount = Some(duplicated_coin_data.amount as u32);
                        duplicated_coin.duplicate_index = duplicated_coin_data.index;
                        duplicated_coins.push(duplicated_coin);
                    }
                }

            } else {

                let new_coin = mercurylib::transfer::receiver::duplicate_coin_to_initialized_state(&wallet, &auth_pubkey);

                if new_coin.is_err() {
                    println!("Error: {}", new_coin.err().unwrap().to_string());
                    continue;
                }

                let mut new_coin = new_coin.unwrap();

                let is_msg_valid = validate_encrypted_message(client_config, &new_coin, enc_message, &wallet.network, &wallet.name, &info_config, blockheight).await;

                if is_msg_valid.is_err() {
                    println!("Validation error: {}", is_msg_valid.err().unwrap().to_string());
                    continue;
                }

                let message_result = process_encrypted_message(client_config, &mut new_coin, enc_message, &wallet.network, &wallet.name, &mut temp_activities).await;

                if message_result.is_err() {
                    let err = message_result.err().unwrap();
                    if let Some(cancelled) = err.downcast_ref::<TransferWasCancelled>() {
                        record_cancelled_transfer(
                            cancelled,
                            &new_coin,
                            &mut temp_activities,
                            &mut cancelled_statechain_ids,
                        );
                        continue;
                    }
                    println!("Processing error: {}", err.to_string());
                    continue;
                }

                temp_coins.push(new_coin.clone());

                let message_result = message_result.unwrap();

                if message_result.is_batch_locked {
                    is_there_batch_locked = true;
                }

                if message_result.statechain_id.is_some() {
                    received_statechain_ids.push(message_result.statechain_id.unwrap());
                }

                if message_result.duplicated_coins.len() > 0 {

                    assert!(!message_result.is_batch_locked);

                    for duplicated_coin_data in message_result.duplicated_coins {
                        let mut duplicated_coin = new_coin.clone();
                        duplicated_coin.status = CoinStatus::DUPLICATED;
                        duplicated_coin.utxo_txid = Some(duplicated_coin_data.txid);
                        duplicated_coin.utxo_vout = Some(duplicated_coin_data.vout);
                        duplicated_coin.amount = Some(duplicated_coin_data.amount as u32);
                        duplicated_coin.duplicate_index = duplicated_coin_data.index;
                        // temp_coins.push(duplicated_coin);
                        duplicated_coins.push(duplicated_coin);
                    }
                }
            }
        }
    }

    temp_coins.extend(duplicated_coins);
    sort_coins_by_statechain(&mut temp_coins);
    wallet.coins = temp_coins.clone();
    wallet.activities = temp_activities.clone();

    update_wallet(&client_config.pool, &wallet).await?;

    // Persist FIRST, then report. Everything that did arrive this poll is saved by the
    // `update_wallet` above (including the TransferCancel activities), which is what makes it safe
    // for `execute` to raise the cancellation immediately afterwards and for
    // `execute_reporting_cancellations` to hand it back on the result instead.
    Ok(TransferReceiveResult{
        is_there_batch_locked,
        received_statechain_ids,
        cancelled_statechain_ids,
    })
}

/// Book a cancelled incoming transfer into the wallet's activity log.
///
/// The receiving slot's `CoinStatus` is deliberately left alone. There is no "cancelled" status, and
/// the nearest existing one (`INVALIDATED`) means something specific — a duplicate superseded by a
/// transfer — that other code branches on; overloading it would make a cancelled payment
/// indistinguishable from a stale duplicate in every consumer of the enum. The durable, unambiguous
/// record is the activity entry, and the loud signal is [`execute`]'s `Err`.
fn record_cancelled_transfer(
    cancelled: &TransferWasCancelled,
    coin: &Coin,
    activities: &mut Vec<Activity>,
    cancelled_statechain_ids: &mut Vec<String>,
) {
    println!(
        "Transfer CANCELLED: statechain id {} — {}",
        cancelled.statechain_id, cancelled.message
    );
    activities.push(Activity {
        utxo: match (coin.utxo_txid.as_ref(), coin.utxo_vout) {
            (Some(txid), Some(vout)) => format!("{}:{}", txid, vout),
            // A never-materialised receiving slot has no outpoint yet; name the transfer instead so
            // the entry is still attributable.
            _ => cancelled.statechain_id.clone(),
        },
        amount: coin.amount.unwrap_or(0),
        action: "TransferCancelled".to_string(),
        date: Utc::now().to_rfc3339(),
    });
    cancelled_statechain_ids.push(cancelled.statechain_id.clone());
}

async fn get_msg_addr(auth_pubkey: &str, client_config: &ClientConfig) -> Result<Vec<String>> {

    let path = format!("transfer/get_msg_addr/{}", auth_pubkey.to_string());

    let client = client_config.get_reqwest_client()?;
    let request = client.get(&format!("{}/{}", client_config.statechain_entity, path));

    let http = request.send().await?;
    // Status BEFORE body. `get_msg_addr` answers an unreadable key with a 500 and
    // `{"error": …, "message": "Invalid authentication public key"}`; parsing that as a
    // `GetMsgAddrResponsePayload` reported "missing field `list_enc_transfer_msg`", which reads as a
    // client bug and hides the coordinator's actual complaint. This call is on the RECEIVE polling
    // path, so a refusal it cannot name is a wallet that looks idle while it is in fact being
    // refused — the silent-degradation shape this repo has a CI guard for. See `server_refusal`.
    let status = http.status().as_u16();
    let value = http.text().await?;
    if !(200..300).contains(&status) {
        return Err(crate::utils::server_refusal("transfer/get_msg_addr", status, &value));
    }

    let response: mercurylib::transfer::receiver::GetMsgAddrResponsePayload = serde_json::from_str(value.as_str())?;

    Ok(response.list_enc_transfer_msg)
}

/// A pending incoming transfer this wallet can see WITHOUT unlocking or claiming it. The transfer
/// message is decrypted with the wallet's own auth key, so a transfer appears here ONLY if it is
/// genuinely addressed to this wallet; the amount is read from the funding tx0 (or, for an off-chain
/// sub-coin, its exit branch). Batch-locked transfers appear here too — the peek stops before the
/// receiver/unlock step that the batch lock gates. The SSP uses this to verify, BEFORE paying a
/// Lightning invoice, that the coin latched to the swap is (a) really addressed to it and (b) worth
/// at least the invoice + fee (review C2/C3): without both checks it would pay for a coin sent to
/// someone else, or an undersized coin.
#[derive(Clone, Debug)]
pub struct PendingTransferInfo {
    pub statechain_id: String,
    /// The auth public key of OUR receiving slot that this message was addressed to — derived from
    /// the fact that its private key decrypted the message, never asserted by the sender.
    ///
    /// A consent primitive must not accept this key from a counterparty (that is the phishing
    /// surface: a sender that names a coin and a key it chose can steer a recipient into signing
    /// away something other than what it described). Carrying it here is what lets the recipient
    /// establish it locally instead.
    pub recipient_auth_pub_key: String,
    /// The mailbox ciphertext EXACTLY as downloaded from `GET /transfer/get_msg_addr` (hex).
    ///
    /// This is the only transfer-INSTANCE identity a recipient can bind a signature to: its `t1` is
    /// blinded against the row's server-random `x1`, so re-addressing the same coin to the same key
    /// necessarily changes these bytes. See `mercurylib::transfer::cancel::transfer_consent_digest`.
    pub encrypted_transfer_msg: String,
    /// Sats funding this coin, **branch-validated** (audit [3]): 0 if the branch fails validation,
    /// so a value gate cannot be tricked by an attacker-inflated un-broadcast leaf.
    pub amount: u64,
    /// The coin's RGB consignment envelope (`BackupTx.rgb_consignment`), if it carries a token.
    /// A caller can validate the colored value pre-payment via `validate_pending_token` (audit [4]).
    pub rgb_consignment: Option<String>,
    /// The coin's own funding outpoint (the RGB witness for consignment validation).
    pub funding_txid: String,
    pub funding_vout: u32,
    /// The un-broadcast exit branch (raw tx hex, root-first) the consignment resolves against.
    pub branch_txs: Vec<String>,
    /// **[P3] A COLOURED CHILD's own witness chain**, root→leaf, when this transfer conveys one.
    ///
    /// A child has no `branch_txs`: its consignment resolves against `colored_child_txids()` — the
    /// root ladder, every intermediate spine segment, then its own two rungs. That list is derivable
    /// from `child_tesr_bundle`, which the transfer message already carries, but nothing surfaced it,
    /// so the SSP's pre-pay RGB gate had nothing to resolve against and could only refuse.
    ///
    /// Empty for the flat lane and for a PLAIN child — the distinction is "is there a coloured child
    /// chain here", not "is this a child".
    pub child_witness_txids: Vec<String>,
    /// Pre-pay ladder census (LIGHTNING.md): `true` iff this coin is safe to accept-before-paying.
    ///
    /// It is `true` ONLY when a binding actually ran and passed — there is no "trivially ok" shape any
    /// more ([D1]). Two lanes:
    ///   * a flat TES-R ladder (`protocol_version >= 2`) → `verify_bundle_bound`: the exact-equality
    ///     census (`num_sigs == flat_backups + tiers`) against the LIVE enclave sig-count, so no hidden
    ///     lower-CSV state is present, AND the [C-1] coin binding, so the ladder provably describes THIS
    ///     coin (funding outpoint, on-chain value, on-chain aggregate key, coordinator-recorded
    ///     aggregate for the sid) rather than merely being self-consistent, AND the Model-A owner-exit
    ///     binding ([D2]) — the ladder must exit to the PROSPECTIVE OWNER's own seed-derived key, not a
    ///     third party's;
    ///   * an in-ladder-split CHILD bundle (`protocol_version >= 3`) → `verify_conveyed_child`, bound to
    ///     the LATCHED statechain id, not yet adopted, over a live on-chain parent root ([D3]).
    ///
    /// The SSP's pre-pay gate MUST refuse to pay for any latched coin whose `ladder_census_ok` is
    /// `false`. Fails CLOSED (`false`) on an unrecognised/absent `protocol_version`, a missing or
    /// malformed ladder/child bundle, a funding UTXO that is off-chain/spent/unconfirmed, an
    /// exit address that is not ours, or an unreadable enclave sig-count / coordinator aggregate.
    pub ladder_census_ok: bool,
    /// **WHY the census refused, when it did.** `None` iff `ladder_census_ok`.
    ///
    /// The two census arms below used to collapse their `Err` with `Err(_) => false` and
    /// `.is_ok()`, so a refusal reached the SSP as a bare boolean and its operator was handed a
    /// six-way disjunction — "un-laddered or below the version floor or hidden state or binding
    /// failure or dead funding output or unreadable" — with no way to tell which. That is precisely
    /// the shape this repo refuses everywhere else: a fail-closed gate that cannot say why is a gate
    /// nobody can operate, and it turns a five-minute diagnosis into a bisect.
    ///
    /// Carrying the reason changes NO decision. The gate still fails closed on any error; this is
    /// the sentence that goes with it.
    pub ladder_census_refusal: Option<String>,
}

/// **[D38/D16] `protocol_version` is a message-SHAPE selector, not an ordinal.**
///
/// The three values in play are not generations of one shape. `0` is the un-laddered carrier lane,
/// `2` is a root-ladder conveyance, `4` is a child conveyance carrying key handover. They are three
/// different message SHAPES that happen to be numbered, and comparing them with `>=` asserts
/// something nothing establishes: that an unknown FUTURE value is safely processed by TODAY's rules.
///
/// The code already showed the seam. `MIN_PREPAY_CHILD_PROTOCOL_VERSION` was **3** — a floor over a
/// set that contains no 3 — because 3 was a legacy shape that has now been deleted.
///
/// So membership is EXACT. Anything outside the set is refused by name, and the numeric ordering
/// carries no meaning: no implementer may read a floor as a compatibility promise.
///
/// This also makes the uniffi FFI, which silently strips `protocol_version`, `tesr_ladder` and
/// `child_tesr_bundle`, fail CLOSED — a stripped tag is not in the set — instead of silently
/// downgrading to the un-laddered census. That is the point of exact-set dispatch and is why it is
/// not merely a stylistic tightening.
pub(crate) const ADMISSIBLE_PROTOCOL_VERSIONS: [u32; 3] = [0, 2, 4];

/// The shape a LADDERED conveyance declares. Anything that carries a `tesr_ladder` must be this.
pub(crate) const SHAPE_ROOT_LADDER: u32 = 2;

/// The shape a CHILD conveyance declares — the only one carrying key-handover material. The legacy
/// exit-only child (3) is deleted, so this is now an exact value rather than a floor.
pub(crate) const SHAPE_CHILD: u32 = 4;

/// Refuse any `protocol_version` outside [`ADMISSIBLE_PROTOCOL_VERSIONS`], by name.
pub(crate) fn admissible_shape(v: u32) -> Result<()> {
    if !ADMISSIBLE_PROTOCOL_VERSIONS.contains(&v) {
        return Err(anyhow::anyhow!(
            "transfer message declares protocol_version {v}, which is not one of the admissible \
             message shapes {ADMISSIBLE_PROTOCOL_VERSIONS:?}. This field selects a SHAPE, not a \
             generation: 0 is the un-laddered carrier lane, 2 a root-ladder conveyance, 4 a child \
             conveyance with key handover. Its numeric ordering carries no meaning, so an unknown \
             value cannot be 'at least' anything — it is refused."
        ));
    }
    Ok(())
}

/// Retained under its old name so the two census sites keep reading as before; it is now the exact
/// root-ladder shape rather than a floor.
pub(crate) const MIN_PREPAY_PROTOCOL_VERSION: u32 = SHAPE_ROOT_LADDER;

/// Retained under its old name; now the exact child shape. **It used to be 3, a floor over a set
/// containing no 3** — the clearest evidence that this field was never an ordinal.
pub(crate) const MIN_PREPAY_CHILD_PROTOCOL_VERSION: u32 = SHAPE_CHILD;

/// [D1/D2] PRE-PAY census of a FLAT (TES-R) laddered conveyance, for a party that is about to make an
/// IRREVERSIBLE payment against it (the SSP) and has NOT claimed the coin.
///
/// Fail-closed BY CONSTRUCTION: every step returns `Err`, and the only caller maps `Err` to
/// `ladder_census_ok = false`. There is deliberately no success path that skips a check.
///
/// `my_backup` is the PROSPECTIVE OWNER's seed-derived backup address (the payer's own), derived
/// exactly as the claim path derives the receiver's.
async fn prepay_flat_census(
    client_config: &ClientConfig,
    network: &str,
    my_backup: &str,
    transfer_msg: &mercurylib::transfer::TransferMsg,
    funding_txid: &str,
    funding_vout: u32,
    onchain_tx0: Option<&String>,
    groups: &[Vec<BackupTx>],
    info_config: &InfoConfig,
    blockheight: u32,
) -> Result<()> {
    // [D1] VERSION FLOOR. The previous shape ran the [C-1] binding only inside an
    // `else if protocol_version >= 2` arm and let the final `else` report `ladder_census_ok = true`,
    // so an attacker declaring `protocol_version = 0` skipped the binding entirely and still cleared
    // the gate that authorises an irreversible Lightning leg.
    // [D38/D16] Exact-set first: an unrecognised shape must never reach a comparison.
    admissible_shape(transfer_msg.protocol_version)?;
    if transfer_msg.protocol_version < MIN_PREPAY_PROTOCOL_VERSION {
        return Err(anyhow!(
            "pre-pay census: conveyance declares protocol_version {} but this path accepts only the root-ladder shape {} — refusing (an unrecognised version must never bypass the [C-1] coin binding)",
            transfer_msg.protocol_version,
            MIN_PREPAY_PROTOCOL_VERSION
        ));
    }
    if groups.len() != 1 {
        return Err(anyhow!(
            "pre-pay census: laddered conveyance carries {} funding group(s) — a ladder is rooted at exactly one funding UTXO",
            groups.len()
        ));
    }
    // [R1] An empty backup vector is a rejection, not a vacuous pass — see the sibling guard on the
    // claim path. (`groups.len() == 1` already implies non-empty, but state it so the invariant does
    // not depend on the grouping function's internals.)
    let backup_group = &groups[0];
    if backup_group.is_empty() {
        return Err(anyhow!(
            "pre-pay census: laddered conveyance carries no backup transactions — nothing to count, nothing to bind"
        ));
    }
    // The funding tx must have been read FROM THE CHAIN. A branch-supplied (un-broadcast) funding tx is
    // sender-controlled, so there would be no authority to bind the ladder to.
    let tx0_hex = onchain_tx0.ok_or_else(|| {
        anyhow!("pre-pay census: the coin's funding UTXO is not on-chain — nothing authoritative to bind the ladder to")
    })?;
    let ladder_json = transfer_msg
        .tesr_ladder
        .as_ref()
        .ok_or_else(|| anyhow!("pre-pay census: laddered conveyance is missing its TES-R ladder"))?;
    let bundle: crate::tesr::TesrBundle = serde_json::from_str(ladder_json)
        .map_err(|e| anyhow!("pre-pay census: malformed TES-R ladder: {e}"))?;

    // [D35 / RGB-1] THE FLAT-BACKUP LANE RULE, on the payer's side of the same acceptance set. The
    // claim path runs this too; the pre-pay path must, because it is the one authorising an
    // IRREVERSIBLE Lightning leg — and the shape it guards against (a coloured backup a prior owner
    // still holds over `F`) leaves the payer holding a coin whose allocation an ancestor can take.
    crate::tesr::verify_flat_backup_lane(&bundle, backup_group)
        .map_err(|e| anyhow!("pre-pay census: {e}"))?;

    // [D2] MODEL-A OWNER-EXIT BINDING. `verify_bundle_bound` binds the ladder to the COIN but is
    // structurally incapable of checking WHO the ladder exits to — `owner_exit_address` is not derivable
    // from the funding output or the coordinator record. So a sender can convey a ladder that is
    // perfectly bound to a real, live, correctly-counted coin and still pays a THIRD PARTY on exit; the
    // payer would hand over Lightning money for a coin whose only exit route enriches someone else.
    // The claim path already refuses this (`bundle.owner_exit_address == my_backup`); the pre-pay path
    // must refuse it too, with the PAYER as the prospective owner.
    if bundle.owner_exit_address != my_backup {
        return Err(anyhow!(
            "pre-pay census: the conveyed ladder exits to {} but the prospective owner's key is {} — a coin-bound ladder that pays a third party",
            bundle.owner_exit_address,
            my_backup
        ));
    }

    // The coordinator's record supplies the LIVE sig-count AND the authoritative per-sid aggregate.
    let info = crate::utils::get_statechain_info(&transfer_msg.statechain_id, client_config)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "pre-pay census: the coordinator has no record for statechain id {} (fail-closed)",
                transfer_msg.statechain_id
            )
        })?;

    // A spent or unconfirmed `F` means the ladder is already dead (or not yet real) and must never gate
    // an irreversible Lightning leg.
    let tx0_outpoint = mercurylib::transfer::TxOutpoint {
        txid: funding_txid.to_string(),
        vout: funding_vout,
    };
    let (unspent, status) = verify_tx0_output_is_unspent_and_confirmed(
        &client_config.electrum_client,
        &tx0_outpoint,
        tx0_hex,
        network,
        client_config.confirmation_target,
    )
    .await?;
    if !unspent {
        return Err(anyhow!(
            "pre-pay census: funding output {funding_txid}:{funding_vout} is already spent — the conveyed ladder is dead"
        ));
    }
    if status != CoinStatus::CONFIRMED {
        return Err(anyhow!(
            "pre-pay census: funding output {funding_txid}:{funding_vout} is not confirmed to the client's target"
        ));
    }

    // [R1] EARN THE `flat_backups` NUMBER BEFORE SPENDING IT.
    //
    // The census `se_num_sigs == flat_backups + tiers + superseded` is the anti-theft linchpin: it is
    // EXACT equality precisely so that a co-signed state the sender kept hidden has no slot to hide
    // in. That only holds while every term is independently earned. The CLAIM path earns
    // `flat_backups` by running `validate_backup_chain_v2` over the conveyed chain ([S2]); the
    // pre-pay path used to hand the RAW `transfer_msg.backup_transactions.len()` straight into
    // `verify_bundle_bound`.
    //
    // Because that length was unvalidated, an attacker could simply PAD the vector — duplicate `tx1`s
    // (same prevout ⟹ still one group, so the funding-group and `.last()` checks are unchanged) or
    // any other filler — inflating `expected` by one per padded entry, and park a live, co-signed
    // rival state in the slack. Every remaining check passes and the SSP pays an IRREVERSIBLE
    // Lightning invoice for a coin the sender can still take back with the lower-CSV state it kept.
    // [D1]'s version floor closed the "declare protocol_version 0 and skip the binding" door; this is
    // the door the census itself exists for.
    //
    // The fix reuses the CLAIM path's validator — deliberately the same function, not a second,
    // weaker re-implementation — and then derives the count from the VALIDATED structure rather than
    // from the message. `validate_backup_chain_v2` rejects an empty chain, verifies each backup's
    // signature/sequence/locktime-sanity/reconstruction against the on-chain `tx0`, and enforces
    // INV-5 (`ladder_decrements_by_interval`): consecutive locktimes must fall by EXACTLY `interval`.
    // INV-5 is what makes padding structurally impossible — a duplicate decrements by 0, an inserted
    // filler by something other than `interval` — and it also rejects an inverted ladder whose stale
    // sender-paying backup would mature first.
    let current_fee_rate_sats_per_byte = if info_config.fee_rate_sats_per_byte > client_config.max_fee_rate {
        client_config.max_fee_rate
    } else {
        info_config.fee_rate_sats_per_byte
    };
    mercurylib::transfer::receiver::validate_backup_chain_v2(
        backup_group,
        tx0_hex,
        blockheight,
        client_config.fee_rate_tolerance,
        current_fee_rate_sats_per_byte,
        info_config.initlock,
        info_config.interval,
    )
    .map_err(|e| {
        anyhow!(
            "pre-pay census: the conveyed backup chain is not structurally valid, so its length cannot be counted into the census: {e}"
        )
    })?;

    let authority = crate::tesr::coin_authority_from_tx0(
        &transfer_msg.statechain_id,
        funding_txid,
        funding_vout,
        tx0_hex,
        info.aggregate_pubkey.clone(),
    )?;
    // [P0-3] EXIT-CHAIN LENGTH CAP on the FLAT lane. `rollover` pushes a level per exhausted epoch
    // with no bound but the state-rung floor, so a conveyed root ladder is `1 + 2·levels.len()`
    // transactions and nothing here has ever bounded that number. Both terms are receiver-derived:
    // `initlock` from this wallet's own `/info/config` fetch and the schedule from its own network
    // preset — never `bundle.params`, which is conveyed.
    //
    // Refusing to ACCEPT is safe; this is not reachable from any exit path.
    //
    // [C-1] And the conveyed schedule is BOUND rather than merely unused: `crate::tesr::cap_schedule`
    // returns this wallet's own preset and refuses — naming the field that disagreed — a ladder whose
    // declared schedule contradicts it. Silently ignoring the field would close this cap and leave
    // `verify_bundle_bound` below reading that same contradicted field for every CSV band.
    let cap_authority = crate::tesr::cap_schedule(network, bundle.params)?;
    debug_assert_eq!(
        cap_authority,
        mercurylib::tesr::TesrParams::for_network(network),
        "the cap authority must be the RECEIVER's preset, never the conveyed schedule"
    );
    crate::tesr::enforce_exit_chain_length(
        "conveyed root ladder",
        bundle.exit_tiers().len(),
        cap_authority,
        info_config.initlock,
    )?;
    // `flat_backups` is now the length of the chain that JUST passed structural validation — the same
    // number the claim path uses — not the sender-declared vector length.
    crate::tesr::verify_bundle_bound(
        &bundle,
        info.num_sigs,
        backup_group.len() as u32,
        &authority,
    )
}

/// [D1/D3] PRE-PAY census of an in-ladder-split CHILD conveyance. Same fail-closed-by-construction
/// shape as [`prepay_flat_census`]; returns the child's census-bound exit value on success, which the
/// caller may then use as the reported amount (a bundle-derived value may ONLY override a trusted one
/// once the bundle has been bound to the coin the payer latched).
async fn prepay_child_census(
    client_config: &ClientConfig,
    wallet_name: &str,
    network: &str,
    my_backup: &str,
    transfer_msg: &mercurylib::transfer::TransferMsg,
    cb_json: &str,
) -> Result<u64> {
    if transfer_msg.protocol_version < MIN_PREPAY_CHILD_PROTOCOL_VERSION {
        return Err(anyhow!(
            "pre-pay census: a child bundle was conveyed under protocol_version {} but child conveyances are >= {} — refusing",
            transfer_msg.protocol_version,
            MIN_PREPAY_CHILD_PROTOCOL_VERSION
        ));
    }
    let cb: crate::tesr::ChildTesrBundle = serde_json::from_str(cb_json)
        .map_err(|e| anyhow!("pre-pay census: malformed child TES-R bundle: {e}"))?;

    // [D3] BIND THE CHILD LANE TO THE LATCHED COIN. `verify_conveyed_child` anchors the bundle to the
    // parent's on-chain `F` and to the coordinator's aggregate for `cb.child_statechain_id` — but
    // NOTHING tied that id to the id the payer latched the swap to. So an attacker could latch sid `X`
    // and convey a fully-valid bundle for an unrelated sid `Y` it genuinely owns: every check inside
    // `verify_conveyed_child` passes, the census reports `true`, and the reported amount is overridden
    // by `Y`'s value — while the Lightning latch, and therefore the coin the payer actually acquires,
    // is `X`. The sender-declared statechain id in the message IS the latched id (the coordinator keys
    // the mailbox by it), so equality with the bundle's child id is the missing binding.
    if cb.child_statechain_id != transfer_msg.statechain_id {
        return Err(anyhow!(
            "pre-pay census: the conveyed child bundle is for statechain id {} but the latched transfer is {} — the census (and the value it returns) would describe a different coin",
            cb.child_statechain_id,
            transfer_msg.statechain_id
        ));
    }

    // [D3] ALREADY-ADOPTED. The claim path FAILS validation for a child this wallet has already adopted
    // (`get_msg_addr` is non-destructive, so a claimed child's message keeps being re-served). Without
    // the same check here a stale, already-owned child keeps re-appearing as a payable pending transfer,
    // so a replayed latch could make the payer pay a second time for a coin it already holds.
    if crate::tesr::load_child(client_config, wallet_name, &cb.child_statechain_id)
        .await?
        .is_some()
    {
        return Err(anyhow!(
            "pre-pay census: split child {} is already adopted by this wallet — not a payable pending transfer",
            cb.child_statechain_id
        ));
    }

    // [D3] LIVENESS. `verify_conveyed_child` reads the parent's `F` only to recover its scriptPubKey and
    // deliberately leaves unspent/confirmed to the caller. The flat lane checks exactly this; the child
    // lane must too, or a payer can be handed a child whose entire exit chain hangs off a funding output
    // that has already been spent out from under it.
    let f_hex = get_tx0(&client_config.electrum_client, &cb.parent.f_txid).await?;
    let f_outpoint = mercurylib::transfer::TxOutpoint {
        txid: cb.parent.f_txid.clone(),
        vout: cb.parent.f_vout,
    };
    let (unspent, status) = verify_tx0_output_is_unspent_and_confirmed(
        &client_config.electrum_client,
        &f_outpoint,
        &f_hex,
        network,
        client_config.confirmation_target,
    )
    .await?;
    if !unspent {
        return Err(anyhow!(
            "pre-pay census: the child's parent funding output {}:{} is already spent — the child's exit chain is dead",
            cb.parent.f_txid,
            cb.parent.f_vout
        ));
    }
    if status != CoinStatus::CONFIRMED {
        return Err(anyhow!(
            "pre-pay census: the child's parent funding output {}:{} is not confirmed to the client's target",
            cb.parent.f_txid,
            cb.parent.f_vout
        ));
    }

    // Model A for the child lane: `verify_conveyed_child` requires the final child state to pay
    // `my_backup` — the prospective owner's own key ([D2] for this lane).
    crate::tesr::verify_conveyed_child(client_config, my_backup, &cb).await
}

pub async fn peek_pending_transfers(
    client_config: &ClientConfig,
    wallet_name: &str,
) -> Result<Vec<PendingTransferInfo>> {
    let wallet = get_wallet(&client_config.pool, wallet_name).await?;

    // [R1] The pre-pay census now runs the CLAIM path's structural backup-chain validation before it
    // trusts the chain's length, so it needs the same two inputs the claim path uses: the SE's
    // lock/interval/fee-rate config and the current tip. Both are fetched ONCE here, and a failure to
    // read either is fatal to the whole peek — a pre-pay gate that cannot validate must not report
    // anything as payable (fail closed).
    let info_config = utils::info_config(client_config).await?;
    let blockheight = client_config
        .electrum_client
        .block_headers_subscribe_raw()?
        .height as u32;

    // Map each auth pubkey to a private key that can decrypt messages addressed to it.
    let mut privkey_by_pubkey: HashMap<String, String> = HashMap::new();
    for coin in wallet.coins.iter() {
        privkey_by_pubkey
            .entry(coin.auth_pubkey.clone())
            .or_insert_with(|| coin.auth_privkey.clone());
    }

    let mut out: Vec<PendingTransferInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (auth_pubkey, auth_privkey) in privkey_by_pubkey.iter() {
        let enc_messages = match get_msg_addr(auth_pubkey, client_config).await {
            std::result::Result::Ok(m) => m,
            Err(_) => continue,
        };
        for enc_message in enc_messages {
            let transfer_msg = match mercurylib::transfer::receiver::decrypt_transfer_msg(
                &enc_message,
                auth_privkey,
            ) {
                std::result::Result::Ok(m) => m,
                Err(_) => continue, // not addressed to us / undecryptable
            };
            if !seen.insert(transfer_msg.statechain_id.clone()) {
                continue;
            }
            // Amount from the funding tx0 (index-0 backup group), read on-chain or from the branch.
            // [R3] The grouping is fallible on attacker-controlled input; a malformed backup vector
            // used to PANIC the process here. A message we cannot even parse is not a payable
            // pending transfer, so drop it entirely rather than surfacing it with a false amount.
            let groups = match split_backup_transactions(&transfer_msg.backup_transactions) {
                std::result::Result::Ok(g) => g,
                Err(_) => continue,
            };
            let mut funding_txid = String::new();
            let mut funding_vout = 0u32;
            // [C-1] The coin's funding transaction AS READ FROM THE CHAIN, kept only when the funding
            // is genuinely on-chain (never a sender-supplied un-broadcast branch tx). This is the
            // authority the conveyed ladder is bound to below — without it the pre-pay census would
            // check a bundle against its own fields.
            let mut onchain_tx0: Option<String> = None;
            let amount = if let Some(first_group) = groups.first() {
                match mercurylib::transfer::receiver::get_tx0_outpoint(first_group) {
                    std::result::Result::Ok(tx0_outpoint) => {
                        funding_txid = tx0_outpoint.txid.clone();
                        funding_vout = tx0_outpoint.vout;
                        match get_tx0_or_branch(
                            &client_config.electrum_client,
                            &tx0_outpoint.txid,
                            &transfer_msg.branch_txs,
                        )
                        .await
                        {
                            std::result::Result::Ok((tx0_hex, funding_from_branch)) => {
                                if !funding_from_branch {
                                    onchain_tx0 = Some(tx0_hex.clone());
                                }
                                // SECURITY (audit [3]): a branch-derived amount is
                                // attacker-controlled — the blind SE co-signs ANY leaf output value,
                                // so an un-broadcast sub-coin's funding tx can claim any amount. A
                                // caller that gates a decision on this amount (the SSP pre-payment
                                // value gate) MUST NOT trust it until the branch is validated. Run
                                // the SAME branch + terminal-ancestor checks the claim path runs; on
                                // ANY failure report amount 0 so the value gate rejects it. An
                                // on-chain (non-branch) funding tx0 is already authoritative.
                                let trusted = if funding_from_branch {
                                    validate_branch(
                                        &client_config.electrum_client,
                                        &transfer_msg.branch_txs,
                                        &wallet.network,
                                        client_config.confirmation_target,
                                    )
                                    .await
                                    .is_ok()
                                        && match required_terminal_ancestors(&transfer_msg.branch_txs)
                                        {
                                            std::result::Result::Ok(req) => verify_terminal_parents(
                                                client_config,
                                                &transfer_msg.terminal_parents,
                                                req,
                                            )
                                            .await
                                            .is_ok(),
                                            Err(_) => false,
                                        }
                                } else {
                                    true
                                };
                                if trusted {
                                    mercurylib::transfer::receiver::get_amount_from_tx0(
                                        &tx0_hex,
                                        &tx0_outpoint,
                                    )
                                    .unwrap_or(0)
                                } else {
                                    0
                                }
                            }
                            Err(_) => 0,
                        }
                    }
                    Err(_) => 0,
                }
            } else {
                0
            };
            // Carry the coin's RGB consignment envelope (if any) + its witness outpoint so a caller
            // can validate the COLORED value pre-payment too (audit [4]); the sats `amount` above is
            // now branch-validated (audit [3]).
            let rgb_consignment = transfer_msg
                .backup_transactions
                .iter()
                .find_map(|b| b.rgb_consignment.clone());
            // PRE-PAY CENSUS (LIGHTNING.md §2/§2b). A paying party (the SSP) must validate a conveyed
            // coin BEFORE the irreversible Lightning leg. Two shapes:
            //   * a flat TES-R ladder (`protocol_version >= 2`) → `prepay_flat_census`:
            //     `verify_bundle_bound` (`num_sigs == flat_backups + tiers` vs the LIVE enclave
            //     sig-count, catching a hidden lower-CSV state that would out-race the conveyed S'
            //     after payment, PLUS the [C-1] coin binding) and the [D2] Model-A owner-exit binding;
            //   * an in-ladder-split CHILD bundle (non-exact PAY, `protocol_version >= 3`) →
            //     `prepay_child_census`: bound to the latched sid, not already adopted, live parent
            //     root, then `verify_conveyed_child` (child pays THIS wallet's key + parent/child
            //     census + terminality). Only THEN is its returned value the trustworthy piece amount
            //     (value-binding fix) that OVERRIDES the branch-derived sats `amount` (a child has no
            //     on-chain funding to read). The receiver runs the same census at claim time; this
            //     hoists it ahead of pay.
            //
            // [D1] FAIL CLOSED ON THE VERSION ITSELF. `protocol_version` is a SENDER-DECLARED field, so
            // dispatching on it and letting the fall-through arm report `true` meant an attacker just
            // declared `protocol_version = 0`: no binding ran, yet `ladder_census_ok` was reported TRUE
            // on the exact path that authorises an irreversible Lightning payment. Every arm below now
            // ends in a census that must SUCCEED; there is no "trivially ok" coin shape here.
            //
            // The prospective owner's own seed-derived backup address — this wallet is the payer, and
            // the coin is addressed to `auth_pubkey`, so the coin slot holding that auth key carries the
            // key the ladder must exit to ([D2]/[D3], the Model-A gate the claim path enforces). All
            // coins sharing an auth pubkey are the same wallet slot and so share `user_pubkey`, which is
            // the only input to this address — the claim path derives it identically. `None` (no such
            // slot) is a rejection, not a bypass.
            let my_backup = wallet
                .coins
                .iter()
                .find(|c| &c.auth_pubkey == auth_pubkey)
                .and_then(|c| {
                    mercurylib::transaction::get_user_backup_address(c, wallet.network.clone()).ok()
                });
            let (ladder_census_ok, child_amount, ladder_census_refusal) =
                match (&transfer_msg.child_tesr_bundle, my_backup.as_deref()) {
                    // No derivable owner key ⟹ nothing to bind the exit to ⟹ refuse.
                    (_, None) => (
                        false,
                        None,
                        Some(
                            "no owner backup address is derivable for this auth key, so the ladder's \
                             exit cannot be bound to us"
                                .to_string(),
                        ),
                    ),
                    (Some(cb_json), Some(bk)) => {
                        match prepay_child_census(
                            client_config,
                            wallet_name,
                            &wallet.network,
                            bk,
                            &transfer_msg,
                            cb_json,
                        )
                        .await
                        {
                            // The child value may override the branch-derived `amount` ONLY here, i.e.
                            // only once the bundle has been bound to the latched sid and censused.
                            std::result::Result::Ok(v) => (true, Some(v), None),
                            Err(e) => (false, None, Some(format!("child census: {e:#}"))),
                        }
                    }
                    (None, Some(bk)) => match prepay_flat_census(
                        client_config,
                        &wallet.network,
                        bk,
                        &transfer_msg,
                        &funding_txid,
                        funding_vout,
                        onchain_tx0.as_ref(),
                        &groups,
                        &info_config,
                        blockheight,
                    )
                    .await
                    {
                        std::result::Result::Ok(()) => (true, None, None),
                        Err(e) => (false, None, Some(format!("flat census: {e:#}"))),
                    },
                };
            let amount = child_amount.unwrap_or(amount);
            out.push(PendingTransferInfo {
                statechain_id: transfer_msg.statechain_id,
                recipient_auth_pub_key: auth_pubkey.clone(),
                encrypted_transfer_msg: enc_message.clone(),
                amount,
                rgb_consignment,
                funding_txid,
                funding_vout,
                branch_txs: transfer_msg.branch_txs.clone(),
                // [P3] Derived from the bundle the message already carries. Best-effort by design:
                // a bundle that will not parse, or is plain, yields an EMPTY list — and an empty
                // list is what the SSP refuses on, so a malformed child cannot become a payable one
                // by failing to describe itself.
                child_witness_txids: transfer_msg
                    .child_tesr_bundle
                    .as_deref()
                    .and_then(|j| serde_json::from_str::<crate::tesr::ChildTesrBundle>(j).ok())
                    .filter(|cb| cb.is_colored())
                    .and_then(|cb| cb.colored_child_txids().ok())
                    .unwrap_or_default(),
                ladder_census_ok,
                ladder_census_refusal,
            });
        }
    }
    Ok(out)
}

/// Group a conveyed backup vector by the outpoint each backup spends, preserving first-appearance
/// order and sorting each group by `tx_n`.
///
/// [R3] FALLIBLE BY DESIGN. This used to `.expect("Valid outpoint")` on `get_previous_outpoint`,
/// which returns `Err` for any tx that fails to deserialise, has more than one input, or has more
/// than one non-OP_RETURN output (`lib/src/wallet/mod.rs`). Every input here is ATTACKER-CONTROLLED —
/// it is the decrypted transfer message — and this function is reached from BOTH acceptance paths
/// (`validate_encrypted_message` at claim time and `peek_pending_transfers` at pre-pay time), so a
/// single crafted backup tx aborted the whole wallet process: a remote, unauthenticated DoS. The
/// error is propagated instead, and both callers treat it as a rejection (fail closed).
pub fn split_backup_transactions(backup_transactions: &Vec<BackupTx>) -> Result<Vec<Vec<BackupTx>>> {
    // HashMap to store grouped transactions
    let mut grouped_txs: HashMap<(String, u32), Vec<BackupTx>> = HashMap::new();

    // Vector to keep track of order of appearance of outpoints
    let mut order_of_appearance: Vec<(String, u32)> = Vec::new();
    // HashSet to track which outpoints we've seen
    let mut seen_outpoints: HashSet<(String, u32)> = HashSet::new();

    // Process each transaction
    for tx in backup_transactions {
        // Get the outpoint for this transaction
        let outpoint = get_previous_outpoint(&tx).map_err(|e| {
            anyhow!("malformed backup transaction in the conveyed vector (cannot read its previous outpoint): {e}")
        })?;

        // Create a key tuple from txid and vout
        let key = (outpoint.txid, outpoint.vout);
        
        // If we haven't seen this outpoint before, record its order
        if seen_outpoints.insert(key.clone()) {
            order_of_appearance.push(key.clone());
        }
        
        // Add the transaction to its group
        grouped_txs.entry(key).or_insert_with(Vec::new).push(tx.clone());
    }
    
    // Create result vector maintaining order of first appearance
    let mut result: Vec<Vec<BackupTx>> = Vec::with_capacity(order_of_appearance.len());
    
    // Add vectors to result in order of first appearance
    for key in order_of_appearance {
        if let Some(mut transactions) = grouped_txs.remove(&key) {
            // Sort each group by tx_n
            transactions.sort_by_key(|tx| tx.tx_n);
            result.push(transactions);
        }
    }

    std::result::Result::Ok(result)
}

/// Verify every structural ancestor node is TERMINAL at the SE (its spend budget is exhausted), so
/// the sender cannot double-spend a parent and invalidate this sub-coin's branch. Queries
/// `GET /statechain/spend_budget/<id>` per parent and requires `terminal == true`.
/// INV-20 hardening: a branch-funded sub-coin must name at least one terminal ancestor per branch
/// hop. `branch_len` is the number of un-broadcast txs in the exit branch (each spends a parent
/// node; a combine spends several, so the true ancestor count is `>= branch_len`). The receiver
/// therefore requires `n_parents >= max(branch_len, 1)` — an empty or short list means the sender
/// omitted an ancestor it could still double-spend, so the sub-coin is refused. `.max(1)` guards the
/// degenerate `branch_len == 0` call (this fn is only reached when funding_from_branch, i.e. len>=1).
pub(crate) fn terminal_parents_sufficient(n_parents: usize, required_ancestors: usize) -> bool {
    n_parents >= required_ancestors.max(1)
}

/// The number of terminal ancestors an exit branch MUST name: the total number of structural
/// inputs it consumes — i.e. `Σ inputs` over all branch txs. Every input of every branch tx spends
/// a statechain node (an on-chain root or an intra-branch sub-coin) that could be double-spent to
/// invalidate the branch unless it is terminal at the SE, so each must be named and proven terminal.
///
/// For a linear split chain this equals the number of hops (each split tx has exactly one input),
/// so it is a no-op there. For a COMBINE tx it is the input count `N` — closing the hole where the
/// old per-hop count (`branch_len`) required only ONE terminal ancestor for an N-input combine,
/// letting a sender combine a terminal carrier with `N-1` non-terminal, double-spendable ones.
/// Reject an exit branch that is not a TREE (D1): every outpoint it consumes must be spent by
/// exactly ONE branch input. A repeated prevout means two branch spends conflict on-chain — only one
/// can confirm — so the branch is un-broadcastable and any coin it funds is unexitable. See
/// `validate_branch` for why per-input script/value checks miss this.
fn reject_non_tree_branch(txs: &[bitcoin::Transaction]) -> Result<()> {
    let mut consumed: HashSet<bitcoin::OutPoint> = HashSet::new();
    for tx in txs {
        for input in &tx.input {
            if !consumed.insert(input.previous_output) {
                return Err(anyhow!(
                    "exit branch consumes outpoint {} more than once — non-tree branch / internal double-spend; rejecting (it could never confirm on-chain)",
                    input.previous_output
                ));
            }
        }
    }
    std::result::Result::Ok(())
}

pub(crate) fn required_terminal_ancestors(branch_txs: &[String]) -> Result<usize> {
    let mut total = 0usize;
    for tx_hex in branch_txs {
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(tx_hex)?)?;
        total += tx.input.len();
    }
    std::result::Result::Ok(total)
}

async fn verify_terminal_parents(client_config: &ClientConfig, parents: &[String], required_ancestors: usize) -> Result<()> {
    // A branch-funded sub-coin ALWAYS has structural ancestors: one per structural INPUT the branch
    // consumes (a split tx spends ONE parent; a combine tx spends N). If the sender names FEWER
    // ancestors than the branch has inputs, it is hiding one it could still double-spend — the
    // receiver must not trust the sender to enumerate its own parents. An empty list is the
    // degenerate case of this and was previously accepted (the bug): reject it. Terminality here is
    // BUDGET-based, not single_use: SDK sub-coins are opened `single_use=false` (get_deposit_address
    // → deposit.rs; see the sibling comment at validate_branch), so an ancestor is terminal only
    // because the SDK set its spend budget to 1 before co-signing the split/combine (IVL-REQ-7). This
    // check forces the sender to name every structural input and prove each terminal at the SE,
    // INCLUDING every input of a multi-input combine.
    if !terminal_parents_sufficient(parents.len(), required_ancestors) {
        return Err(anyhow!(
            "off-chain sub-coin names {} terminal ancestor(s) but its exit branch consumes {} structural input(s) — refusing (the sender may be hiding a non-terminal, double-spendable ancestor; a combine of N carriers needs all N named + terminal)",
            parents.len(),
            required_ancestors
        ));
    }
    let client = client_config.get_reqwest_client()?;
    for parent_id in parents {
        let url = format!(
            "{}/statechain/spend_budget/{}",
            client_config.statechain_entity, parent_id
        );
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "could not query terminal state of parent {parent_id}: {}",
                resp.status()
            ));
        }
        let v: serde_json::Value = resp.json().await?;
        let terminal = v.get("terminal").and_then(|t| t.as_bool()).unwrap_or(false);
        if !terminal {
            return Err(anyhow!(
                "structural parent {parent_id} is NOT terminal at the SE — rejecting sub-coin (the sender could still double-spend it)"
            ));
        }
    }
    Ok(())
}

async fn validate_encrypted_message(client_config: &ClientConfig, coin: &Coin, enc_message: &str, network: &str, wallet_name: &str, info_config: &InfoConfig, blockheight: u32) -> Result<()> {

    let client_auth_key = coin.auth_privkey.clone();
    let new_user_pubkey = coin.user_pubkey.clone();

    let transfer_msg = mercurylib::transfer::receiver::decrypt_transfer_msg(enc_message, &client_auth_key)?;

    // [in-ladder split] A split-child payment carries the child's exit bundle (and, from
    // `protocol_version >= 4`, the key-handover material) but NO backup ladder. Verify the bundle
    // against authoritative on-chain + SE values (verify_child_bundle: parent F on-chain, parent+child
    // terminal, child pays THIS coin's key) and skip the backup-chain checks below (there are
    // none to validate).
    if let Some(cb_json) = &transfer_msg.child_tesr_bundle {
        // [R4] VERSION FLOOR ON THE CLAIM PATH'S CHILD LANE, mirroring `prepay_child_census`. A
        // `child_tesr_bundle` attached to a message declaring a version below the child floor is a
        // version/payload mismatch: the sender is asking to be processed by rules that predate the
        // child lane while shipping child material. Refuse rather than guess.
        if transfer_msg.protocol_version < MIN_PREPAY_CHILD_PROTOCOL_VERSION {
            return Err(anyhow::anyhow!(
                "a child bundle was conveyed under protocol_version {} but child conveyances are >= {} — refusing",
                transfer_msg.protocol_version,
                MIN_PREPAY_CHILD_PROTOCOL_VERSION
            ));
        }
        let cb: crate::tesr::ChildTesrBundle = serde_json::from_str(cb_json)
            .map_err(|e| anyhow::anyhow!("malformed child TES-R bundle: {e}"))?;
        // [R4] BIND THE BUNDLE TO THE CONVEYED SLOT, mirroring [D3] on the pre-pay path. The
        // coordinator keys the mailbox by `transfer_msg.statechain_id`, so THAT is the slot this
        // message conveys; `cb.child_statechain_id` is the slot the bundle describes and the slot
        // `process_encrypted_message` then adopts, unlocks and completes the key handover on. Nothing
        // forced them to be the same id. `verify_conveyed_child` is no help — it validates the bundle
        // against `cb.child_statechain_id` throughout, so a bundle for an unrelated child the sender
        // genuinely owns passes every check while the transfer, the unlock and the handover run
        // against a different coin. (The `protocol_version >= 4` handover signature binds the
        // recipient over the child's funding outpoint, which mitigates the v4 shape — but v3 carries
        // no such signature at all, and a mitigation is not the binding.) The sender already enforces
        // this equality in `convey_child_bundle`; the receiver must not take it on trust.
        if cb.child_statechain_id != transfer_msg.statechain_id {
            return Err(anyhow::anyhow!(
                "the conveyed child bundle is for statechain id {} but the transfer message conveys {} — refusing (adoption would describe a different coin)",
                cb.child_statechain_id,
                transfer_msg.statechain_id
            ));
        }
        // Idempotency (mirrors the flat-transfer pattern where a re-received coin fails validation and
        // is skipped): if this child is already adopted, FAIL validation so the receive loop skips it
        // rather than booking a duplicate. get_msg_addr is non-destructive, so the message is re-served.
        if crate::tesr::load_child(client_config, wallet_name, &cb.child_statechain_id).await?.is_some() {
            return Err(anyhow::anyhow!("split child {} already adopted", cb.child_statechain_id));
        }
        let my_backup = mercurylib::transaction::get_user_backup_address(coin, network.to_string())
            .map_err(|_| anyhow::anyhow!("cannot derive the receiver's backup address"))?;
        crate::tesr::verify_conveyed_child(client_config, &my_backup, &cb).await?;

        // A handover-carrying conveyance must also prove the SENDER authorised THIS recipient over the
        // child's funding outpoint — the same binding the flat lane checks. Without it a conveyance
        // could be replayed toward a different receiver key.
        if transfer_msg.protocol_version >= 4 {
            use bitcoin::consensus::deserialize;
            let funding_hex = cb
                .ancestors
                .last()
                .map(|a| a.state.signed_tx.clone())
                .unwrap_or_else(|| cb.parent.current().state.signed_tx.clone());
            let sp_tx: bitcoin::Transaction = deserialize(&hex::decode(&funding_hex)?)?;
            let sp_outpoint = mercurylib::transfer::TxOutpoint {
                txid: sp_tx.txid().to_string(),
                vout: cb.sp_vout,
            };
            if !mercurylib::transfer::receiver::verify_transfer_signature(
                &coin.user_pubkey,
                &sp_outpoint,
                &transfer_msg,
            )? {
                return Err(anyhow::anyhow!(
                    "invalid transfer signature on the conveyed child {}",
                    cb.child_statechain_id
                ));
            }
        }
        return Ok(());
    }

    // ---------------------------------------------------------------------------------------------
    // FLAT (laddered) LANE. Everything below this point lives inside the per-group loop, so it all
    // depends on there being at least one group — and on the message declaring a version whose rules
    // this function actually implements. Both are checked HERE, before the loop, because a check
    // inside a loop that never runs is not a check.
    // ---------------------------------------------------------------------------------------------

    // [R2] AN EMPTY BACKUP VECTOR IS A REJECTION, NOT A VACUOUS PASS.
    //
    // With `backup_transactions` empty, `split_backup_transactions` yields ZERO groups, so the loop
    // below never executes: the transfer-signature check, the tx0-output-pubkey check, the
    // latest-backup-pays-to-me check, the ladder binding, the unspent/confirmed check and the
    // backup-chain validation are ALL skipped, and this function returns `Ok(())` having verified
    // literally nothing about the message. `process_encrypted_message` then runs its own loop, which
    // is likewise empty — so a coin whose validation "passed" is booked with no evidence at all. Fail
    // closed on the shape itself.
    if transfer_msg.backup_transactions.is_empty() {
        return Err(anyhow::anyhow!(
            "transfer message carries no backup transactions — every structural check on this path is per-group, so an empty vector would be accepted without verifying anything; rejecting"
        ));
    }

    // [R4] VERSION/PAYLOAD CONSISTENCY ON THE CLAIM PATH.
    //
    // `protocol_version` is SENDER-DECLARED, and it SELECTS THE RULES: at `>= 2` this path runs
    // `verify_bundle_bound` (the [C-1] coin binding + the exact tier census + the Model-A owner-exit
    // gate); below that it runs the legacy un-laddered rule. A sender must not be able to ship ladder
    // material and simultaneously ask to be judged by the rules that predate it — that is a payload
    // the declared version does not describe, and "which check runs" is exactly what an attacker
    // wants to choose. Refuse the mismatch outright.
    //
    // WHY NOT AN UNCONDITIONAL FLOOR OF 2 HERE (unlike [D1] on the pre-pay path). A pre-pay refusal
    // means "do not pay"; a claim-path refusal means "this coin can never be received". The
    // un-laddered shape is still produced by this codebase (a raw `deposit` with no `claim()`
    // ladder-establish pass, and — deliberately — any coin whose aggregate the coordinator does not
    // record, which `wallet.rs` leaves un-laddered precisely so it stays transferable), so an
    // unconditional floor BRICKS those coins rather than hardening anything. It is also not needed:
    // the `< 2` arm is not a weaker census, it is the un-laddered census, and it is EXACT equality
    // (`num_sigs == backup_transactions.len()`) over the enclave's live co-sign count. A laddered
    // coin's tiers each consume a co-sign slot, so for any coin carrying even one tier
    // `num_sigs > backup_transactions.len()` — permanently. Declaring version 0 to dodge the bound
    // verifier therefore cannot succeed on a laddered coin: it fails the count check instead. Padding
    // the backup vector to rebalance that count is defeated inside `validate_signature_scheme`, which
    // this arm runs in full (INV-5's exact-`interval` decrement rejects duplicates, and
    // `verify_blinded_musig_scheme` demands per-`tx_n` SE blinding data for every entry). The floor
    // that WOULD be load-bearing — the one guarding the ladder binding — is enforced below by the
    // `>= 2` arm itself, which requires the ladder to be present and bound.
    // **[D38/D16] EXACT-SET DISPATCH, before any shape-specific rule.** An unrecognised
    // `protocol_version` is refused outright rather than compared with `>=` — see
    // `admissible_shape`. This is what makes the uniffi FFI's silent stripping of the tag fail
    // CLOSED instead of downgrading a laddered conveyance to the un-laddered census.
    admissible_shape(transfer_msg.protocol_version)?;

    if transfer_msg.protocol_version < MIN_PREPAY_PROTOCOL_VERSION
        && transfer_msg.tesr_ladder.is_some()
    {
        return Err(anyhow::anyhow!(
            "transfer message declares protocol_version {} (below {}) yet carries a TES-R ladder — a payload the declared version does not describe; refusing rather than letting the sender choose which census runs",
            transfer_msg.protocol_version,
            MIN_PREPAY_PROTOCOL_VERSION
        ));
    }

    // ── [P1 / WP6(i), corrected by D35] THE FLAT-BACKUP SHAPE IS A FUNCTION OF THE DECLARED LANE ──
    //
    // A conveyed message carrying a `tesr_ladder` describes a coin whose exit is that ladder's tiers,
    // and the flat backups beside it are HOP backups over the funding outpoint. What may legitimately
    // ride on those rows depends on which ladder it is, and the two answers are opposite:
    //
    //   * a PLAIN ladder's tiers carry no RGB state transition, so exiting through them moves the
    //     sats and BURNS any allocation on the coin — RGB material on such a message is refused;
    //   * a COLOURED ladder's tiers each carry a valid transition, so the coin legitimately holds
    //     both. Its carrier envelope (`rgb_consignment`) is exactly what lets the next receiver bind
    //     the assignment to its OWN outpoint. What must be refused there instead is an OP_RETURN in
    //     a flat backup transaction: nothing binds that commitment's assignment, so it hands every
    //     ancestor a spend of `F` that re-assigns the allocation rather than merely voiding it.
    //
    // **The check used to be the union of the two**, keyed on "a ladder is present" and asserting
    // PLAIN in its prose without ever reading `is_colored()`. It therefore missed the coloured lane's
    // real defect (RGB-1) and refused the coloured lane's legitimate shape — which is the last hop of
    // the uncolourable-carrier rescue, where `accept_ladder` colours a legacy piece that still
    // carries its envelope. `verify_flat_backup_lane` states both rules from the lane it reads off
    // the structure. A message with no ladder at all is the un-laddered carrier lane and is not this
    // function's business: there the coloured backup IS the transfer vehicle.
    if let Some(ladder) = transfer_msg.tesr_ladder.as_ref() {
        let bundle: crate::tesr::TesrBundle = serde_json::from_str(ladder)
            .map_err(|e| anyhow::anyhow!("malformed TES-R ladder: {e}"))?;
        crate::tesr::verify_flat_backup_lane(&bundle, &transfer_msg.backup_transactions).map_err(
            |e| anyhow::anyhow!("refusing conveyance of {}: {e}", transfer_msg.statechain_id),
        )?;
    }

    let grouped_backup_transactions = split_backup_transactions(&transfer_msg.backup_transactions)?;

    for (index, backup_transactions) in grouped_backup_transactions.iter().enumerate() {
  
        let tx0_outpoint = mercurylib::transfer::receiver::get_tx0_outpoint(backup_transactions)?;

        let (tx0_hex, funding_from_branch) = get_tx0_or_branch(
            &client_config.electrum_client,
            &tx0_outpoint.txid,
            &transfer_msg.branch_txs,
        )
        .await?;
        if funding_from_branch {
            // Un-broadcast funding (off-chain sub-coin): the exit branch substitutes for the
            // on-chain checks — root must be on-chain/unspent/confirmed and every branch tx
            // consensus-valid.
            validate_branch(
                &client_config.electrum_client,
                &transfer_msg.branch_txs,
                network,
                client_config.confirmation_target,
            )
            .await?;
            // And every structural ancestor node must be TERMINAL at the SE (its spend budget is
            // exhausted), so the sender can no longer double-spend a parent and invalidate the
            // branch. This is the receiver's independent guarantee — it does not trust that the
            // sender set the budget.
            // Require one terminal ancestor per structural INPUT across the branch (Σ inputs), not
            // per hop — so a multi-input combine forces ALL its inputs to be named + terminal.
            let required_ancestors = required_terminal_ancestors(&transfer_msg.branch_txs)?;
            verify_terminal_parents(client_config, &transfer_msg.terminal_parents, required_ancestors).await?;
        }

        if index == 0 {
            let is_transfer_signature_valid = mercurylib::transfer::receiver::verify_transfer_signature(&new_user_pubkey, &tx0_outpoint, &transfer_msg)?; 

            if !is_transfer_signature_valid {
                return Err(anyhow::anyhow!("Invalid transfer signature".to_string()));
            }
        }

        let statechain_info = utils::get_statechain_info(&transfer_msg.statechain_id, &client_config).await?;

        if statechain_info.is_none() {
            return Err(anyhow::anyhow!("Statechain info not found".to_string()));
        }

        let statechain_info = statechain_info.unwrap();

        let is_tx0_output_pubkey_valid = mercurylib::transfer::receiver::validate_tx0_output_pubkey(&statechain_info.enclave_public_key, &transfer_msg, &tx0_outpoint, &tx0_hex, network)?;

        if !is_tx0_output_pubkey_valid {
            return Err(anyhow::anyhow!("Invalid tx0 output pubkey".to_string()));
        }

        let latest_backup_tx_pays_to_user_pubkey = mercurylib::transfer::receiver::verify_latest_backup_tx_pays_to_user_pubkey(&transfer_msg, &new_user_pubkey, network)?;

        if !latest_backup_tx_pays_to_user_pubkey {
            return Err(anyhow::anyhow!("Latest Backup Tx does not pay to the expected public key".to_string()));
        }

        if transfer_msg.protocol_version >= 2 {
            // Laddered (TES-R) coin: verify the conveyed exit ladder and its EXACT sig-count via the
            // R′ verifier, which enforces `se_num_sigs == flat_backups + tier_count` (no hidden
            // co-signed state) plus a valid exit chain — the laddered analogue of the un-laddered
            // backup-count linchpin below.
            //
            // [C-1] It is the BOUND verifier. `verify_bundle` alone checks the trigger against
            // `bundle.f_txid`/`f_vout` and the tier payees against `bundle.agg_address` — all fields
            // of the very bundle under test — so it proves internal consistency, not that the ladder
            // describes THIS coin. A sender could convey a self-consistent decoy ladder over an
            // attacker-controlled outpoint (owner_exit_address correctly set, tiers padded so the
            // census balances), have it accepted, and then spend the coin with the REAL trigger it
            // kept. The authority therefore comes from the coin: the funding outpoint validated on
            // this path, its on-chain value and aggregate scriptPubKey, and the coordinator's
            // recorded aggregate for the sid.
            if index != 0 {
                return Err(anyhow::anyhow!(
                    "laddered transfer carries more than one funding group — a ladder is rooted at exactly one funding UTXO"
                ));
            }
            if funding_from_branch {
                // A laddered coin rests on a CONFIRMED on-chain funding UTXO; if the funding came from
                // an un-broadcast branch tx there is no on-chain authority to bind the ladder to.
                return Err(anyhow::anyhow!(
                    "laddered transfer's funding UTXO is not on-chain — cannot bind the ladder to the coin"
                ));
            }
            let ladder = transfer_msg
                .tesr_ladder
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("laddered transfer is missing its TES-R ladder"))?;
            let bundle: crate::tesr::TesrBundle = serde_json::from_str(ladder)
                .map_err(|e| anyhow::anyhow!("malformed TES-R ladder: {e}"))?;
            let coin_authority = crate::tesr::coin_authority_from_tx0(
                &transfer_msg.statechain_id,
                &tx0_outpoint.txid,
                tx0_outpoint.vout,
                &tx0_hex,
                statechain_info.aggregate_pubkey.clone(),
            )?;
            // [P0-3] EXIT-CHAIN LENGTH CAP on the FLAT lane — the claim-time twin of the pre-pay
            // census's. See the note there; both terms are receiver-derived and this is admission
            // only, never reachable from an exit path. [C-1] The conveyed schedule is bound here too
            // — same reasoning, and this is the door a claim comes through when no pre-pay census ran.
            let cap_authority = crate::tesr::cap_schedule(network, bundle.params)?;
            debug_assert_eq!(
                cap_authority,
                mercurylib::tesr::TesrParams::for_network(network),
                "the cap authority must be the RECEIVER's preset, never the conveyed schedule"
            );
            crate::tesr::enforce_exit_chain_length(
                "conveyed root ladder",
                bundle.exit_tiers().len(),
                cap_authority,
                info_config.initlock,
            )?;
            // **[D38/D10 — B.7] VALIDATE BEFORE YOU COUNT.**
            //
            // The census is exact equality, `se_num_sigs == flat_backups + tiers + superseded`, and
            // the `flat_backups` term was `transfer_msg.backup_transactions.len()` — the length of a
            // vector the SENDER wrote, taken before anything had checked its structure. The
            // structural validation ran ~50 lines later, inside the per-group loop.
            //
            // INV-5 is what makes that length unforgeable: `ladder_decrements_by_interval` requires
            // consecutive locktimes to fall by EXACTLY `interval`, so a duplicate decrements by 0 and
            // an inserted filler by something else. Counting first means counting a vector that has
            // not yet met INV-5, and a padded vector inflates `expected` by one per padded entry —
            // absorbing a hidden co-signed rival state while the census still balances exactly.
            //
            // The pre-pay census already had this order and says so in its own comment. This is the
            // claim path catching up: the count now comes from a chain that has PASSED.
            mercurylib::transfer::receiver::validate_backup_chain_v2(
                &transfer_msg.backup_transactions,
                &tx0_hex,
                blockheight,
                client_config.fee_rate_tolerance,
                // Same clamp the later per-group validation applies; computed here because the
                // count now happens before that binding exists.
                if info_config.fee_rate_sats_per_byte > client_config.max_fee_rate {
                    client_config.max_fee_rate
                } else {
                    info_config.fee_rate_sats_per_byte
                },
                info_config.initlock,
                info_config.interval,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "the conveyed backup chain is not structurally valid, so its length cannot be \
                     counted into the census: {e:?}"
                )
            })?;
            crate::tesr::verify_bundle_bound(
                &bundle,
                statechain_info.num_sigs,
                transfer_msg.backup_transactions.len() as u32,
                &coin_authority,
            )?;
            // Model A fund-safety gate: the ladder's final state MUST exit to the RECEIVER's own
            // seed-derived key (P2TR of this coin's user_pubkey). Without this, a sender could set
            // owner_exit_address + pre-sign S' to pay a third party while still passing verify_bundle.
            let my_backup = mercurylib::transaction::get_user_backup_address(coin, network.to_string())
                .map_err(|_| anyhow::anyhow!("cannot derive the receiver's backup address"))?;
            if bundle.owner_exit_address != my_backup {
                return Err(anyhow::anyhow!(
                    "laddered transfer rejected: the conveyed ladder does not exit to the receiver's own key"
                ));
            }
        } else if statechain_info.num_sigs != transfer_msg.backup_transactions.len() as u32 {
            // [R4] The un-laddered census. EXACT equality against the enclave's live co-sign count is
            // what makes this arm safe to reach at all: every tier a laddered coin carries consumes a
            // co-sign slot, so a laddered coin can never satisfy it, and a sender cannot select this
            // arm to dodge `verify_bundle_bound` above. See the version/payload note before the loop.
            return Err(anyhow::anyhow!("num_sigs is not correct".to_string()));
        }

        if !funding_from_branch {
            let (is_tx0_output_unspent, _) = verify_tx0_output_is_unspent_and_confirmed(&client_config.electrum_client, &tx0_outpoint, &tx0_hex, &network, client_config.confirmation_target).await?;

            if !is_tx0_output_unspent {
                return Err(anyhow::anyhow!("tx0 output is spent or not confirmed".to_string()));
            }
        }

        let current_fee_rate_sats_per_byte = if info_config.fee_rate_sats_per_byte > client_config.max_fee_rate {
            client_config.max_fee_rate
        } else {
            info_config.fee_rate_sats_per_byte
        };

        // [S2] The conveyed flat backup chain (the signed-once backups every coin carries) MUST be
        // validated for BOTH coin shapes. This was previously gated to `protocol_version < 2` on the
        // reasoning that "a laddered coin does not use that chain" — which was WRONG: a laddered coin
        // still conveys those backups AND still feeds their COUNT into verify_bundle's anti-theft
        // equation above (`flat_backups = backup_transactions.len()`). Skipping this left that term
        // attacker-supplied and structurally unvalidated, so a sender could (a) pad the vector with
        // duplicate tx1s — same prevout ⟹ one group, first-by-tx_n and .last() unchanged — to inflate
        // `expected` and absorb a hidden co-signed state, or (b) invert the ladder, building the
        // receiver-paying backup at L+interval while retaining their own at L, so their stale backup
        // matures FIRST. `ladder_decrements_by_interval` (INV-5) is the only defence against (b) and it
        // lived solely in here. The gate was a regression introduced when the laddered shape was added
        // alongside the un-laddered one, which had always run this check; both shapes now run it.
        let previous_lock_time = if transfer_msg.protocol_version >= 2 {
            // Laddered: the tiers consume SE co-sign slots, so a backup's `tx_n` no longer aligns with the
            // SE's per-co-sign `statechain_info` index and the blinded-musig lookup would read a
            // TIER's blinding info. Run the structural chain validation, which keeps INV-5 — the
            // defence against both S2 attacks (duplicate padding and ladder inversion).
            mercurylib::transfer::receiver::validate_backup_chain_v2(
                backup_transactions,
                &tx0_hex,
                blockheight,
                client_config.fee_rate_tolerance,
                current_fee_rate_sats_per_byte,
                info_config.initlock,
                info_config.interval)
        } else {
            mercurylib::transfer::receiver::validate_signature_scheme(
                backup_transactions,
                &statechain_info,
                &tx0_hex,
                blockheight,
                client_config.fee_rate_tolerance,
                current_fee_rate_sats_per_byte,
                info_config.initlock,
                info_config.interval)
        };

        if previous_lock_time.is_err() {
            let error = previous_lock_time.err().unwrap();
            return Err(anyhow!("Signature scheme validation failed. Error {}", error.to_string()));
        }
    }

    Ok(())
}

async fn process_encrypted_message(client_config: &ClientConfig, coin: &mut Coin, enc_message: &str, network: &str, wallet_name: &str, activities: &mut Vec<Activity>) -> Result<MessageResult> {

    let mut transfer_receive_result = MessageResult {
        is_batch_locked: false,
        statechain_id: None,
        duplicated_coins: Vec::new(),
    };

    let client_auth_key = coin.auth_privkey.clone();

    let transfer_msg = mercurylib::transfer::receiver::decrypt_transfer_msg(enc_message, &client_auth_key)?;

    // [in-ladder split] Adopt a conveyed split child (already verified in validate_encrypted_message).
    // `protocol_version >= 4` carries the STANDARD key handover, so the receiver COMPLETES it here: the
    // SE rotates its share leaving the child aggregate `A_child` INVARIANT (the pre-signed child ladder
    // stays valid) and re-points auth to this wallet, which permanently locks the sender out. That makes
    // the child a FIRST-CLASS coin, not merely an exitable claim (docs/utexo/CHILDREN.md).
    // Version 3 is the legacy no-handover conveyance and is still adopted exit-only.
    if let Some(cb_json) = &transfer_msg.child_tesr_bundle {
        let cb: crate::tesr::ChildTesrBundle = serde_json::from_str(cb_json)
            .map_err(|e| anyhow::anyhow!("malformed child TES-R bundle: {e}"))?;
        // Idempotency: a prior claim already adopted this child (get_msg_addr is non-destructive, so
        // the message is re-served on every claim). Do nothing — booking again would duplicate the coin.
        if crate::tesr::load_child(client_config, wallet_name, &cb.child_statechain_id).await?.is_some() {
            return Ok(transfer_receive_result);
        }

        // SP.out[j] is the (un-broadcast) funding outpoint of the child.
        use bitcoin::consensus::deserialize;
        // The child's funding tx is the LAST segment above it: the parent's SP for a depth-1 child,
        // otherwise the deepest intermediate segment's state.
        let sp_hex = cb
            .ancestors
            .last()
            .map(|a| a.state.signed_tx.clone())
            .unwrap_or_else(|| cb.parent.current().state.signed_tx.clone());
        let sp_tx: bitcoin::Transaction = deserialize(&hex::decode(&sp_hex)?)?;
        let sp_txid = sp_tx.txid().to_string();
        let sp_out = sp_tx
            .output
            .get(cb.sp_vout as usize)
            .ok_or_else(|| anyhow::anyhow!("SP has no output {}", cb.sp_vout))?
            .clone();
        let sp_outpoint = mercurylib::transfer::TxOutpoint {
            txid: sp_txid.clone(),
            vout: cb.sp_vout,
        };

        // The SE's blinding factor for THIS child slot (x1_pub), needed to validate t1 and derive t2.
        let statechain_info =
            crate::utils::get_statechain_info(&cb.child_statechain_id, client_config)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no statechain info for child {}", cb.child_statechain_id))?;

        // Clear the RECEIVER-side lock BEFORE completing the handover (the flat lane does the same).
        // [non-exact LN RECEIVE, LIGHTNING.md §2b] For a latched conveyance this is what signals "the
        // receiver has claimed": once the owner (SSP) has also confirmed, both lock bits are false and
        // the SE releases the HODL preimage. The auth key has not rotated yet, so sign with this
        // wallet's own auth key. NOT best-effort any more — the handover below depends on it.
        let signed_for_unlock =
            mercurylib::transfer::receiver::sign_message(&cb.child_statechain_id, coin)?;
        unlock_statecoin(client_config, &cb.child_statechain_id, &signed_for_unlock, &coin.auth_pubkey)
            .await?;

        // Complete the key handover: /transfer/receiver rotates the SE share and the auth key.
        let payload = mercurylib::transfer::receiver::create_transfer_receiver_request_payload(
            &statechain_info,
            &transfer_msg,
            coin,
        )?;
        let server_public_key_hex = match send_transfer_receiver_request_payload(client_config, &payload).await {
            std::result::Result::Ok(res) => {
                // Batch-locked: return BEFORE any coin mutation and BEFORE persist_child, so the next
                // claim re-serves the message and adopts cleanly once the batch unlocks.
                if res.is_batch_locked {
                    return Ok(MessageResult {
                        is_batch_locked: true,
                        statechain_id: None,
                        duplicated_coins: Vec::new(),
                    });
                }
                res.server_pubkey
                    .ok_or_else(|| anyhow::anyhow!("transfer/receiver returned no server pubkey"))?
            }
            // Propagate UNCHANGED: re-wrapping in `anyhow!("Error: {}")` would erase the
            // `TransferWasCancelled` type the receive loop downcasts on, turning a cancelled
            // payment back into an indistinguishable "processing error".
            Err(err) => return Err(err),
        };

        // Passing the UN-BROADCAST SP as `tx0_hex` makes this REQUIRE that the rotated aggregate equals
        // `SP.out[j]`'s output key — i.e. proof that `A_child` is invariant, so every pre-signed child
        // tier is still valid under the new share split.
        let new_key_info = mercurylib::transfer::receiver::get_new_key_info(
            &server_public_key_hex,
            coin,
            &cb.child_statechain_id,
            &sp_outpoint,
            &sp_hex,
            network,
        )?;

        coin.server_pubkey = Some(server_public_key_hex);
        coin.aggregated_pubkey = Some(new_key_info.aggregate_pubkey);
        coin.aggregated_address = Some(new_key_info.aggregate_address);
        coin.statechain_id = Some(cb.child_statechain_id.clone());
        coin.signed_statechain_id = Some(new_key_info.signed_statechain_id.clone());
        coin.amount = Some(new_key_info.amount);
        coin.utxo_txid = Some(sp_txid.clone());
        coin.utxo_vout = Some(cb.sp_vout);
        // `locktime` stays None ON PURPOSE: a child exits by RELATIVE CSV, it has no absolute-locktime
        // backup. Setting Some(0) would make the coin permanently "near its floor", so every quote would
        // bill a phantom re-anchor and the maintenance pass would try to refresh it forever.
        coin.status = CoinStatus::CONFIRMED;

        // Persist the child bundle LAST — it is the adoption marker, so any failure above leaves the
        // message re-claimable rather than half-adopted.
        crate::tesr::persist_child(client_config, wallet_name, &cb).await?;

        activities.push(Activity {
            utxo: sp_txid,
            amount: new_key_info.amount,
            action: "Receive".to_string(),
            date: Utc::now().to_rfc3339(),
        });

        transfer_receive_result.statechain_id = Some(cb.child_statechain_id.clone());
        return Ok(transfer_receive_result);
    }

    // [R3] Fallible: a malformed conveyed backup vector is an error, not a panic. It cannot normally
    // reach here (`validate_encrypted_message` runs first and would have rejected it), so propagating
    // simply aborts processing this message.
    let grouped_backup_transactions = split_backup_transactions(&transfer_msg.backup_transactions)?;

    for (index, backup_transactions) in grouped_backup_transactions.iter().enumerate() {

        if index == 0 {

            let tx0_outpoint = mercurylib::transfer::receiver::get_tx0_outpoint(backup_transactions)?;
            let (tx0_hex, funding_from_branch) = get_tx0_or_branch(
                &client_config.electrum_client,
                &tx0_outpoint.txid,
                &transfer_msg.branch_txs,
            )
            .await?;

            let statechain_info = utils::get_statechain_info(&transfer_msg.statechain_id, &client_config).await?;   
            let statechain_info = statechain_info.unwrap();

            let tx0_status = if funding_from_branch {
                validate_branch(
                    &client_config.electrum_client,
                    &transfer_msg.branch_txs,
                    network,
                    client_config.confirmation_target,
                )
                .await?
            } else {
                let (_, s) = verify_tx0_output_is_unspent_and_confirmed(&client_config.electrum_client, &tx0_outpoint, &tx0_hex, &network, client_config.confirmation_target).await?;
                s
            };

            let backup_tx = backup_transactions.last().unwrap();

            let last_tx_lock_time = mercurylib::utils::get_blockheight(&backup_tx)?;

            let transfer_receiver_request_payload = mercurylib::transfer::receiver::create_transfer_receiver_request_payload(&statechain_info, &transfer_msg, &coin)?;

            // unlock the statecoin - it might be part of a batch

            // the pub_auth_key has not been updated yet in the server (it will be updated after the transfer/receive call)
            // So we need to manually sign the statechain_id with the client_auth_key
            let signed_statechain_id_for_unlock = mercurylib::transfer::receiver::sign_message(&transfer_msg.statechain_id, &coin)?;

            unlock_statecoin(&client_config, &transfer_msg.statechain_id, &signed_statechain_id_for_unlock, &coin.auth_pubkey).await?;

            let transfer_receiver_result = send_transfer_receiver_request_payload(&client_config, &transfer_receiver_request_payload).await;

            let server_public_key_hex = match transfer_receiver_result {
                std::result::Result::Ok(server_public_key_hex) => {
        
                    if server_public_key_hex.is_batch_locked {
                        return Ok(MessageResult {
                            is_batch_locked: true,
                            statechain_id: None,
                            duplicated_coins: Vec::new(),
                        });
                    }
        
                    server_public_key_hex.server_pubkey.unwrap()
                },
                // Propagate UNCHANGED — see the note on the child-bundle path above: the
                // `TransferWasCancelled` type must survive to the receive loop.
                Err(err) => {
                    return Err(err);
                }
            };

            let new_key_info = mercurylib::transfer::receiver::get_new_key_info(&server_public_key_hex, &coin, &transfer_msg.statechain_id, &tx0_outpoint, &tx0_hex, network)?;

            coin.server_pubkey = Some(server_public_key_hex);
            coin.aggregated_pubkey = Some(new_key_info.aggregate_pubkey);
            coin.aggregated_address = Some(new_key_info.aggregate_address);
            coin.statechain_id = Some(transfer_msg.statechain_id.clone());
            coin.signed_statechain_id = Some(new_key_info.signed_statechain_id.clone());
            coin.amount = Some(new_key_info.amount);
            coin.utxo_txid = Some(tx0_outpoint.txid.clone());
            coin.utxo_vout = Some(tx0_outpoint.vout);
            coin.locktime = Some(last_tx_lock_time);
            coin.status = tx0_status;

            let date = Utc::now(); // This will get the current date and time in UTC
            let iso_string = date.to_rfc3339(); // Converts the date to an ISO 8601 string

            let activity = Activity {
                utxo: tx0_outpoint.txid.clone(),
                amount: new_key_info.amount,
                action: "Receive".to_string(),
                date: iso_string
            };

            activities.push(activity);

            insert_or_update_backup_txs(&client_config.pool, wallet_name, &transfer_msg.statechain_id, &transfer_msg.backup_transactions).await?;

            // Persist the exit branch (if any) so unilateral exit can broadcast it before the
            // leaf backups. Stored under a derived key next to the coin's backups.
            if !transfer_msg.branch_txs.is_empty() {
                let branch: Vec<mercurylib::wallet::BackupTx> = transfer_msg
                    .branch_txs
                    .iter()
                    .enumerate()
                    .map(|(i, tx)| mercurylib::wallet::BackupTx {
                        tx_n: (i + 1) as u32,
                        tx: tx.clone(),
                        client_public_nonce: String::new(),
                        server_public_nonce: String::new(),
                        client_public_key: String::new(),
                        server_public_key: String::new(),
                        blinding_factor: String::new(),
                        rgb_consignment: None,
                        rgb_blinding: None,
                    })
                    .collect();
                insert_or_update_backup_txs(
                    &client_config.pool,
                    wallet_name,
                    &format!("branch-{}", transfer_msg.statechain_id),
                    &branch,
                )
                .await?;
            }

            // Persist the structural ancestor chain (the terminal_parents the sender named) under
            // "parents-<id>", one id per BackupTx.tx row — the same convention register_split_subcoins
            // uses on the sender side. This lets THIS receiver, if it later re-transfers the sub-coin
            // off-chain, pass on the FULL ancestor set (its grandparents included). Without it a
            // second off-chain hop would name too few ancestors and be rejected by the receiver's
            // terminal-parent count check (INV-20).
            if !transfer_msg.terminal_parents.is_empty() {
                let parents: Vec<mercurylib::wallet::BackupTx> = transfer_msg
                    .terminal_parents
                    .iter()
                    .enumerate()
                    .map(|(i, id)| mercurylib::wallet::BackupTx {
                        tx_n: (i + 1) as u32,
                        tx: id.clone(),
                        client_public_nonce: String::new(),
                        server_public_nonce: String::new(),
                        client_public_key: String::new(),
                        server_public_key: String::new(),
                        blinding_factor: String::new(),
                        rgb_consignment: None,
                        rgb_blinding: None,
                    })
                    .collect();
                insert_or_update_backup_txs(
                    &client_config.pool,
                    wallet_name,
                    &format!("parents-{}", transfer_msg.statechain_id),
                    &parents,
                )
                .await?;
            }

            transfer_receive_result.is_batch_locked = false;
            transfer_receive_result.statechain_id = Some(transfer_msg.statechain_id.clone());
        } else {

            let tx0_outpoint = mercurylib::transfer::receiver::get_tx0_outpoint(backup_transactions)?;
            let (tx0_hex, _) = get_tx0_or_branch(
                &client_config.electrum_client,
                &tx0_outpoint.txid,
                &transfer_msg.branch_txs,
            )
            .await?;

            let first_backup_tx = backup_transactions.first().unwrap();

            let tx_outpoint = get_previous_outpoint(&first_backup_tx)?;

            let amount = mercurylib::transfer::receiver::get_amount_from_tx0(&tx0_hex, &tx_outpoint)?;

            transfer_receive_result.duplicated_coins.push(DuplicatedCoinData {
                txid: tx_outpoint.txid,
                vout: tx_outpoint.vout,
                amount,
                index: index as u32,
            });
        }
    }

    // Model A adoption: the conveyed ladder was verified (validate_encrypted_message: verify_bundle +
    // exits-to-my-key). Persist it under the received coin's statechain_id so the receiver now OWNS a
    // complete, self-paying exit chain and can transfer/exit/renew it — no SE cooperation needed.
    if transfer_msg.protocol_version >= 2 {
        if let Some(ladder) = &transfer_msg.tesr_ladder {
            if let std::result::Result::Ok(bundle) = serde_json::from_str::<crate::tesr::TesrBundle>(ladder) {
                let _ = crate::tesr::persist(client_config, wallet_name, &bundle).await;
            }
        }
    }

    Ok(transfer_receive_result)
}

async fn get_tx0(electrum_client: &electrum_client::Client, tx0_txid: &str) -> Result<String> {

    let tx0_txid = Txid::from_str(tx0_txid)?;
    let tx_bytes = electrum_client.batch_transaction_get_raw(&[tx0_txid])?;

    if tx_bytes.len() == 0 {
        return Err(anyhow!("tx0 not found"));
    }

    let tx0_hex = hex::encode(&tx_bytes[0]);

    Ok(tx0_hex)
}

/// Resolve the funding tx of a coin: on-chain first, else from the transfer message's exit
/// branch (an off-chain split/combine sub-coin whose funding tx is un-broadcast). Returns the
/// funding tx hex and whether it came from the branch.
async fn get_tx0_or_branch(
    electrum_client: &electrum_client::Client,
    tx0_txid: &str,
    branch_txs: &[String],
) -> Result<(String, bool)> {
    if let std::result::Result::Ok(hex) = get_tx0(electrum_client, tx0_txid).await {
        return Ok((hex, false));
    }
    for tx_hex in branch_txs {
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(tx_hex)?)?;
        if tx.txid().to_string() == tx0_txid {
            return Ok((tx_hex.clone(), true));
        }
    }
    Err(anyhow!("funding tx {} not on-chain and not in the exit branch", tx0_txid))
}

/// Validate an exit branch: fully-signed txs, root-first, each spending its predecessor, with the
/// root spending an ON-CHAIN outpoint that must be unspent and confirmed. Script-verifies every
/// branch input (consensus rules), so a co-signed-but-invalid chain cannot be accepted. Returns
/// the coin status derived from the root confirmation depth.
async fn validate_branch(
    electrum_client: &electrum_client::Client,
    branch_txs: &[String],
    network: &str,
    confirmation_target: u32,
) -> Result<CoinStatus> {
    use bitcoin::OutPoint;
    use std::collections::HashMap;

    if branch_txs.is_empty() {
        return Err(anyhow!("empty exit branch"));
    }
    let mut txs: Vec<bitcoin::Transaction> = Vec::new();
    for tx_hex in branch_txs {
        txs.push(bitcoin::consensus::encode::deserialize(&hex::decode(tx_hex)?)?);
    }
    let branch_ids: std::collections::HashSet<String> =
        txs.iter().map(|t| t.txid().to_string()).collect();

    // The exit branch MUST be a TREE: every outpoint it consumes is spent by exactly one branch
    // input (D1). A non-tree branch — two branch txs (e.g. two inputs of a combine) spending the
    // SAME outpoint — is script-valid per-input yet UN-BROADCASTABLE as a whole: the two spends are
    // mutually exclusive on-chain, so a tx consuming both can never confirm. A token carrier is
    // `single_use=false`, so the SE re-signs it freely and a malicious sender can obtain two
    // conflicting co-signed spends of one carrier; without this guard the receiver books a CONFIRMED
    // coin that can never exit (fund stranding). Checked BEFORE prevout resolution because both
    // `tx.verify` and the per-tx value loop resolve a shared outpoint independently and would pass.
    reject_non_tree_branch(&txs)?;

    // Collect all prevouts: from earlier branch txs, or (for the root) from chain.
    let mut prevouts: HashMap<OutPoint, bitcoin::TxOut> = HashMap::new();
    let mut root_outpoint: Option<mercurylib::transfer::TxOutpoint> = None;
    for tx in &txs {
        for input in &tx.input {
            let prev_txid = input.previous_output.txid.to_string();
            if branch_ids.contains(&prev_txid) {
                let parent = txs.iter().find(|t| t.txid().to_string() == prev_txid).unwrap();
                let out = parent
                    .output
                    .get(input.previous_output.vout as usize)
                    .ok_or_else(|| anyhow!("branch link references missing output"))?;
                prevouts.insert(input.previous_output, out.clone());
            } else {
                // Root input: must be on-chain, unspent and confirmed.
                let root_hex = get_tx0(electrum_client, &prev_txid).await?;
                let root_tx: bitcoin::Transaction =
                    bitcoin::consensus::encode::deserialize(&hex::decode(&root_hex)?)?;
                let out = root_tx
                    .output
                    .get(input.previous_output.vout as usize)
                    .ok_or_else(|| anyhow!("branch root references missing output"))?;
                prevouts.insert(input.previous_output, out.clone());
                let outpoint = mercurylib::transfer::TxOutpoint {
                    txid: prev_txid.clone(),
                    vout: input.previous_output.vout,
                };
                let (unspent, status) = verify_tx0_output_is_unspent_and_confirmed(
                    electrum_client,
                    &outpoint,
                    &root_hex,
                    network,
                    confirmation_target,
                )
                .await?;
                if !unspent {
                    return Err(anyhow!("exit-branch root output is spent"));
                }
                if root_outpoint.is_none() {
                    root_outpoint = Some(outpoint);
                }
                if status != CoinStatus::CONFIRMED {
                    return Err(anyhow!("exit-branch root is not confirmed"));
                }
            }
        }
    }
    // INV-4 (audit [11]): a structural branch tx must be IMMEDIATELY broadcastable — no unreached
    // locktime. The blind SE never constrains a branch's nLockTime, so a malicious sender could ship
    // a branch tx with a far-future locktime; the receiver would book the coin CONFIRMED yet be
    // unable to exit, while the sender's matured parent/deposit backup later sweeps the shared root
    // outpoint back to themselves (total loss of received value). Reject any branch tx whose locktime
    // is not already satisfied at the receiver's current tip.
    let tip = electrum_client.block_headers_subscribe_raw()?.height as u32;
    for tx in &txs {
        let lock_time = tx.lock_time.to_consensus_u32();
        if lock_time > tip {
            return Err(anyhow!(
                "exit-branch tx {} has an unreached locktime {} (tip {}) — not immediately broadcastable, violates INV-4; rejecting",
                tx.txid(),
                lock_time,
                tip
            ));
        }
    }

    // Consensus-verify every branch tx against its prevouts (signatures + scripts) AND check value
    // conservation. `tx.verify` runs script/signature checks but NOT the fee rule, so a malicious
    // sender could hand the receiver a branch whose txs create value (Σ outputs > Σ inputs); those
    // scripts pass here but the network would reject the tx, leaving the receiver holding a coin it
    // can never exit on-chain while the sender keeps the real funds. Require a non-negative fee at
    // every hop so the whole branch is actually broadcastable.
    for tx in &txs {
        let in_value: u64 = tx
            .input
            .iter()
            .map(|i| prevouts.get(&i.previous_output).map(|o| o.value).unwrap_or(0))
            .sum();
        let out_value: u64 = tx.output.iter().map(|o| o.value).sum();
        if out_value > in_value {
            return Err(anyhow!(
                "exit-branch tx {} creates value (outputs {} sats > inputs {} sats) — not broadcastable, so the branch is unexitable; rejecting",
                tx.txid(),
                out_value,
                in_value
            ));
        }
        tx.verify(|op| prevouts.get(op).cloned())
            .map_err(|e| anyhow!("exit-branch tx {} fails script verification: {e}", tx.txid()))?;
    }
    Ok(CoinStatus::CONFIRMED)
}

async fn verify_tx0_output_is_unspent_and_confirmed(electrum_client: &electrum_client::Client, tx0_outpoint: &mercurylib::transfer::TxOutpoint, tx0_hex: &str, network: &str, confirmation_target: u32) -> Result<(bool, CoinStatus)> {
    let output_address = mercurylib::transfer::receiver::get_output_address_from_tx0(&tx0_outpoint, &tx0_hex, &network)?;

    let network = get_network(&network)?;
    let address = Address::from_str(&output_address)?.require_network(network)?;
    let script = address.script_pubkey();
    let script = script.as_script();

    let res = electrum_client.script_list_unspent(script)?;

    let block_header = electrum_client.block_headers_subscribe_raw()?;
    let blockheight = block_header.height;

    let mut status = CoinStatus::UNCONFIRMED;

    for unspent in res {
        if (unspent.tx_hash.to_string() == tx0_outpoint.txid) && (unspent.tx_pos as u32 == tx0_outpoint.vout) {
            // Electrum reports height 0 for a MEMPOOL (0-conf) utxo. Guard the confirmations math
            // with `height > 0` (as coin_status.rs does): without it, `blockheight - 0 + 1` is a huge
            // number that trivially clears confirmation_target, mis-booking an RBF-able mempool root
            // as CONFIRMED — a combine multiplies this (N roots). A 0-conf root stays UNCONFIRMED so
            // the caller rejects the branch as unconfirmed.
            if unspent.height > 0 {
                let confirmations = blockheight - unspent.height + 1;
                if confirmations as u32 >= confirmation_target {
                    status = CoinStatus::CONFIRMED;
                }
            }
            return Ok((true, status));
        }
    }

    Ok((false, status))
}

async fn unlock_statecoin(client_config: &ClientConfig, statechain_id: &str, signed_statechain_id: &str, auth_pubkey: &str) -> Result<()> {

    let path = "transfer/unlock";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", client_config.statechain_entity, path));

    let transfer_unlock_request_payload = mercurylib::transfer::receiver::TransferUnlockRequestPayload {
        statechain_id: statechain_id.to_string(),
        auth_sig: signed_statechain_id.to_string(),
        auth_pub_key: Some(auth_pubkey.to_string()),
    };

    let status = request.json(&transfer_unlock_request_payload).send().await?.status();

    if !status.is_success() {
        return Err(anyhow::anyhow!("Failed to update transfer message".to_string()));
    }

    Ok(())
}

pub struct TransferReceiveRequestResult {
    pub is_batch_locked: bool,
    pub server_pubkey: Option<String>,
}

async fn send_transfer_receiver_request_payload(client_config: &ClientConfig, transfer_receiver_request_payload: &mercurylib::transfer::receiver::TransferReceiverRequestPayload) -> Result<TransferReceiveRequestResult>{

    let path = "transfer/receiver";

    let client = client_config.get_reqwest_client()?;

        let request: reqwest::RequestBuilder = client.post(&format!("{}/{}", client_config.statechain_entity, path));

        let response = request.json(&transfer_receiver_request_payload).send().await?;

        let status = response.status();

        let value = response.text().await?;

        // A cancelled transfer answers 410 Gone with a TYPED body. It must never be flattened into
        // the generic "Failed to update transfer message" below: to a recipient, a payment that was
        // withdrawn and a mailbox that was always empty look identical, and only the typed answer
        // distinguishes them.
        if status == StatusCode::GONE {
            if let std::result::Result::Ok(error) =
                serde_json::from_str::<mercurylib::transfer::receiver::TransferReceiverErrorResponsePayload>(value.as_str())
            {
                if matches!(error.code, mercurylib::transfer::receiver::TransferReceiverError::TransferCancelledError) {
                    return Err(anyhow::Error::new(TransferWasCancelled {
                        statechain_id: transfer_receiver_request_payload.statechain_id.clone(),
                        message: error.message,
                    }));
                }
            }
            return Err(anyhow::anyhow!("transfer/receiver refused (410): {}", value));
        }

        if status == StatusCode::BAD_REQUEST{

            let error: mercurylib::transfer::receiver::TransferReceiverErrorResponsePayload = serde_json::from_str(value.as_str())?;

            match error.code {
                mercurylib::transfer::receiver::TransferReceiverError::ExpiredBatchTimeError => {
                    return Err(anyhow::anyhow!(error.message));
                },
                mercurylib::transfer::receiver::TransferReceiverError::StatecoinBatchLockedError => {
                    return Ok(TransferReceiveRequestResult {
                        is_batch_locked: true,
                        server_pubkey: None,
                    });
                },
                // A 400 never carries this code (the coordinator answers 410), but the match must
                // stay exhaustive rather than swallow it under a wildcard.
                mercurylib::transfer::receiver::TransferReceiverError::TransferCancelledError => {
                    return Err(anyhow::Error::new(TransferWasCancelled {
                        statechain_id: transfer_receiver_request_payload.statechain_id.clone(),
                        message: error.message,
                    }));
                },
            }
        }

        if status == StatusCode::OK {
            let response: mercurylib::transfer::receiver::TransferReceiverPostResponsePayload = serde_json::from_str(value.as_str())?;
            return Ok(TransferReceiveRequestResult {
                is_batch_locked: false,
                server_pubkey: Some(response.server_pubkey)
            });
        } else {
            return Err(anyhow::anyhow!("{}: {}", "Failed to update transfer message".to_string(), value));
        }
    
}
#[cfg(test)]
mod transfer_cancelled_signal_tests {
    use super::*;

    fn cancelled() -> anyhow::Error {
        anyhow::Error::new(TransferWasCancelled {
            statechain_id: "sid-abc".to_string(),
            message: "this transfer was cancelled by the sender with the recipient's consent; the payment did not complete".to_string(),
        })
    }

    /// The receive loop distinguishes a cancelled payment from an ordinary claim miss ONLY by
    /// downcasting. Pin that the type survives being returned through the claim paths unchanged.
    #[test]
    fn typed_cancellation_survives_propagation() {
        fn claim_path() -> Result<()> {
            // exactly what both claim paths now do: `Err(err) => return Err(err)`
            Err(cancelled())
        }
        let err = claim_path().unwrap_err();
        let found = err.downcast_ref::<TransferWasCancelled>();
        assert!(found.is_some(), "the cancellation signal was lost in propagation");
        assert_eq!(found.unwrap().statechain_id, "sid-abc");
    }

    /// The defect this guards against, stated as a test: re-wrapping the error in a fresh `anyhow!`
    /// (which is what both claim paths used to do — `anyhow!("Error: {}", err.to_string())`) erases
    /// the type, and the loop then treats a cancelled payment as a transient miss and prints-and-
    /// continues. Failure would look like an idle mailbox, which is the whole thing this must not do.
    #[test]
    fn restringifying_the_error_would_lose_the_signal() {
        let rewrapped = anyhow::anyhow!("Error: {}", cancelled().to_string());
        assert!(
            rewrapped.downcast_ref::<TransferWasCancelled>().is_none(),
            "if this ever passes, the claim paths may re-wrap freely; today they must not"
        );
        // and the loud text is at least still present in the string form
        assert!(rewrapped.to_string().contains("was cancelled"));
    }

    #[test]
    fn cancellation_display_names_the_transfer_and_the_reason() {
        let text = cancelled().to_string();
        assert!(text.contains("sid-abc"), "must name the transfer: {text}");
        assert!(text.contains("cancelled"), "must say cancelled: {text}");
    }

    /// The receive result carries cancellations separately from receipts, so a caller can never read
    /// one as the other.
    #[test]
    fn receive_result_separates_cancellations_from_receipts() {
        let r = TransferReceiveResult {
            is_there_batch_locked: false,
            received_statechain_ids: vec!["got-paid".to_string()],
            cancelled_statechain_ids: vec!["sid-abc".to_string()],
        };
        assert!(!r.received_statechain_ids.contains(&"sid-abc".to_string()));
        assert_eq!(r.cancelled_statechain_ids, vec!["sid-abc".to_string()]);
    }

    /// A receiving slot as it exists BEFORE the transfer completes: keys derived, but no outpoint
    /// and no amount, because nothing was ever received into it. That is precisely the shape a
    /// cancellation arrives on, so the fixture is built here rather than borrowed from a
    /// general-purpose constructor that would default the outpoint to something.
    fn unmaterialised_receiving_slot() -> Coin {
        Coin {
            index: 0,
            user_privkey: String::new(),
            user_pubkey: String::new(),
            auth_privkey: String::new(),
            auth_pubkey: String::new(),
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
            statechain_id: Some("sid-abc".to_string()),
            signed_statechain_id: None,
            locktime: None,
            secret_nonce: None,
            public_nonce: None,
            blinding_factor: None,
            server_public_nonce: None,
            tx_cpfp: None,
            tx_withdraw: None,
            withdrawal_address: None,
            status: mercurylib::wallet::CoinStatus::INITIALISED,
            duplicate_index: 0,
            single_use: false,
            epoch_deadline: None,
        }
    }

    /// The activity booked for a cancelled payment is distinguishable from a received one, and works
    /// for a receiving slot that never materialised an outpoint.
    #[test]
    fn cancelled_transfer_is_booked_as_an_activity() {
        let mut activities: Vec<Activity> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let coin = unmaterialised_receiving_slot();

        record_cancelled_transfer(
            &TransferWasCancelled {
                statechain_id: "sid-abc".to_string(),
                message: "cancelled".to_string(),
            },
            &coin,
            &mut activities,
            &mut ids,
        );

        assert_eq!(ids, vec!["sid-abc".to_string()]);
        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].action, "TransferCancelled");
        // no outpoint on an unmaterialised slot: the entry falls back to naming the transfer
        assert_eq!(activities[0].utxo, "sid-abc");
    }
}

#[cfg(test)]
mod terminal_parents_tests {
    use super::{required_terminal_ancestors, terminal_parents_sufficient};

    // Build a raw (non-witness) tx hex with `n_inputs` inputs and one output — version-agnostic
    // consensus encoding, enough for the input-count check (n_inputs < 253 so the varint is 1 byte).
    fn tx_hex_with_inputs(n_inputs: usize) -> String {
        let mut h = String::from("02000000"); // version 2
        h.push_str(&format!("{:02x}", n_inputs)); // input count (compact size, < 253)
        for i in 0..n_inputs {
            h.push_str(&format!("{:02x}", i as u8).repeat(32)); // 32-byte prevout txid
            h.push_str("00000000"); // prevout vout = 0
            h.push_str("00"); // empty scriptSig
            h.push_str("ffffffff"); // sequence
        }
        h.push_str("01"); // one output
        h.push_str("e803000000000000"); // value = 1000 sats
        h.push_str("00"); // empty scriptPubKey
        h.push_str("00000000"); // locktime 0
        h
    }

    // required_terminal_ancestors = Σ inputs over branch txs (NOT the hop/tx count). A combine tx
    // with N inputs demands N terminal ancestors, closing the multi-carrier double-spend hole.
    #[test]
    fn required_ancestors_counts_inputs_not_hops() {
        // Linear split chain: three 1-input txs -> 3 (unchanged; == hop count).
        let split_chain = vec![tx_hex_with_inputs(1), tx_hex_with_inputs(1), tx_hex_with_inputs(1)];
        assert_eq!(required_terminal_ancestors(&split_chain).unwrap(), 3);

        // A single 2-input COMBINE (one hop) -> 2 required, not 1.
        let combine2 = vec![tx_hex_with_inputs(2)];
        assert_eq!(required_terminal_ancestors(&combine2).unwrap(), 2);
        // The old per-hop rule would have required only 1; the sender could hide the 2nd input.
        assert!(!terminal_parents_sufficient(1, required_terminal_ancestors(&combine2).unwrap()));
        assert!(terminal_parents_sufficient(2, required_terminal_ancestors(&combine2).unwrap()));

        // A wide 6-input combine -> 6.
        let combine6 = vec![tx_hex_with_inputs(6)];
        assert_eq!(required_terminal_ancestors(&combine6).unwrap(), 6);
        assert!(!terminal_parents_sufficient(5, 6));
        assert!(terminal_parents_sufficient(6, 6));

        // Combine (N=3) then split (1) -> 3 + 1 = 4.
        let combine_then_split = vec![tx_hex_with_inputs(3), tx_hex_with_inputs(1)];
        assert_eq!(required_terminal_ancestors(&combine_then_split).unwrap(), 4);
    }

    // D1: an exit branch must be a TREE. A non-tree branch (a repeated prevout, across txs or within
    // one combine tx) is un-broadcastable and must be rejected — else a receiver books a coin that
    // can never confirm on-chain (fund stranding).
    #[test]
    fn non_tree_branch_is_rejected() {
        use super::reject_non_tree_branch;
        let de = |h: &str| -> bitcoin::Transaction {
            bitcoin::consensus::encode::deserialize(&hex::decode(h).unwrap()).unwrap()
        };
        // A single tx with two DISTINCT inputs (00..:0 and 01..:0) is a tree.
        assert!(reject_non_tree_branch(&[de(&tx_hex_with_inputs(2))]).is_ok(), "distinct inputs = tree");
        // Two txs BOTH spending 00..:0 → conflicting spends of one outpoint → non-tree, rejected.
        assert!(
            reject_non_tree_branch(&[de(&tx_hex_with_inputs(1)), de(&tx_hex_with_inputs(1))]).is_err(),
            "two branch txs spending the same outpoint must be rejected"
        );
        // A single combine tx with a DUPLICATE input (00..:0 twice) → non-tree, rejected.
        let dup_input_tx = {
            let mut h = String::from("02000000");
            h.push_str("02");
            for _ in 0..2 {
                h.push_str(&"00".repeat(32));
                h.push_str("00000000");
                h.push_str("00");
                h.push_str("ffffffff");
            }
            h.push_str("01");
            h.push_str("e803000000000000");
            h.push_str("00");
            h.push_str("00000000");
            h
        };
        assert!(
            reject_non_tree_branch(&[de(&dup_input_tx)]).is_err(),
            "a combine tx with a duplicate input must be rejected"
        );
    }

    // INV-20: the receiver must reject a branch-funded sub-coin whose sender names FEWER terminal
    // ancestors than the branch has hops (the previously-accepted empty list is the len==0 case).
    #[test]
    fn empty_parents_on_a_branch_is_rejected() {
        // 1-hop branch, zero named ancestors -> reject (the confirmed exploit).
        assert!(!terminal_parents_sufficient(0, 1));
        // deeper branches with an empty list -> reject.
        assert!(!terminal_parents_sufficient(0, 3));
    }

    #[test]
    fn fewer_parents_than_hops_is_rejected() {
        // 3-hop branch but only 1 ancestor named (a single terminal "decoy") -> reject.
        assert!(!terminal_parents_sufficient(1, 3));
        assert!(!terminal_parents_sufficient(2, 3));
    }

    #[test]
    fn one_parent_per_hop_is_accepted() {
        // honest SDK: ancestors == branch depth.
        assert!(terminal_parents_sufficient(1, 1));
        assert!(terminal_parents_sufficient(2, 2));
        assert!(terminal_parents_sufficient(3, 3));
    }

    #[test]
    fn more_parents_than_hops_is_accepted() {
        // a combine hop consumes several ancestors -> more parents than branch txs is fine.
        assert!(terminal_parents_sufficient(4, 2));
    }

    #[test]
    fn degenerate_zero_branch_still_requires_a_parent() {
        // defensive: max(1) means "no ancestors named" is never sufficient.
        assert!(!terminal_parents_sufficient(0, 0));
        assert!(terminal_parents_sufficient(1, 0));
    }
}

#[cfg(test)]
mod poll_cancellation_reporting_tests {
    use super::*;

    /// The poll-level error must carry the IDS, not only a sentence. `execute` has to stay `Err` —
    /// that is the loud signal — but a caller which knows how to report cancellations properly
    /// (the SDK's `claim`) must be able to recover them WITHOUT re-parsing prose, otherwise its only
    /// options are to lose the whole pass or to lose the cancellation.
    #[test]
    fn the_poll_error_carries_the_ids_not_just_a_sentence() {
        let err = anyhow::Error::new(TransfersCancelledInPoll {
            statechain_ids: vec!["sid-a".to_string(), "sid-b".to_string()],
        });
        let found = err
            .downcast_ref::<TransfersCancelledInPoll>()
            .expect("the poll error must be downcastable to its ids");
        assert_eq!(found.statechain_ids, vec!["sid-a".to_string(), "sid-b".to_string()]);
    }

    /// ...and it still SAYS the same thing. The text is what a plain `?`-ing caller shows the user,
    /// so adding the type must not quietly soften the wording.
    #[test]
    fn the_poll_error_still_says_the_payment_did_not_arrive() {
        let text = TransfersCancelledInPoll { statechain_ids: vec!["sid-a".to_string()] }.to_string();
        assert!(text.contains("CANCELLED"), "{text}");
        assert!(text.contains("will never complete"), "{text}");
        assert!(text.contains("sid-a"), "must name the transfer: {text}");
    }
}


/// [D38/D16] `protocol_version` is a SHAPE selector, and the code's own history proves it.
#[cfg(test)]
mod exact_shape_dispatch_tests {
    use super::*;

    #[test]
    fn the_admissible_set_is_exact_and_unknown_values_are_refused() {
        for v in ADMISSIBLE_PROTOCOL_VERSIONS {
            assert!(admissible_shape(v).is_ok(), "shape {v} must be admissible");
        }
        // 3 is the deleted legacy child. It must now be REFUSED — and it is the value the old
        // child "floor" was set to, over a set that never contained it.
        for v in [1u32, 3, 5, 99, u32::MAX] {
            let e = admissible_shape(v).expect_err("unknown shape must be refused");
            assert!(e.to_string().contains(&v.to_string()), "the refusal must name the value: {e}");
            assert!(
                e.to_string().contains("SHAPE") || e.to_string().contains("shape"),
                "the refusal must say WHY — an ordinal reading is the error being prevented: {e}"
            );
        }
    }

    /// The floors are now exact shapes. If either drifts back to a value outside the admissible set,
    /// the comparison it feeds becomes unreachable or vacuous.
    #[test]
    fn the_named_shapes_are_inside_the_admissible_set() {
        assert!(ADMISSIBLE_PROTOCOL_VERSIONS.contains(&SHAPE_ROOT_LADDER));
        assert!(ADMISSIBLE_PROTOCOL_VERSIONS.contains(&SHAPE_CHILD));
        assert_eq!(MIN_PREPAY_PROTOCOL_VERSION, SHAPE_ROOT_LADDER);
        assert_eq!(MIN_PREPAY_CHILD_PROTOCOL_VERSION, SHAPE_CHILD);
        // The seam that revealed the category error: the child gate used to be 3.
        assert!(
            !ADMISSIBLE_PROTOCOL_VERSIONS.contains(&3),
            "3 was a floor over a set containing no 3; if 3 is admissible again this test is stale"
        );
    }
}
