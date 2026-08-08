
use std::{env, process::Command, thread, time::Duration};

use anyhow::{anyhow, Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};

async fn try_to_send_unconfirmed_coin(client_config: &ClientConfig, to_address: &str, wallet: &Wallet, statechain_id: &str) -> Result<()> {

    let batch_id = None;

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, to_address, &wallet.name, &statechain_id, None, force_send, batch_id).await;

    assert!(result.is_err());

    let error = result.err().unwrap();

    let error_message = format!("No coins with status CONFIRMED or IN_TRANSFER associated with this statechain ID were found");

    assert!(error.to_string() == error_message);

    println!("TB01 - [3/4] send of not-yet-CONFIRMED coin SC={} correctly rejected: {}", &statechain_id[..8.min(statechain_id.len())], error_message);

    Ok(())
}

async fn sucessfully_transfer(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let amount = 1000;

    let address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    // let mut coins_json = Vec::new();

    // for coin in wallet.coins.iter() {
    //     let obj = json!({
    //         "coin.user_pubkey": coin.user_pubkey,
    //         "coin.aggregated_address": coin.aggregated_address.as_ref().unwrap_or(&"".to_string()),
    //         "coin.address": coin.address,
    //         "coin.statechain_id": coin.statechain_id.as_ref().unwrap_or(&"".to_string()),
    //         "coin.amount": coin.amount.unwrap_or(0),
    //         "coin.status": coin.status,
    //         "coin.locktime": coin.locktime.unwrap_or(0),
    //     });

    //     coins_json.push(obj);
    // }

    // let coins_json_string = serde_json::to_string_pretty(&coins_json).unwrap();
    // println!("{}", coins_json_string);

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone()));

    if new_coin.is_none() {
        return Err(anyhow!("Coin not found in wallet"));
    }

    let new_coin = new_coin.unwrap();

    assert!(new_coin.status == CoinStatus::INITIALISED);
    assert!(new_coin.amount == Some(amount));
    assert!(new_coin.statechain_id.is_some());

    println!("TB01 - [1] deposit address {} INITIALISED for {} sats in {} (SC={})",
        &address[..12.min(address.len())],
        amount,
        wallet1.name,
        new_coin.statechain_id.as_ref().map(|s| s[..8.min(s.len())].to_string()).unwrap_or_default());

    let _ = bitcoin_core::sendtoaddress(amount, &address)?;

    println!("TB01 - [2] sent {} sats on-chain to {}, waiting for electrs to index", amount, &address[..12.min(address.len())]);

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    println!("TB01 - [2] electrs indexed the deposit tx for {}", &address[..12.min(address.len())]);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::IN_MEMPOOL);

    println!("TB01 - [2] coin at {} is IN_MEMPOOL", &address[..12.min(address.len())]);

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    let statechain_id = new_coin.statechain_id.as_ref().unwrap();

    println!("TB01 - [3] {} issued transfer address {} for SC={}", wallet2.name, &wallet2_transfer_adress[..12.min(wallet2_transfer_adress.len())], &statechain_id[..8.min(statechain_id.len())]);

    try_to_send_unconfirmed_coin(&client_config, &wallet2_transfer_adress, &wallet1, statechain_id).await?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(1, &core_wallet_address)?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::UNCONFIRMED);

    println!("TB01 - [4] mined 1 block; coin SC={} is UNCONFIRMED (target {} confirmations)", &statechain_id[..8.min(statechain_id.len())], client_config.confirmation_target);

    try_to_send_unconfirmed_coin(&client_config, &wallet2_transfer_adress, &wallet1, statechain_id).await?;

    let remaining_blocks = client_config.confirmation_target - 1;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    println!("TB01 - [5] mined {} remaining blocks to reach the confirmation target", remaining_blocks);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    println!("TB01 - [5] coin SC={} ({} sats) is CONFIRMED and sendable", &statechain_id[..8.min(statechain_id.len())], amount);

    let batch_id = None;

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet.name, &statechain_id, None, force_send, batch_id).await;

    assert!(result.is_ok());

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::IN_TRANSFER);

    println!("TB01 - [6] {} sent SC={} to {} ({}); sender coin now IN_TRANSFER", wallet1.name, &statechain_id[..8.min(statechain_id.len())], wallet2.name, &wallet2_transfer_adress[..12.min(wallet2_transfer_adress.len())]);

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet2.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    assert!(received_statechain_ids.contains(&statechain_id.to_string()));
    assert!(received_statechain_ids.len() == 1);

    println!("TB01 - [7] {} received exactly {} statechain id, including SC={}", wallet2.name, received_statechain_ids.len(), &statechain_id[..8.min(statechain_id.len())]);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::TRANSFERRED);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet2.name).await?;
    let local_wallet_2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;
    let new_coin = local_wallet_2.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    println!("TB01 - [7] SC={} is TRANSFERRED for {} and CONFIRMED for {}", &statechain_id[..8.min(statechain_id.len())], wallet1.name, wallet2.name);

    let fee_rate = None;

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet2.name, &statechain_id, &core_wallet_address, fee_rate, None).await;

    assert!(result.is_ok());

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet2.name).await?;
    let local_wallet_2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;
    let new_coin = local_wallet_2.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::WITHDRAWING);

    println!("TB01 - [8] {} broadcast withdrawal of SC={} to {}; coin now WITHDRAWING", wallet2.name, &statechain_id[..8.min(statechain_id.len())], &core_wallet_address[..12.min(core_wallet_address.len())]);

    let _ = bitcoin_core::generatetoaddress(client_config.confirmation_target, &core_wallet_address)?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet2.name).await?;
    let local_wallet_2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;
    let new_coin = local_wallet_2.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::WITHDRAWN);

    println!("TB01 - [8] mined {} blocks; SC={} is WITHDRAWN ({} sats left the statechain)", client_config.confirmation_target, &statechain_id[..8.min(statechain_id.len())], amount);

    Ok(())
}

pub async fn execute() -> Result<()> {

    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output().expect("failed to execute process");

    env::set_var("ML_NETWORK", "regtest");

    let client_config = mercuryrustlib::client_config::load().await;

    let wallet1 = mercuryrustlib::wallet::create_wallet(
        "wallet1", 
        &client_config).await?;

    mercuryrustlib::sqlite_manager::insert_wallet(&client_config.pool, &wallet1).await?;

    let wallet2 = mercuryrustlib::wallet::create_wallet(
        "wallet2", 
        &client_config).await?;

    mercuryrustlib::sqlite_manager::insert_wallet(&client_config.pool, &wallet2).await?;

    println!("TB01 - [0] created wallets {} and {} on regtest (fresh wallet.db)", wallet1.name, wallet2.name);

    sucessfully_transfer(&client_config, &wallet1, &wallet2).await?;

    println!("TB01 - Transfer completed successfully");

    Ok(())
}
