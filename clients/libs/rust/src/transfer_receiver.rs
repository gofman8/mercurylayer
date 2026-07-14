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
}

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

pub async fn execute(client_config: &ClientConfig, wallet_name: &str) -> Result<TransferReceiveResult>{

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

                let is_msg_valid = validate_encrypted_message(client_config, &coin, enc_message, &wallet.network, &info_config, blockheight).await;

                if is_msg_valid.is_err() {
                    println!("Validation error: {}", is_msg_valid.err().unwrap().to_string());
                    continue;
                }

                let message_result = process_encrypted_message(client_config, &mut coin, enc_message, &wallet.network, &wallet.name, &mut temp_activities).await;

                if message_result.is_err() {
                    println!("Processing error: {}", message_result.err().unwrap().to_string());
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

                let is_msg_valid = validate_encrypted_message(client_config, &new_coin, enc_message, &wallet.network, &info_config, blockheight).await;

                if is_msg_valid.is_err() {
                    println!("Validation error: {}", is_msg_valid.err().unwrap().to_string());
                    continue;
                }

                let message_result = process_encrypted_message(client_config, &mut new_coin, enc_message, &wallet.network, &wallet.name, &mut temp_activities).await;

                if message_result.is_err() {
                    println!("Processing error: {}", message_result.err().unwrap().to_string());
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

    Ok(TransferReceiveResult{
        is_there_batch_locked,
        received_statechain_ids
    })
}

async fn get_msg_addr(auth_pubkey: &str, client_config: &ClientConfig) -> Result<Vec<String>> {

    let path = format!("transfer/get_msg_addr/{}", auth_pubkey.to_string());

    let client = client_config.get_reqwest_client()?;
    let request = client.get(&format!("{}/{}", client_config.statechain_entity, path));

    let value = request.send().await?.text().await?;

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
}

pub async fn peek_pending_transfers(
    client_config: &ClientConfig,
    wallet_name: &str,
) -> Result<Vec<PendingTransferInfo>> {
    let wallet = get_wallet(&client_config.pool, wallet_name).await?;

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
            let groups = split_backup_transactions(&transfer_msg.backup_transactions);
            let mut funding_txid = String::new();
            let mut funding_vout = 0u32;
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
            out.push(PendingTransferInfo {
                statechain_id: transfer_msg.statechain_id,
                amount,
                rgb_consignment,
                funding_txid,
                funding_vout,
                branch_txs: transfer_msg.branch_txs.clone(),
            });
        }
    }
    Ok(out)
}

pub fn split_backup_transactions(backup_transactions: &Vec<BackupTx>) -> Vec<Vec<BackupTx>> {
    // HashMap to store grouped transactions
    let mut grouped_txs: HashMap<(String, u32), Vec<BackupTx>> = HashMap::new();
    
    // Vector to keep track of order of appearance of outpoints
    let mut order_of_appearance: Vec<(String, u32)> = Vec::new();
    // HashSet to track which outpoints we've seen
    let mut seen_outpoints: HashSet<(String, u32)> = HashSet::new();
    
    // Process each transaction
    for tx in backup_transactions {
        // Get the outpoint for this transaction
        let outpoint = get_previous_outpoint(&tx).expect("Valid outpoint");
        
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
    
    result
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

async fn validate_encrypted_message(client_config: &ClientConfig, coin: &Coin, enc_message: &str, network: &str, info_config: &InfoConfig, blockheight: u32) -> Result<()> {

    let client_auth_key = coin.auth_privkey.clone();
    let new_user_pubkey = coin.user_pubkey.clone();

    let transfer_msg = mercurylib::transfer::receiver::decrypt_transfer_msg(enc_message, &client_auth_key)?;

    let grouped_backup_transactions = split_backup_transactions(&transfer_msg.backup_transactions);

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
            // TES-R (Utexo V2) coin: verify the conveyed exit ladder and its EXACT sig-count via the
            // R′ verifier (crate::tesr::verify_bundle), which enforces
            // `se_num_sigs == v1_backups + tier_count` (no hidden co-signed state) plus a valid exit
            // chain — the V2 analogue of the flat V1 backup-count linchpin below.
            let ladder = transfer_msg
                .tesr_ladder
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("v2 transfer is missing its TES-R ladder"))?;
            let bundle: crate::tesr::TesrBundle = serde_json::from_str(ladder)
                .map_err(|e| anyhow::anyhow!("malformed TES-R ladder: {e}"))?;
            crate::tesr::verify_bundle(
                &bundle,
                statechain_info.num_sigs,
                transfer_msg.backup_transactions.len() as u32,
            )?;
        } else if statechain_info.num_sigs != transfer_msg.backup_transactions.len() as u32 {
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

        // The V1 decrementing-locktime backup ladder check applies only to V1 coins. A TES-R (V2)
        // coin does not use that ladder — its exit assurance is the tier ladder, already verified by
        // the R′ check (crate::tesr::verify_bundle) at the num_sigs gate above — so it is skipped here.
        if transfer_msg.protocol_version < 2 {
            let previous_lock_time = mercurylib::transfer::receiver::validate_signature_scheme(
                backup_transactions,
                &statechain_info,
                &tx0_hex,
                blockheight,
                client_config.fee_rate_tolerance,
                current_fee_rate_sats_per_byte,
                info_config.initlock,
                info_config.interval);

            if previous_lock_time.is_err() {
                let error = previous_lock_time.err().unwrap();
                return Err(anyhow!("Signature scheme validation failed. Error {}", error.to_string()));
            }
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

    let grouped_backup_transactions = split_backup_transactions(&transfer_msg.backup_transactions);

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
                Err(err) => {
                    return Err(anyhow::anyhow!("Error: {}", err.to_string()));
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
