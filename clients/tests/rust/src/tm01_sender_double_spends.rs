use std::{env, process::Command, thread, time::Duration};
use anyhow::{Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};

async fn tm01(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet, wallet3: &Wallet) -> Result<()> {

    let amount = 1000;

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    let _ = bitcoin_core::sendtoaddress(amount, &address)?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    let batch_id = None;

    let force_send = false;

    let statechain_id = new_coin.statechain_id.as_ref().unwrap();

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;

    // Setup, not the test: this send only has to succeed so that there is something to double-spend.
    assert!(
        result.is_ok(),
        "TM01 - [1] the FIRST send of SC={statechain_id} to wallet2 must succeed — it is the setup \
         for the double-spend, not the property under test. Failed with: {:?}",
        result.as_ref().err()
    );
    println!("TM01 - [1] wallet1 conveyed SC={} to wallet2 (unclaimed)", &statechain_id[..8]);

    let wallet3_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet3.name).await?;

    let batch_id = None;

    // ---- REDIRECTING AN UN-RECEIVED TRANSFER -------------------------------------------------
    //
    // This test used to assert that wallet1 could simply send the coin AGAIN, to a different
    // recipient, and that the second conveyance would silently overwrite the first. That is no
    // longer the protocol. The coordinator now holds an OPEN-TRANSFER LOCK: while a conveyed
    // transfer is outstanding, the SE refuses further co-signatures on that coin.
    //
    // The lock is the point, not an obstacle to route around. Silent overwrite meant a recipient
    // who had been handed a transfer could have it revoked underneath them with no record and no
    // say. Redirection is still possible — it is now EXPLICIT: cancel, with the current
    // recipient's consent, and only then send elsewhere.
    //
    // So the sequence below tests three things the old single assertion could not:
    //   [2a] the lock actually refuses the silent overwrite,
    //   [2b] the recipient can consent to releasing it,
    //   [2c] once cancelled, the coin is spendable again and the redirect goes through.
    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet3_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;

    assert!(
        result.is_err(),
        "TM01 - [2a] re-conveying SC={statechain_id} to wallet3 while the transfer to wallet2 is \
         still open must be REFUSED. It was accepted, which means a conveyed transfer can be \
         revoked out from under its recipient with no record — the exact thing the open-transfer \
         lock exists to prevent."
    );
    let err = result.err().unwrap().to_string();
    assert!(
        err.contains("open transfer"),
        "TM01 - [2a] refused, but NOT by the open-transfer lock. Any other error (unreachable \
         server, timeout, locked db) satisfies is_err() too and would leave the lock untested. \
         Got: {err}"
    );
    println!("TM01 - [2a] silent overwrite REFUSED by the open-transfer lock: {err}");

    // The recipient's half. wallet2 is a separate wallet with its own auth key, so wallet1 cannot
    // produce this consent — it must come from the side that would lose the coin. (Both wallets
    // happen to live in one local db here; the keys are still distinct, and the sender-only path is
    // refused for exactly that reason.)
    let preview = mercuryrustlib::transfer_sender::preview_cancel_consent(&client_config, &wallet2.name, &statechain_id).await?;
    assert_eq!(
        preview.statechain_id(), statechain_id.as_str(),
        "TM01 - [2b] wallet2's consent preview must describe the coin under cancellation, not some \
         other pending transfer — the token is bound to the transfer it previews."
    );
    let recipient_auth_pub_key = preview.recipient_auth_pub_key().to_string();
    let consent = mercuryrustlib::transfer_sender::cancel_consent(&client_config, &wallet2.name, &preview).await?;

    println!("TM01 - [2b] wallet2 (the current recipient) consented to cancelling SC={}", &statechain_id[..8]);

    let outcome = mercuryrustlib::transfer_sender::cancel_with_consent(
        &client_config, &wallet1.name, &statechain_id, &recipient_auth_pub_key, &consent,
    ).await?;
    assert_eq!(
        outcome, mercuryrustlib::transfer_sender::CancelOutcome::Cancelled,
        "TM01 - [2b] the consented cancellation of SC={statechain_id} must report a FRESH \
         cancellation. `AlreadyCancelled` here would mean this transfer was withdrawn by something \
         other than this call."
    );
    println!("TM01 - [2b] wallet1 cancelled the transfer to wallet2 with that consent");

    // The cancellation releases the coordinator's lock; the local coin still reads IN_TRANSFER
    // until it is resynced. tb05 does the same between its cancel and its re-convey.
    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let batch_id = None;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet3_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;

    assert!(
        result.is_ok(),
        "TM01 - [2c] after the cancellation, re-conveying SC={statechain_id} to wallet3 must \
         succeed: the coin is wallet1's again and no transfer is open on it. If this still fails, \
         the cancel released the coordinator's record but NOT the coin — the redirect path is \
         broken and a cancelled transfer strands the coin. Failed with: {:?}",
        result.as_ref().err()
    );
    println!("TM01 - [2c] lock released; wallet1 redirected SC={} to wallet3", &statechain_id[..8]);

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet3.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    assert!(received_statechain_ids.len() == 1);

    assert!(received_statechain_ids[0] == statechain_id.to_string());

    let batch_id = None;

    // this second "double spend" is not legitimate, as the statecoin has already been received by wallet3
    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;

    // A negative test that accepts ANY error is green when the stack is simply down. Both halves
    // matter: that it was refused, AND that it was refused for the reason this test claims to prove.
    assert!(
        result.is_err(),
        "TM01 - [3] the ILLEGITIMATE double spend must be REFUSED: wallet3 has ALREADY received \
         SC={statechain_id}, so re-conveying it to wallet2 is theft of a spent coin. It was accepted."
    );
    let err = result.err().unwrap().to_string();
    assert!(
        err.contains("Signature does not match authentication key"),
        "TM01 - [3] refused, but for the WRONG reason. The refusal must come from the auth-key check \
         — wallet3's receive rotated the coin's authentication key, so wallet1 can no longer \
         authenticate against it. Any other error (server unreachable, database locked, timeout) \
         means this test passed without exercising the property at all. Got: {err}"
    );
    println!("TM01 - [3] stale-key double spend REFUSED as expected: {err}");

    // If we update wallet1, the error will happen when we try to send the coin to wallet2
    // The step above tested that the sender can double spend the coin, but the server will not accept it

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let batch_id = None;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;

    // Same coin, but wallet1 has now resynced and KNOWS it no longer holds it. The refusal must
    // therefore come from wallet1's own local coin state, not from the server — a different
    // property from [3], and one that would be indistinguishable if either assertion took any error.
    assert!(
        result.is_err(),
        "TM01 - [4] after resync, wallet1 knows it no longer holds SC={statechain_id} and must \
         refuse LOCALLY, before any network call. It was accepted."
    );
    let err = result.err().unwrap().to_string();
    assert!(
        err.contains("No coins with status CONFIRMED or IN_TRANSFER associated with this statechain ID were found"),
        "TM01 - [4] refused, but for the WRONG reason. The refusal must be wallet1's LOCAL coin-state \
         check after resync — if it instead came back from the server, the client is leaking spent \
         coins into send attempts and the local guard is not doing its job. Got: {err}"
    );
    println!("TM01 - [4] post-resync send REFUSED locally as expected: {err}");

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

    let wallet3 = mercuryrustlib::wallet::create_wallet(
        "wallet3", 
        &client_config).await?;

    mercuryrustlib::sqlite_manager::insert_wallet(&client_config.pool, &wallet3).await?;

    tm01(&client_config, &wallet1, &wallet2, &wallet3).await?;

    println!("TM01 - Sender Double Spends Test completed successfully");

    Ok(())
}
