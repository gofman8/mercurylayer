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
async fn verify_terminal_parents(client_config: &ClientConfig, parents: &[String]) -> Result<()> {
    if parents.is_empty() {
        return Ok(());
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
            verify_terminal_parents(client_config, &transfer_msg.terminal_parents).await?;
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

        if statechain_info.num_sigs != transfer_msg.backup_transactions.len() as u32 {
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
    // Consensus-verify every branch tx against its prevouts (signatures + scripts).
    for tx in &txs {
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
            let confirmations = blockheight - unspent.height + 1;

            if confirmations as u32 >= confirmation_target {
                status = CoinStatus::CONFIRMED;
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