use std::{env, process::Command, thread, time::Duration};

use anyhow::{Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};

async fn w1_transfer_to_w2(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let amount = 1000;

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let deposit_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    let _ = bitcoin_core::sendtoaddress(amount, &deposit_address)?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::CONFIRMED).unwrap();
    let statechain_id = new_coin.statechain_id.as_ref().unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    let batch_id = None;
    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;
    assert!(
        result.is_ok(),
        "TV01 - the setup send must succeed: wallet1 -> wallet2 for SC={statechain_id}. Everything \
         this test proves about stale-state broadcast depends on the transfer having happened. \
         Failed with: {:?}",
        result.as_ref().err()
    );

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet2.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    assert!(received_statechain_ids.contains(&statechain_id.to_string()));
    assert!(received_statechain_ids.len() == 1);
    
    mercuryrustlib::coin_status::update_coins(&client_config, &wallet2.name).await?;
    let local_wallet_2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;
    let new_w2_coin = local_wallet_2.coins.iter().find(|&coin| coin.statechain_id == Some(statechain_id.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);
    assert!(new_w2_coin.status == CoinStatus::CONFIRMED);

    let fee_rate = None;

    let core_wallet_address = Some(core_wallet_address);

    let result = mercuryrustlib::broadcast_backup_tx::execute(&client_config, &wallet1.name, &statechain_id, core_wallet_address.clone(), fee_rate).await;

    // THE SECURITY PROPERTY OF THIS FILE: wallet1 SENT the coin, so its retained backup is stale.
    // Broadcasting it must fail, and specifically it must fail because the node enforces the
    // TIMELOCK — that is what makes the previous owner harmless. Any other error means the
    // broadcast was blocked by something incidental, and the timelock was never tested at all.
    assert!(
        result.is_err(),
        "TV01 - the PREVIOUS owner's stale backup for SC={statechain_id} must NOT broadcast: \
         wallet1 already sent this coin to wallet2. It was accepted, which is a theft path."
    );

    let err = result.err().unwrap();

    let electrs_msg_err = "Electrum server error: {\"code\":2,\"message\":\"non-final\"}";

    let esplora_msg_err = r#"Electrum server error: "sendrawtransaction RPC error: {\"code\":-26,\"message\":\"non-final\"}""#;

    let err_msg = err.to_string();

    assert!(
        err_msg == electrs_msg_err || err_msg == esplora_msg_err,
        "TV01 - the stale backup was rejected, but NOT as `non-final`. Only a timelock rejection \
         proves the stale-state defence; an unreachable backend or a malformed tx would reject it \
         too and tell us nothing.\n  expected one of:\n    {electrs_msg_err}\n    {esplora_msg_err}\n\
           actual: {err_msg}"
    );

    let _ = bitcoin_core::generatetoaddress(990, &core_wallet_address.clone().unwrap())?;

    let result = mercuryrustlib::broadcast_backup_tx::execute(&client_config, &wallet2.name, &statechain_id, core_wallet_address, fee_rate).await;

    // The other half of the property: the CURRENT owner's backup must work once its timelock
    // matures. Without this the test would be satisfied by a system that simply rejects everything.
    assert!(
        result.is_ok(),
        "TV01 - the CURRENT owner's backup for SC={statechain_id} must broadcast after 990 blocks: \
         wallet2 holds the coin and its timelock has matured. If this fails, unilateral exit is \
         broken for the legitimate owner. Failed with: {:?}",
        result.as_ref().err()
    );

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

    w1_transfer_to_w2(&client_config, &wallet1, &wallet2).await?;

    println!("TV01 - Result as reported.");

    Ok(())
}