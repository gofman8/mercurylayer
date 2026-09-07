use std::{collections::{HashMap, HashSet}, str::FromStr};

use crate::{sqlite_manager::{get_wallet, update_wallet}, client_config::ClientConfig, utils};
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
    /// The coin's RGB material, if it carries a token: the COLOURED ladder's LEAF consignment
    /// (`ColoredLadder` / `ColoredChild`), wrapped as the SDK's `{"c","a","s"}` envelope (`c` =
    /// base64 consignment, `a` = the declared amount, `s` = the sats on the final state) so a paying
    /// party can validate the coloured value pre-payment exactly the way the claim path books it
    /// (audit [4], `validate_pending_token_ex`). The declared `a` is only ever CROSS-CHECKED
    /// against what the consignment assigns. No consignment rides on a backup row any more — there
    /// are none — and a PLAIN ladder carries `None`.
    pub rgb_consignment: Option<String>,
    /// Where that consignment ASSIGNS the allocation: the receiver's own final-state payload output
    /// (`state.txid:payload_vout` of the root ladder's current state, or of the child's own state) —
    /// the outpoint `accept_colored_ladder` / `colored_child_health` book at claim. Empty when
    /// `rgb_consignment` is `None`. NOT the funding outpoint: on a coloured ladder the allocation
    /// sits at the leaf, not at `F`.
    pub rgb_assignment_txid: String,
    pub rgb_assignment_vout: u32,
    /// The coin's own funding outpoint (the RGB witness for consignment validation).
    pub funding_txid: String,
    pub funding_vout: u32,
    /// The un-broadcast exit branch (raw tx hex, root-first) the consignment resolves against.
    pub branch_txs: Vec<String>,
    /// **[P3] A COLOURED coin's own witness chain**, root→leaf, when this transfer conveys one:
    /// a coloured ROOT ladder's `ladder_txids()` (T, X_m, S_k), or a coloured CHILD's
    /// `colored_child_txids()`. Named for the child case it was added for; the root case fills the
    /// same field so one validator (`validate_pending_token_ex`) serves both.
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
pub(crate) const ADMISSIBLE_PROTOCOL_VERSIONS: [u32; 2] = [2, 4];

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
             generation: 2 is a root-ladder conveyance, 4 a child conveyance with key handover. \
             The un-laddered shape (0) no longer exists — every coin's exit is its ladder. The \
             numeric ordering carries no meaning, so an unknown value cannot be 'at least' \
             anything — it is refused."
        ));
    }
    Ok(())
}



/// [D1/D2] PRE-PAY census of a LADDERED conveyance, for a party that is about to make an
/// IRREVERSIBLE payment against it (the SSP) and has NOT claimed the coin.
///
/// Fail-closed BY CONSTRUCTION: every step returns `Err`, and the only caller maps `Err` to
/// `ladder_census_ok = false`. There is deliberately no success path that skips a check.
///
/// `my_backup` is the PROSPECTIVE OWNER's seed-derived backup address (the payer's own), derived
/// exactly as the claim path derives the receiver's.
///
/// A laddered coin carries NO flat backup, so the census is exactly `tiers + superseded` and the
/// funding outpoint is the bundle's own, bound to the chain by `coin_authority_from_tx0`. Same
/// checks as the claim path, in the same order.
async fn prepay_flat_census(
    client_config: &ClientConfig,
    network: &str,
    my_backup: &str,
    transfer_msg: &mercurylib::transfer::TransferMsg,
    info_config: &InfoConfig,
) -> Result<()> {
    // [D38/D16] Exact-set first: an unrecognised shape must never reach a comparison.
    admissible_shape(transfer_msg.protocol_version)?;
    if transfer_msg.protocol_version != SHAPE_ROOT_LADDER {
        return Err(anyhow!(
            "pre-pay census: conveyance declares protocol_version {} but this path accepts only the root-ladder shape {} — refusing (an unrecognised version must never bypass the [C-1] coin binding)",
            transfer_msg.protocol_version,
            SHAPE_ROOT_LADDER
        ));
    }
    let ladder_json = transfer_msg
        .tesr_ladder
        .as_ref()
        .ok_or_else(|| anyhow!("pre-pay census: laddered conveyance is missing its TES-R ladder"))?;
    let bundle: crate::tesr::TesrBundle = serde_json::from_str(ladder_json)
        .map_err(|e| anyhow!("pre-pay census: malformed TES-R ladder: {e}"))?;

    // NO FLAT BACKUP AND NO BRANCH may travel with a ladder — same rule, same function, as the
    // claim path; this is the path that authorises an IRREVERSIBLE Lightning leg.
    crate::tesr::verify_flat_backup_lane(&bundle, &transfer_msg.backup_transactions)
        .map_err(|e| anyhow!("pre-pay census: {e}"))?;
    refuse_branch_material(transfer_msg).map_err(|e| anyhow!("pre-pay census: {e}"))?;

    // [D2] MODEL-A OWNER-EXIT BINDING. `verify_bundle_bound` binds the ladder to the COIN but is
    // structurally incapable of checking WHO the ladder exits to. The payer is the prospective
    // owner, so the ladder must exit to the payer's key.
    if bundle.owner_exit_address != my_backup {
        return Err(anyhow!(
            "pre-pay census: the conveyed ladder exits to {} but the prospective owner's key is {} — a coin-bound ladder that pays a third party",
            bundle.owner_exit_address,
            my_backup
        ));
    }

    // The funding tx must have been read FROM THE CHAIN — the authority the ladder is bound to.
    let tx0_hex = get_tx0(&client_config.electrum_client, &bundle.f_txid).await.map_err(|e| {
        anyhow!(
            "pre-pay census: the coin's funding UTXO {}:{} is not on-chain ({e}) — nothing authoritative to bind the ladder to",
            bundle.f_txid,
            bundle.f_vout
        )
    })?;

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
        txid: bundle.f_txid.clone(),
        vout: bundle.f_vout,
    };
    let (unspent, status) = verify_tx0_output_is_unspent_and_confirmed(
        &client_config.electrum_client,
        &tx0_outpoint,
        &tx0_hex,
        network,
        client_config.confirmation_target,
    )
    .await?;
    if !unspent {
        return Err(anyhow!(
            "pre-pay census: funding output {}:{} is already spent — the conveyed ladder is dead",
            bundle.f_txid,
            bundle.f_vout
        ));
    }
    if status != CoinStatus::CONFIRMED {
        return Err(anyhow!(
            "pre-pay census: funding output {}:{} is not confirmed to the client's target",
            bundle.f_txid,
            bundle.f_vout
        ));
    }

    let authority = crate::tesr::coin_authority_from_tx0(
        &transfer_msg.statechain_id,
        &bundle.f_txid,
        bundle.f_vout,
        &tx0_hex,
        info.aggregate_pubkey.clone(),
    )?;
    // [P0-3] EXIT-CHAIN LENGTH CAP, receiver-derived on both terms; [C-1] the conveyed schedule is
    // BOUND rather than merely unused.
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
    // THE CENSUS: `se_num_sigs == tiers + superseded`. The flat term is zero because no flat backup
    // is ever co-signed for a laddered coin — there is nothing for a padded vector to inflate.
    crate::tesr::verify_bundle_bound(&bundle, info.num_sigs, 0, &authority)
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
    // **[D75] EACH CENSUS SELF-GUARDS ITS OWN SHAPE.** This function had NO `admissible_shape` call
    // and gated with `<`, so every `protocol_version` in `[SHAPE_CHILD, u32::MAX]` cleared it — the
    // exact ordinal reading [D16] forbids, on the lane that pays an irreversible Lightning leg.
    //
    // Inert at HEAD (an unknown value selects the same arms shape 4 does), which is why this is a
    // correctness fix rather than a patch for a live theft. It is placed HERE, inside the census,
    // and deliberately NOT before the caller's lane select: the lane is chosen by payload PRESENCE
    // (`child_tesr_bundle.is_some()`), and hoisting a shape refusal above that select would refuse
    // messages this arm never claimed to handle.
    admissible_shape(transfer_msg.protocol_version)?;
    if transfer_msg.protocol_version != SHAPE_CHILD {
        return Err(anyhow!(
            "pre-pay census: a child bundle was conveyed under protocol_version {} but the child \
             shape is exactly {} — refusing. The tag selects a SHAPE, not a level: it is compared \
             for equality, never for ordering ([D16], [D75]).",
            transfer_msg.protocol_version,
            SHAPE_CHILD
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
            // The coin's funding outpoint is the conveyed LADDER's own `f_txid:f_vout`, and the
            // amount is read from that transaction ON CHAIN. A laddered coin carries no flat backup
            // to derive either from, and an un-broadcast funding is not admitted on this lane. A
            // child conveyance has no on-chain funding of its own; its amount comes from the child
            // census below and nothing else.
            // The conveyed ROOT ladder, when this is a whole-coin hop — parsed once; the funding
            // outpoint, the amount and (for a coloured ladder) the RGB material all come off it. A
            // child conveyance carries no on-chain funding of its own; its amount comes from the
            // child census below and nothing else.
            let root_bundle: Option<crate::tesr::TesrBundle> =
                if transfer_msg.child_tesr_bundle.is_none() {
                    transfer_msg
                        .tesr_ladder
                        .as_deref()
                        .and_then(|j| serde_json::from_str::<crate::tesr::TesrBundle>(j).ok())
                } else {
                    None
                };
            let child_bundle: Option<crate::tesr::ChildTesrBundle> = transfer_msg
                .child_tesr_bundle
                .as_deref()
                .and_then(|j| serde_json::from_str::<crate::tesr::ChildTesrBundle>(j).ok());
            let mut funding_txid = String::new();
            let mut funding_vout = 0u32;
            let amount: u64 = match root_bundle.as_ref() {
                Some(b) => {
                    funding_txid = b.f_txid.clone();
                    funding_vout = b.f_vout;
                    match get_tx0(&client_config.electrum_client, &b.f_txid).await {
                        std::result::Result::Ok(tx0_hex) => {
                            let outpoint = mercurylib::transfer::TxOutpoint {
                                txid: b.f_txid.clone(),
                                vout: b.f_vout,
                            };
                            mercurylib::transfer::receiver::get_amount_from_tx0(&tx0_hex, &outpoint)
                                .map(|a| a as u64)
                                .unwrap_or(0)
                        }
                        Err(_) => 0,
                    }
                }
                None => 0,
            };
            // A laddered coin's RGB material, when it has any, is the COLOURED ladder itself: the
            // LEAF consignment, resolved against the coin's own tier txids and assigning the
            // allocation to the receiver's final-state payload output — exactly what
            // `accept_colored_ladder` / `colored_child_health` validate at claim. No consignment
            // rides on a flat row any more, and a plain ladder carries none. Wrapped as the SDK's
            // `{"c","a","s"}` envelope so the paying party's validator reads it unchanged; the
            // declared `a` is only ever CROSS-CHECKED against what the consignment assigns, and a
            // bundle that will not parse or is plain yields NOTHING — which is what the paying party
            // refuses on, so a malformed conveyance cannot become a payable one by failing to
            // describe itself.
            let (rgb_consignment, rgb_assignment_txid, rgb_assignment_vout, rgb_witness_txids): (
                Option<String>,
                String,
                u32,
                Vec<String>,
            ) = match (root_bundle.as_ref(), child_bundle.as_ref()) {
                (Some(b), _) if b.is_colored() => {
                    let rgb = b.rgb.as_ref().expect("is_colored");
                    let leaf = b.current().state.clone();
                    (
                        b.leaf_consignment().map(|c| {
                            serde_json::json!({ "c": c, "a": rgb.amount, "s": leaf.out_value })
                                .to_string()
                        }),
                        leaf.txid.clone(),
                        leaf.payload_vout,
                        b.ladder_txids(),
                    )
                }
                (_, Some(cb)) if cb.is_colored() => {
                    let rgb = cb.rgb.as_ref().expect("is_colored");
                    let leaf = cb.child_state.clone();
                    (
                        cb.leaf_consignment().map(|c| {
                            serde_json::json!({ "c": c, "a": rgb.amount, "s": leaf.out_value })
                                .to_string()
                        }),
                        leaf.txid.clone(),
                        leaf.payload_vout,
                        cb.colored_child_txids().unwrap_or_default(),
                    )
                }
                _ => (None, String::new(), 0, Vec::new()),
            };
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
                        &info_config,
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
                rgb_assignment_txid,
                rgb_assignment_vout,
                funding_txid,
                funding_vout,
                branch_txs: transfer_msg.branch_txs.clone(),
                // [P3] The coloured coin's own witness chain, derived above from the bundle the
                // message already carries; empty for a plain coin or an unparseable bundle.
                child_witness_txids: rgb_witness_txids,
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





/// **[REQ-83] Collect LADDERLESS claims delivered to this wallet.**
///
/// Its own pass, and it has to be: every other receive path is per-COIN, and a stub is not a coin —
/// no slot is created for it and `SP.out[j]` pays the payee's own key. There is no hand-over to
/// complete, nothing to rotate, and no `coin` to hang the message on. What arrives is a DOCUMENT.
///
/// **A message that claims to be both a delivery and a hand-over is refused outright**, rather than
/// having one arm win. Letting presence-order decide would let a sender choose which kind of message
/// they sent after the receiver started reading it — and the two kinds are checked by entirely
/// different rules.
///
/// Returns the number of claims newly adopted. Each is verified against the chain and the SE's
/// attested facts before it is stored; a delivery that does not verify is skipped, not stored, and
/// the mailbox read is non-destructive so nothing is lost by skipping.
pub async fn claim_ladderless_deliveries(
    client_config: &ClientConfig,
    wallet_name: &str,
) -> Result<usize> {
    let wallet = crate::sqlite_manager::get_wallet(&client_config.pool, wallet_name).await?;
    let mut privkey_by_pubkey: HashMap<String, String> = HashMap::new();
    for coin in wallet.coins.iter() {
        privkey_by_pubkey
            .entry(coin.auth_pubkey.clone())
            .or_insert_with(|| coin.auth_privkey.clone());
    }

    let mut adopted = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    for (auth_pubkey, auth_privkey) in privkey_by_pubkey.iter() {
        let enc_messages = match get_msg_addr(auth_pubkey, client_config).await {
            std::result::Result::Ok(m) => m,
            Err(_) => continue,
        };
        for enc_message in enc_messages {
            let msg = match mercurylib::transfer::receiver::decrypt_transfer_msg(
                &enc_message,
                auth_privkey,
            ) {
                std::result::Result::Ok(m) => m,
                Err(_) => continue, // not addressed to us / undecryptable
            };
            let Some(leaf_json) = msg.ladderless_leaf.as_deref() else {
                continue; // an ordinary hand-over: not this pass's business
            };
            if msg.child_tesr_bundle.is_some() || msg.tesr_ladder.is_some() {
                return Err(anyhow!(
                    "a delivered ladderless claim also carries hand-over material. A message is one \
                     kind or the other, and the two are checked by different rules — refusing rather \
                     than letting presence-order decide which the sender meant"
                ));
            }
            let leaf: crate::tesr::LadderlessLeaf = match serde_json::from_str(leaf_json) {
                std::result::Result::Ok(l) => l,
                Err(_) => continue,
            };
            // Deduplicated by the OUTPOINT it claims, because that is what a stub IS. The mailbox is
            // non-destructive, so the same delivery is re-served on every pass.
            let key = format!("{}:{}", leaf.parent_statechain_id, leaf.sp_vout);
            if !seen.insert(key) {
                continue;
            }
            if crate::tesr::adopt_stub_leaf(client_config, wallet_name, &leaf)
                .await
                .is_ok()
            {
                adopted += 1;
            }
        }
    }
    Ok(adopted)
}

async fn validate_encrypted_message(client_config: &ClientConfig, coin: &Coin, enc_message: &str, network: &str, wallet_name: &str, info_config: &InfoConfig, blockheight: u32) -> Result<()> {

    let client_auth_key = coin.auth_privkey.clone();
    let new_user_pubkey = coin.user_pubkey.clone();

    let transfer_msg = mercurylib::transfer::receiver::decrypt_transfer_msg(enc_message, &client_auth_key)?;

    // **[D71 / M-4] ALREADY ADOPTED — refuse a replayed conveyance BY NAME.**
    //
    // The mailbox read is non-destructive, so a claimed coin's message keeps being re-served, and a
    // coordinator can duplicate a ciphertext at will. A duplicate takes the same path as an honest
    // re-serve: the first copy consumes the INITIALISED slot, the second mints a slot by CLONING the
    // keys of a coin with the same auth key — so every check that binds the message to the coin
    // passes. What refuses it today is `validate_tx0_output_pubkey`, because the completed handover
    // rotated the SE's share and `S + E' != tx0.out.key`. That is a CONSEQUENCE, not a rule: the
    // protection is a side effect of an unrelated subsystem, and a specification cannot state it.
    //
    // The child lane has refused this by name since [D3] ("split child … already adopted"); the root
    // lane is the one that never did. Same rule, same lane, stated where it belongs.
    //
    // **The predicate is ADOPTED-AND-SPENDABLE, and its narrowness is the whole care here.**
    //
    //  * `TRANSFERRED` / `WITHDRAWN` / `INVALIDATED` / `DUPLICATED` are NOT adoption — refusing on
    //    those would break the legitimate round trip (send a coin away, receive it back later),
    //    which leaves exactly such a row behind.
    //  * `IN_TRANSFER` is NOT adoption either, and this is the case a careless predicate gets wrong:
    //    in a SELF-TRANSFER the sender's own row sits at `IN_TRANSFER` under the very id the
    //    receiving slot is about to adopt, in the same wallet (rgb10 PART 2 does this). Refusing
    //    there would break a working feature to stop a replay.
    //  * `WITHDRAWING` IS adoption: the wallet holds the coin and is exiting it on chain.
    // `Ok` is shadowed in this module, so the path is spelled out.
    if let std::result::Result::Ok(existing) =
        crate::sqlite_manager::get_wallet(&client_config.pool, wallet_name).await
    {
        let live = existing.coins.iter().any(|c| {
            c.statechain_id.as_deref() == Some(transfer_msg.statechain_id.as_str())
                && matches!(
                    c.status,
                    CoinStatus::IN_MEMPOOL
                        | CoinStatus::UNCONFIRMED
                        | CoinStatus::CONFIRMED
                        | CoinStatus::WITHDRAWING
                )
        });
        if live {
            return Err(anyhow::anyhow!(
                "statechain id {} is already adopted by this wallet — refusing this conveyance as a \
                 replay. The mailbox read is non-destructive and a duplicated message would \
                 otherwise mint a second row for one coin; the balance would then be summed over \
                 rows that describe the same coin twice. If this is a coin you genuinely re-received \
                 after transferring it away, or a self-transfer whose sending row is IN_TRANSFER, \
                 this refusal does not fire — neither status counts as adoption.",
                transfer_msg.statechain_id
            ));
        }
    }

    // [in-ladder split] A split-child payment carries the child's exit bundle (and, from
    // `protocol_version >= 4`, the key-handover material) but NO backup ladder. Verify the bundle
    // against authoritative on-chain + SE values (verify_child_bundle: parent F on-chain, parent+child
    // terminal, child pays THIS coin's key) and skip the backup-chain checks below (there are
    // none to validate).
    if let Some(cb_json) = &transfer_msg.child_tesr_bundle {
        // [R4] SHAPE CHECK ON THE CLAIM PATH'S CHILD LANE, mirroring `prepay_child_census`. A
        // `child_tesr_bundle` attached to a message declaring any other shape is a version/payload
        // mismatch: the sender is asking to be processed by rules that predate the child lane while
        // shipping child material. Refuse rather than guess.
        //
        // **[D75] This block ends in `return`, STRICTLY BEFORE this function's own
        // `admissible_shape` call — so until now the child lane reached no shape check at all, and
        // the ordinal `<` let every value in `[SHAPE_CHILD, u32::MAX]` through.** The check belongs
        // here, where the lane is, not at the top: moving it up would not have been equivalent,
        // because this arm is selected by payload presence rather than by the tag.
        admissible_shape(transfer_msg.protocol_version)?;
        if transfer_msg.protocol_version != SHAPE_CHILD {
            return Err(anyhow::anyhow!(
                "a child bundle was conveyed under protocol_version {} but the child shape is \
                 exactly {} — refusing. The tag selects a SHAPE, not a level ([D16], [D75]).",
                transfer_msg.protocol_version,
                SHAPE_CHILD
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
    // ROOT-LADDER LANE. A laddered coin carries NO flat backup: its exit is its ladder, and the
    // census the receiver runs is exactly `se_num_sigs == tiers + superseded`. Everything below is
    // derived from the conveyed BUNDLE and bound to the chain and to the coordinator's record —
    // never from a `backup_transactions` vector, which must be EMPTY. There is no un-laddered lane.
    // ---------------------------------------------------------------------------------------------

    // **[D38/D16] EXACT-SET DISPATCH, before any shape-specific rule.** An unrecognised
    // `protocol_version` is refused outright rather than compared with `>=` — see
    // `admissible_shape`. This is what makes the uniffi FFI's silent stripping of the tag fail
    // CLOSED instead of downgrading a laddered conveyance to a census that no longer exists.
    admissible_shape(transfer_msg.protocol_version)?;
    if transfer_msg.protocol_version != SHAPE_ROOT_LADDER {
        return Err(anyhow::anyhow!(
            "transfer message declares protocol_version {} for a root conveyance; the only \
             admissible root shape is {} (a TES-R ladder). There is no un-laddered lane: a coin \
             whose exit material is a flat absolute-locktime backup cannot be received.",
            transfer_msg.protocol_version,
            SHAPE_ROOT_LADDER
        ));
    }
    let ladder = transfer_msg
        .tesr_ladder
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("laddered transfer is missing its TES-R ladder"))?;
    let bundle: crate::tesr::TesrBundle = serde_json::from_str(ladder)
        .map_err(|e| anyhow::anyhow!("malformed TES-R ladder: {e}"))?;

    // [RETAINED BACKUPS] NO FLAT BACKUP MAY TRAVEL WITH A LADDER. A conveyed flat backup would be
    // a co-sign the census cannot account for, and — worse — a matured spend of `F` that a prior
    // owner keeps: plain, it burns a carrier's allocation; coloured, it re-assigns it. The vector
    // is therefore required to be empty, by name, and so is every piece of branch material.
    crate::tesr::verify_flat_backup_lane(&bundle, &transfer_msg.backup_transactions).map_err(
        |e| anyhow::anyhow!("refusing conveyance of {}: {e}", transfer_msg.statechain_id),
    )?;
    refuse_branch_material(&transfer_msg)?;

    // The funding outpoint is the bundle's own, and the transaction is read FROM THE CHAIN: a
    // laddered coin rests on an on-chain funding UTXO (confirmed or still in the mempool), and a
    // funding we cannot fetch is a coin we cannot bind a ladder to. The outpoint is sender-supplied
    // until `verify_bundle_bound` below has compared it against the on-chain output and the
    // coordinator's recorded aggregate; every check between here and there reads the CHAIN's copy
    // of the transaction, never the message's.
    let tx0_outpoint = mercurylib::transfer::TxOutpoint {
        txid: bundle.f_txid.clone(),
        vout: bundle.f_vout,
    };
    let tx0_hex = get_tx0(&client_config.electrum_client, &bundle.f_txid)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "laddered transfer's funding UTXO {}:{} is not on-chain ({e}) — cannot bind the \
                 ladder to the coin",
                bundle.f_txid,
                bundle.f_vout
            )
        })?;

    let is_transfer_signature_valid = mercurylib::transfer::receiver::verify_transfer_signature(&new_user_pubkey, &tx0_outpoint, &transfer_msg)?;

    if !is_transfer_signature_valid {
        return Err(anyhow::anyhow!("Invalid transfer signature".to_string()));
    }

    let statechain_info = utils::get_statechain_info(&transfer_msg.statechain_id, &client_config)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Statechain info not found"))?;

    let is_tx0_output_pubkey_valid = mercurylib::transfer::receiver::validate_tx0_output_pubkey(&statechain_info.enclave_public_key, &transfer_msg, &tx0_outpoint, &tx0_hex, network)?;

    if !is_tx0_output_pubkey_valid {
        return Err(anyhow::anyhow!("Invalid tx0 output pubkey".to_string()));
    }

    // [C-1] THE BOUND VERIFIER. `verify_bundle` alone checks the trigger against `bundle.f_txid`
    // and the tier payees against `bundle.agg_address` — all fields of the very bundle under test —
    // so it proves internal consistency, not that the ladder describes THIS coin. The authority
    // comes from the coin: the funding output just fetched from the chain, its value and aggregate
    // scriptPubKey, and the coordinator's recorded aggregate for the sid.
    let coin_authority = crate::tesr::coin_authority_from_tx0(
        &transfer_msg.statechain_id,
        &tx0_outpoint.txid,
        tx0_outpoint.vout,
        &tx0_hex,
        statechain_info.aggregate_pubkey.clone(),
    )?;
    // [P0-3] EXIT-CHAIN LENGTH CAP — admission only, never reachable from an exit path. Both terms
    // are receiver-derived: `initlock` (the exit window every walk must fit inside) from this
    // wallet's own `/info/config` fetch and the schedule from its own network preset. [C-1] The
    // conveyed schedule is BOUND rather than merely unused: `cap_schedule` refuses a ladder whose
    // declared schedule contradicts the receiver's preset.
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
    // THE CENSUS. `se_num_sigs == tiers + superseded`, exact equality against the enclave's
    // attested count. The flat term is ZERO by construction — not "zero because the vector was
    // empty", but zero because no flat backup is ever co-signed for a laddered coin, at deposit or
    // at any hop. A hidden co-signed state has no slot to hide in.
    crate::tesr::verify_bundle_bound(
        &bundle,
        statechain_info.num_sigs,
        0,
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

    // `F` must be UNSPENT. Its confirmation status is not a refusal: a ladder over a funding output
    // still in the mempool is a good coin (its exit is signed and needs no confirmation), and the
    // coin is booked with the chain's status and walked to CONFIRMED like any deposit.
    let (is_tx0_output_unspent, _) = verify_tx0_output_is_unspent_and_confirmed(&client_config.electrum_client, &tx0_outpoint, &tx0_hex, &network, client_config.confirmation_target).await?;

    if !is_tx0_output_unspent {
        return Err(anyhow::anyhow!("tx0 output is spent".to_string()));
    }

    let _ = blockheight;

    Ok(())
}

/// A root-ladder conveyance carries no exit BRANCH and names no terminal parents: those were the
/// off-chain split lane's exit material, whose funding was an un-broadcast transaction. A ladder is
/// rooted at an ON-CHAIN funding output, so any branch material beside it describes a coin this
/// lane does not admit.
fn refuse_branch_material(transfer_msg: &mercurylib::transfer::TransferMsg) -> Result<()> {
    if !transfer_msg.branch_txs.is_empty() || !transfer_msg.terminal_parents.is_empty() {
        return Err(anyhow::anyhow!(
            "refusing conveyance of {}: it carries {} exit-branch transaction(s) and {} terminal \
             parent id(s) beside a TES-R ladder. A laddered coin is rooted at an on-chain funding \
             output and has no exit branch; the off-chain branch lane no longer exists.",
            transfer_msg.statechain_id,
            transfer_msg.branch_txs.len(),
            transfer_msg.terminal_parents.len()
        ));
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
    // the child a FIRST-CLASS coin, not merely an exitable claim (docs/utexo/spec/CHILDREN.md).
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
        let _sp_out = sp_tx
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

    // ROOT-LADDER ADOPTION. The conveyed ladder was verified in `validate_encrypted_message`: bound
    // to the on-chain funding output and to the coordinator's recorded aggregate, its census
    // balanced against the enclave's attested count, its final state paying this coin's own key.
    // A laddered coin carries NO flat backup, so nothing here is derived from
    // `backup_transactions` — the funding outpoint is the bundle's own.
    let ladder = transfer_msg
        .tesr_ladder
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("laddered transfer is missing its TES-R ladder"))?;
    let bundle: crate::tesr::TesrBundle = serde_json::from_str(ladder)
        .map_err(|e| anyhow::anyhow!("malformed TES-R ladder: {e}"))?;
    let tx0_outpoint = mercurylib::transfer::TxOutpoint {
        txid: bundle.f_txid.clone(),
        vout: bundle.f_vout,
    };
    let tx0_hex = get_tx0(&client_config.electrum_client, &bundle.f_txid).await?;

    let statechain_info = utils::get_statechain_info(&transfer_msg.statechain_id, &client_config)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Statechain info not found"))?;

    // The coin books the CHAIN's view of its funding output. A ladder over a funding output that
    // is still in the mempool is a perfectly good coin — its exit is signed and needs no
    // confirmation to be valid — and the status machine walks it to CONFIRMED like any deposit.
    let (_, tx0_status) = verify_tx0_output_is_unspent_and_confirmed(
        &client_config.electrum_client,
        &tx0_outpoint,
        &tx0_hex,
        &network,
        client_config.confirmation_target,
    )
    .await?;

    // PERSIST THE LADDER BEFORE THE HANDOVER. Once the coordinator rotates its share the coin is
    // ours, and a coin of ours with no ladder row has no exit at all. A row written for a handover
    // that then fails is harmless — it names a coin this wallet does not hold, and a later
    // successful claim overwrites it. The previous order (a best-effort write AFTER the handover)
    // could adopt a coin whose only exit material was never written to disk.
    crate::tesr::persist(client_config, wallet_name, &bundle).await.map_err(|e| {
        anyhow::anyhow!(
            "refusing to claim {}: its exit ladder could not be written to this wallet ({e}). A \
             coin adopted without its ladder row would have no exit material at all.",
            transfer_msg.statechain_id
        )
    })?;

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
    // `locktime` stays None ON PURPOSE, for a root exactly as for a child: a laddered coin has no
    // absolute-locktime backup and therefore no calendar. Setting one would make every deadline
    // pass read a phantom clock.
    coin.locktime = None;
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

    transfer_receive_result.is_batch_locked = false;
    transfer_receive_result.statechain_id = Some(transfer_msg.statechain_id.clone());

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

    /// **[D75] THE CHILD LANE ADMITS EXACTLY ONE SHAPE.**
    ///
    /// Both child gates — `prepay_child_census` (the SSP's pre-pay path, which precedes an
    /// irreversible Lightning leg) and `validate_encrypted_message`'s child block (the claim path) —
    /// previously had NO `admissible_shape` call and gated with `<`, so every value in
    /// `[SHAPE_CHILD, u32::MAX]` cleared them. Inert, because an unknown value selected the same arms
    /// shape 4 does — and exactly the ordinal reading [D16] forbids.
    ///
    /// **Scope, stated so this is not mistaken for more than it is** ([D64]): this exercises the
    /// PREDICATE COMPOSITION the two gates now run, not the gates themselves — both are `async` and
    /// take a live `ClientConfig`. What proves the composition is actually ON those paths is
    /// `the_child_lane_gates_check_the_shape_before_parsing_the_bundle` (a presence-and-ordering
    /// scan, which is what a source scan may assert), and what proves honest traffic still passes is
    /// the live child suite — `sdk17`, `sdk59`, `sdk60`.
    #[test]
    fn the_child_lane_admits_exactly_shape_four() {
        let gate = |v: u32| -> Result<()> {
            admissible_shape(v)?;
            if v != SHAPE_CHILD {
                return Err(anyhow!("shape {v} is not the child shape {SHAPE_CHILD}"));
            }
            Ok(())
        };
        assert!(gate(SHAPE_CHILD).is_ok(), "the child shape itself must be admitted");
        for v in [0u32, 1, 2, 3, 5, 99, u32::MAX] {
            assert!(
                gate(v).is_err(),
                "shape {v} must be refused on the child lane — under the old `<` gate every value \
                 at or above {SHAPE_CHILD} passed"
            );
        }
        // The two halves refuse for DIFFERENT reasons, and both messages must name the offender.
        let unknown = gate(99).unwrap_err().to_string();
        assert!(unknown.contains("99"), "an unknown shape must be named: {unknown}");
        let known_wrong = gate(SHAPE_ROOT_LADDER).unwrap_err().to_string();
        assert!(
            known_wrong.contains("2") && known_wrong.contains(&SHAPE_CHILD.to_string()),
            "a KNOWN but wrong shape must name both it and the expected one: {known_wrong}"
        );
    }

    /// **[D75] …and the check sits AHEAD of the bundle parse on both child gates.**
    ///
    /// Presence and ordering only — [D64]'s ceiling. It cannot prove the gates are reachable (the
    /// live child suite does that); what it stops is the specific regression that existed until now,
    /// where the claim path's child block `return`ed before the function's only shape check.
    #[test]
    fn the_child_lane_gates_check_the_shape_before_parsing_the_bundle() {
        let src = include_str!("transfer_receiver.rs");
        for (gate, parse) in [
            ("pre-pay census: a child bundle was conveyed", "pre-pay census: malformed child TES-R bundle"),
            ("a child bundle was conveyed under protocol_version", "malformed child TES-R bundle"),
        ] {
            let at = src.find(gate).unwrap_or_else(|| panic!("child gate not found: {gate}"));
            let head = &src[..at];
            let call = head.rfind("admissible_shape(transfer_msg.protocol_version)")
                .unwrap_or_else(|| panic!("no admissible_shape ahead of: {gate}"));
            assert!(call < at, "the shape check must precede the refusal it guards: {gate}");
            let parsed = src[at..].find(parse).unwrap_or_else(|| panic!("parse site not found: {parse}"));
            assert!(parsed > 0, "the bundle parse must follow the shape check: {parse}");
        }
    }

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
        // The seam that revealed the category error: the child gate used to be 3.
        assert!(
            !ADMISSIBLE_PROTOCOL_VERSIONS.contains(&3),
            "3 was a floor over a set containing no 3; if 3 is admissible again this test is stale"
        );
    }
}
