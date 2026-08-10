use std::{env, process::Command, thread, time::Duration};

use anyhow::{Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};

async fn withdraw_flow(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet)  -> Result<()> {

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
    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    let amount = 2000;

    let _ = bitcoin_core::sendtoaddress(amount, &deposit_address)?;

    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::CONFIRMED);
    let duplicated_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::DUPLICATED);

    assert!(new_coin.is_some());
    assert!(duplicated_coin.is_some());

    let new_coin = new_coin.unwrap();
    let duplicated_coin = duplicated_coin.unwrap();

    assert!(new_coin.duplicate_index == 0);
    assert!(duplicated_coin.duplicate_index == 1);

    let statechain_id = new_coin.statechain_id.as_ref().unwrap();

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    let batch_id = None;

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, statechain_id, None, force_send, batch_id.clone()).await;

    assert!(
        result.is_err(),
        "TA02 - withdraw_flow: transferring statechain_id {statechain_id} out of wallet1 without --force must be REFUSED, because wallet1 also holds a DUPLICATED sibling coin (duplicate_index=1) for this same statechain_id and sending the non-duplicated coin away would strand that duplicate, causing PERMANENT LOSS of its funds. It was accepted instead."
    );

    let error_msg = result.err().unwrap().to_string();

    assert!(
        error_msg == "Coin is duplicated. If you want to proceed, use the command '--force, -f' option. \
        You will no longer be able to move other duplicate coins with the same statechain_id and this will cause PERMANENT LOSS of these duplicate coin funds.",
        "TA02 - withdraw_flow: transfer of statechain_id {statechain_id} was refused, but for the WRONG reason — expected the duplicate-coin force-flag guard in transfer_sender::execute (clients/libs/rust/src/transfer_sender.rs:991) to fire; any other error means this test proved nothing about duplicate-deposit protection. Got: {error_msg}"
    );

    let fee_rate = None;

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet1.name, statechain_id, &core_wallet_address, fee_rate, Some(1)).await;

    assert!(
        result.is_ok(),
        "TA02 - withdraw_flow: withdrawing the DUPLICATED coin (statechain_id {statechain_id}, duplicate_index=1) directly from wallet1 to {core_wallet_address} must succeed — withdrawing a duplicate outright (rather than transferring it) is exactly the recovery path the duplicate-coin guard points to. Failed with: {:?}",
        result.as_ref().err()
    );

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, statechain_id, None, force_send, batch_id).await;

    assert!(
        result.is_err(),
        "TA02 - withdraw_flow: after the duplicate coin (duplicate_index=1) for statechain_id {statechain_id} was withdrawn, transferring the remaining coin must still be REFUSED, because the recipient would compute a different signature count than the sender's coin now reflects — accepting this transfer would hand the recipient a coin they can never fully validate. It was accepted instead."
    );

    let error_msg = result.err().unwrap().to_string();

    assert!(
        error_msg == "There have been withdrawals of other coins with this same statechain_id (possibly duplicates).\
        This transfer cannot be performed because the recipient would reject it due to the difference in signature count.\
        This coin can be withdrawn, however.",
        "TA02 - withdraw_flow: transfer of statechain_id {statechain_id} was refused, but for the WRONG reason — expected the withdrawn-duplicate signature-count guard in transfer_sender::execute (clients/libs/rust/src/transfer_sender.rs:1002) to fire; any other error means this test proved nothing about duplicate-withdrawal protection. Got: {error_msg}"
    );

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet1.name, statechain_id, &core_wallet_address, fee_rate, None).await;

    assert!(
        result.is_ok(),
        "TA02 - withdraw_flow: withdrawing the remaining (non-duplicated) coin for statechain_id {statechain_id} from wallet1 to {core_wallet_address} must succeed, since the guard above only blocks TRANSFERRING it, not withdrawing it. Failed with: {:?}",
        result.as_ref().err()
    );

    Ok(())
}

async fn transfer_flow(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet)  -> Result<()> {

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
    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    let amount = 2000;

    let _ = bitcoin_core::sendtoaddress(amount, &deposit_address)?;

    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::CONFIRMED);
    let duplicated_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::DUPLICATED);

    assert!(new_coin.is_some());
    assert!(duplicated_coin.is_some());

    let new_coin = new_coin.unwrap();
    let duplicated_coin = duplicated_coin.unwrap();

    assert!(new_coin.duplicate_index == 0);
    assert!(duplicated_coin.duplicate_index == 1);

    let statechain_id = new_coin.statechain_id.as_ref().unwrap();

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    let batch_id = None;

    let force_send = true;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, statechain_id, None, force_send, batch_id.clone()).await;

    assert!(
        result.is_ok(),
        "TA02 - transfer_flow: transferring statechain_id {statechain_id} out of wallet1 WITH --force must succeed even though a DUPLICATED sibling coin (duplicate_index=1) shares this statechain_id, because force_send=true explicitly opts into overriding the duplicate-coin guard. Failed with: {:?}",
        result.as_ref().err()
    );

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet2.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    assert!(received_statechain_ids.contains(&statechain_id.to_string()));
    assert!(received_statechain_ids.len() == 1);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let transferred_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::TRANSFERRED);
    let duplicated_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::INVALIDATED);

    assert!(transferred_coin.is_some());
    assert!(duplicated_coin.is_some());

    let transferred_coin = transferred_coin.unwrap();
    let duplicated_coin = duplicated_coin.unwrap();

    assert!(transferred_coin.duplicate_index == 0);
    assert!(duplicated_coin.duplicate_index == 1);

    let fee_rate = None;

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet1.name, statechain_id, &core_wallet_address, fee_rate, Some(1)).await;

    assert!(
        result.is_err(),
        "TA02 - transfer_flow: after wallet1 force-transferred statechain_id {statechain_id} away, withdrawing 'duplicate_index=1' for it must be REFUSED — the force-send left that sibling coin INVALIDATED (not DUPLICATED), so there is no longer a duplicate row at index 1 to withdraw and accepting this would mean the duplicate-index bookkeeping is broken. It was accepted instead."
    );

    let error_msg = result.err().unwrap().to_string();

    // assert!(error_msg == "Signature does not match authentication key.");

    assert!(
        error_msg == "No duplicated coins associated with this statechain ID and index 1 were found",
        "TA02 - transfer_flow: withdraw of statechain_id {statechain_id} duplicate_index=1 was refused, but for the WRONG reason — expected the duplicated-index-not-found guard in withdraw::execute (clients/libs/rust/src/withdraw.rs:48) to fire; any other error means this test proved nothing about post-force-transfer duplicate-index bookkeeping. Got: {error_msg}"
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

    withdraw_flow(&client_config, &wallet1, &wallet2).await?;
    transfer_flow(&client_config, &wallet1, &wallet2).await?;

    println!("TA02 - Test \"Duplicate Deposits in the Same Adress\" completed successfully");

    Ok(())
}