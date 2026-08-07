use std::{env, process::Command, thread, time::Duration};

use anyhow::{Result, Ok};
use electrum_client::ElectrumApi;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};

pub async fn old_state_broadcasted(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let amount = 1000;

    // Create first deposit address

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

    let wallet1_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet1.name).await?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::CONFIRMED).unwrap();
    let statechain_id_1 = new_coin.statechain_id.as_ref().unwrap();

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet1_transfer_adress, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;

    assert!(result.is_ok(), "transfer_sender::execute failed: {:?}", result.as_ref().err());

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    // The coin now has an OPEN transfer (to wallet1's own address, never claimed). The coordinator's
    // pending-transfer lock makes the SE refuse every further co-signature of this coin, so a second
    // transfer cannot even be signed. That lock is load-bearing — it is the only thing stopping a
    // still-owner sender from co-signing a rival state while a conveyed recipient holds claimable
    // material — so assert it is really there before asserting it can be released.
    let blocked = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;

    assert!(blocked.is_err(), "a coin with an open transfer must not be co-signable");
    let blocked_msg = blocked.unwrap_err().to_string();
    assert!(
        blocked_msg.contains("coin has an open transfer"),
        "expected the pending-transfer lock to refuse; got: {blocked_msg}"
    );

    // Cancel the abandoned transfer. It WAS conveyed (transfer_sender posts the mailbox message), so
    // the recorded recipient must co-sign the release — and here wallet1 is that recipient, because
    // it addressed the transfer to itself. This is not a sender-only cancel; it is the same consent
    // rule, satisfied by a wallet that holds both keys.
    let cancel_result = mercuryrustlib::transfer_sender::cancel(&client_config, &wallet1.name, &statechain_id_1.clone()).await;

    assert!(cancel_result.is_ok(), "transfer_sender::cancel failed: {:?}", cancel_result.as_ref().err());
    assert_eq!(
        cancel_result.unwrap(),
        mercuryrustlib::transfer_sender::CancelOutcome::Cancelled
    );

    // Cancelling twice is idempotent, not an error: cancellation is irreversible, so a client that
    // retries after a dropped response must not be told something different the second time.
    let again = mercuryrustlib::transfer_sender::cancel(&client_config, &wallet1.name, &statechain_id_1.clone()).await;
    assert!(again.is_ok(), "a repeated cancel must be idempotent: {:?}", again.as_ref().err());
    assert_eq!(
        again.unwrap(),
        mercuryrustlib::transfer_sender::CancelOutcome::AlreadyCancelled
    );

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    // The lock is released: the coin is spendable again.
    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;

    assert!(result.is_ok(), "transfer_sender::execute failed: {:?}", result.as_ref().err());

    // ADVERSARIAL, on the live stack: the coin is now conveyed to WALLET2, which has not claimed it.
    // wallet1 must not be able to take it back — it does not hold wallet2's recipient key, and
    // without that key nothing distinguishes "wallet2 has not downloaded the message" from "wallet2
    // downloaded it and is about to claim". A sender-only cancel here is exactly the two-victim
    // break (convey to Bob, cancel, convey to Carol), so it must be refused BY NAME.
    let refused = mercuryrustlib::transfer_sender::cancel(&client_config, &wallet1.name, &statechain_id_1.clone()).await;

    assert!(refused.is_err(), "a sender must NOT be able to cancel a transfer conveyed to someone else");
    let refused_msg = refused.unwrap_err().to_string();
    assert!(
        refused_msg.contains("the recipient must co-sign the cancellation"),
        "expected the recipient-consent refusal; got: {refused_msg}"
    );

    // ... and the refusal left the transfer intact: the coin is still locked, so wallet1 cannot
    // co-sign a rival state while wallet2 holds claimable material.
    let wallet1_second_address = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet1.name).await?;
    let still_locked = mercuryrustlib::transfer_sender::execute(&client_config, &wallet1_second_address, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;
    assert!(still_locked.is_err(), "a refused cancel must leave the pending-transfer lock in place");

    let backup_transactions = mercuryrustlib::sqlite_manager::get_backup_txs(&client_config.pool, &wallet1.name, &statechain_id_1).await?;

    assert!(backup_transactions.len() == 3);

    let bkp_tx = backup_transactions.get(1).unwrap();

    assert!(bkp_tx.tx_n == 2);

    let tx_bytes = hex::decode(&bkp_tx.tx)?;
    let txid = client_config.electrum_client.transaction_broadcast_raw(&tx_bytes);

    assert!(txid.is_err());

    let tx_lock_time = mercuryrustlib::get_blockheight(&bkp_tx)?;

    let current_blockheight = electrs::get_blockheight(&client_config).await? as u32;

    let height_diff = tx_lock_time - current_blockheight;

    let _ = bitcoin_core::generatetoaddress(height_diff, &core_wallet_address)?;

    let txid = client_config.electrum_client.transaction_broadcast_raw(&tx_bytes);

    assert!(txid.is_ok());

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

    old_state_broadcasted(&client_config, &wallet1, &wallet2).await?;

    println!("TB05 - Timelock tests completed successfully");

    Ok(())
}