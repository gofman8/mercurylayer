use std::{env, process::Command, thread, time::Duration};
use anyhow::{Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};


async fn tb02(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let amount = 1000;

    // Create first deposit address

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    let _ = bitcoin_core::sendtoaddress(amount, &address)?;

    println!("TB02 - [1] first deposit funded {} sats to {}... (wallet1)", amount, &address[..12]);

    // Create second deposit address

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    let _ = bitcoin_core::sendtoaddress(amount, &address)?;

    println!("TB02 - [1] second deposit funded {} sats to {}... (wallet1)", amount, &address[..12]);

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    println!("TB02 - [2] mined {} blocks to {}...; waiting for electrs to index {}...", remaining_blocks, &core_wallet_address[..12], &address[..12]);

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    println!("TB02 - [2] electrs indexed the deposit address");

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    assert!(wallet1.coins.len() == 2);

    for coin in wallet1.coins.iter() {
        assert!(coin.status == CoinStatus::CONFIRMED);
    }

    println!("TB02 - [3] wallet1 booked {} coins, all CONFIRMED", wallet1.coins.len());

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    println!("TB02 - [4] wallet2 issued ONE transfer address {}... to be reused by both sends", &wallet2_transfer_adress[..12]);

    for coin in wallet1.coins.iter() {
        let batch_id = None;

        let force_send = false;

        let statechain_id = coin.statechain_id.as_ref().unwrap();

        let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;

        assert!(
            result.is_ok(),
            "TB02 - [4]: sending coin SC={statechain_id} ({} sats) from wallet1 to the reused transfer address {wallet2_transfer_adress} must succeed — reusing one recipient address across two different statechain_id transfers is exactly the address-reuse behavior this test verifies. Failed with: {:?}",
            coin.amount.unwrap_or(0),
            result.as_ref().err()
        );

        println!("TB02 - [4] wallet1 -> wallet2 sent SC={}... ({} sats) to the reused address", &statechain_id[..8], coin.amount.unwrap_or(0));
    }

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet2.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    println!("TB02 - [5] wallet2 receive claimed {} statechain ids from the single reused address", received_statechain_ids.len());

    let wallet2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;

    assert!(wallet2.coins.len() == 2);

    for coin in wallet2.coins.iter() {
        assert!(coin.status == CoinStatus::CONFIRMED);
        assert!(received_statechain_ids.contains(&coin.statechain_id.as_ref().unwrap().clone()));
    }

    for i in 0..wallet2.coins.len() {
        for j in (i + 1)..wallet2.coins.len() {
            assert_eq!(wallet2.coins[i].user_privkey, wallet2.coins[j].user_privkey, "user_privkey mismatch");
            assert_eq!(wallet2.coins[i].auth_privkey, wallet2.coins[j].auth_privkey, "auth_privkey mismatch");
            assert_eq!(wallet2.coins[i].address, wallet2.coins[j].address, "address mismatch");

            assert_ne!(wallet2.coins[i].server_pubkey, wallet2.coins[j].server_pubkey, "server_pubkey should differ");
            assert_ne!(wallet2.coins[i].statechain_id, wallet2.coins[j].statechain_id, "statechain_id should differ");
            assert_ne!(wallet2.coins[i].aggregated_address, wallet2.coins[j].aggregated_address, "aggregated_address should differ");
        }
    }

    println!("TB02 - [5] address reuse verified across {} wallet2 coins: same user/auth privkey + address, distinct server_pubkey/statechain_id/aggregated_address", wallet2.coins.len());

    let statechain_id = wallet2.coins[0].statechain_id.as_ref().unwrap().clone();

    let fee_rate = None;

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet2.name, &statechain_id, &core_wallet_address, fee_rate, None).await;

    assert!(
        result.is_ok(),
        "TB02 - [6]: wallet2 withdrawing SC={statechain_id} (coin[0] of the reused-address pair) to {core_wallet_address} must succeed. Failed with: {:?}",
        result.as_ref().err()
    );

    let wallet2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;

    assert!(wallet2.coins[0].status == CoinStatus::WITHDRAWING);

    println!("TB02 - [6] wallet2 withdrew SC={}... to {}...; coin[0] now WITHDRAWING", &statechain_id[..8], &core_wallet_address[..12]);

    let _ = bitcoin_core::generatetoaddress(client_config.confirmation_target, &core_wallet_address)?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet2.name).await?;
    let wallet2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;

    assert!(wallet2.coins[0].status == CoinStatus::WITHDRAWN);

    println!("TB02 - [6] mined {} blocks; wallet2 coin[0] SC={}... reached WITHDRAWN", client_config.confirmation_target, &statechain_id[..8]);

    let wallet1_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet1.name).await?;

    let batch_id = None;

    let statechain_id = wallet2.coins[1].statechain_id.as_ref().unwrap().clone();

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet1_transfer_adress, &wallet2.name, &statechain_id, None, force_send, batch_id).await;

    assert!(
        result.is_ok(),
        "TB02 - [7]: wallet2 sending the surviving coin SC={statechain_id} (coin[1] of the reused-address pair, wallet2.coins[0] having already been withdrawn) to wallet1 at {wallet1_transfer_adress} must succeed. Failed with: {:?}",
        result.as_ref().err()
    );

    println!("TB02 - [7] wallet2 -> wallet1 sent the surviving coin SC={}... to {}...", &statechain_id[..8], &wallet1_transfer_adress[..12]);

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet1.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    assert!(received_statechain_ids.contains(&statechain_id.to_string()));
    assert!(received_statechain_ids.len() == 1);

    println!("TB02 - [7] wallet1 received exactly 1 statechain id back: SC={}...", &statechain_id[..8]);

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet1.name, &statechain_id, &core_wallet_address, fee_rate, None).await;

    assert!(
        result.is_ok(),
        "TB02 - [8]: wallet1 withdrawing the received coin SC={statechain_id} to {core_wallet_address} must succeed. Failed with: {:?}",
        result.as_ref().err()
    );

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let withdrawn_coin = wallet1.coins.iter().find(|&coin| coin.statechain_id == Some(statechain_id.to_string()) && coin.status == CoinStatus::WITHDRAWING);
    let transferred_coin = wallet1.coins.iter().find(|&coin| coin.statechain_id == Some(statechain_id.to_string()) && coin.status == CoinStatus::TRANSFERRED);

    assert!(withdrawn_coin.is_some());
    assert!(transferred_coin.is_some());

    println!("TB02 - [8] wallet1 withdrew SC={}...; both the WITHDRAWING and the older TRANSFERRED duplicate rows are present ({} coins in wallet1)", &statechain_id[..8], wallet1.coins.len());

    let _ = bitcoin_core::generatetoaddress(client_config.confirmation_target, &core_wallet_address)?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let withdrawn_coin = wallet1.coins.iter().find(|&coin| coin.statechain_id == Some(statechain_id.to_string()) && coin.status == CoinStatus::WITHDRAWN);

    assert!(withdrawn_coin.is_some());

    println!("TB02 - [8] mined {} blocks; wallet1 coin SC={}... reached WITHDRAWN", client_config.confirmation_target, &statechain_id[..8]);

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

    tb02(&client_config, &wallet1, &wallet2).await?;

    println!("TB02 - Transfer Address Reuse completed successfully");

    Ok(())
}
