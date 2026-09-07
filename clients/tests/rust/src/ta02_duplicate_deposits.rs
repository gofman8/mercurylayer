//! Legacy sequence (TA02): **duplicate deposits to one aggregate address are NEVER conveyed.**
//!
//! A second UTXO paid to a coin's aggregate address is booked as a DUPLICATED sibling row
//! (`duplicate_index = 1`). A duplicate has no exit material of its own: a coin's only exit
//! material is its TES-R ladder, and a ladder is rooted at exactly ONE funding outpoint, so the
//! receiver's census has no slot for a second one. The old lane let the owner force the transfer
//! through (`--force`), riding the flat backup chain and INVALIDATING the sibling — that chain no
//! longer exists, so the force flag is inert and `transfer_sender::execute` refuses the transfer
//! by name whatever the flag says. The recovery path for a duplicate is a COOPERATIVE WITHDRAWAL
//! from the depositor's own wallet, which is what both flows below drive.
//!
//!   * `withdraw_flow`: transfer refused (duplicate present) → withdraw the duplicate (index 1)
//!     → transfer still refused (a withdrawn sibling would make the receiver's signature count
//!     disagree) → withdraw the index-0 coin. Both withdrawals succeed.
//!   * `transfer_flow`: the same with `force_send = true`: refused identically, nothing reaches the
//!     receiver, nothing is INVALIDATED, and after the duplicate is withdrawn a second withdrawal
//!     of index 1 is refused by the duplicate-index bookkeeping ("no duplicated coins ... index 1").

use std::{env, process::Command, thread, time::Duration};

use anyhow::{Ok, Result};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};

/// The refusal `transfer_sender::execute` returns for a coin with a DUPLICATED sibling, whatever
/// `force_send` says.
const DUPLICATE_REFUSAL: &str = "has duplicate deposits";

/// Deposit `first` sats to a fresh aggregate address of `wallet1`, confirm it, then pay `second`
/// sats to the SAME address and confirm that too. Returns `(deposit_address, statechain_id)` with
/// `wallet1` holding a CONFIRMED index-0 coin and a DUPLICATED index-1 sibling for it.
async fn stage_duplicate(
    client_config: &ClientConfig,
    wallet1: &Wallet,
    core_wallet_address: &str,
    first: u32,
    second: u32,
) -> Result<(String, String)> {
    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;
    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;
    let deposit_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(
        client_config,
        &wallet1.name,
        &token_id,
        first,
    )
    .await?;

    let _ = bitcoin_core::sendtoaddress(first, &deposit_address)?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, core_wallet_address)?;

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;
    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, first).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(client_config, &wallet1.name).await?;
    let wallet: mercuryrustlib::Wallet =
        mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet
        .coins
        .iter()
        .find(|&coin| coin.aggregated_address == Some(deposit_address.clone()))
        .unwrap();
    assert!(new_coin.status == CoinStatus::CONFIRMED);
    // A deposit's ladder is its only exit material, and it is established at first sight.
    let sid = new_coin.statechain_id.clone().unwrap();
    assert!(
        mercuryrustlib::tesr::load(client_config, &wallet1.name, &sid).await?.is_some(),
        "TA02: the index-0 deposit {sid} must carry its TES-R ladder before anything else happens"
    );

    let _ = bitcoin_core::sendtoaddress(second, &deposit_address)?;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, core_wallet_address)?;

    let mut is_tx_indexed = false;
    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, second).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet =
        mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet1.coins.iter().find(|&coin| {
        coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::CONFIRMED
    });
    let duplicated_coin = wallet1.coins.iter().find(|&coin| {
        coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::DUPLICATED
    });

    assert!(new_coin.is_some());
    assert!(duplicated_coin.is_some());

    let new_coin = new_coin.unwrap();
    let duplicated_coin = duplicated_coin.unwrap();

    assert!(new_coin.duplicate_index == 0);
    assert!(duplicated_coin.duplicate_index == 1);
    assert!(
        duplicated_coin.locktime.is_none() && new_coin.locktime.is_none(),
        "TA02: no coin carries an absolute locktime — there is no flat backup for one to come from"
    );

    Ok((deposit_address, sid))
}

async fn withdraw_flow(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {
    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let (_deposit_address, statechain_id) =
        stage_duplicate(client_config, wallet1, &core_wallet_address, 1000, 2000).await?;
    let statechain_id = statechain_id.as_str();

    let wallet2_transfer_adress =
        mercuryrustlib::transfer_receiver::new_transfer_address(client_config, &wallet2.name).await?;

    let batch_id = None;
    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(
        client_config,
        &wallet2_transfer_adress,
        &wallet1.name,
        statechain_id,
        None,
        force_send,
        batch_id.clone(),
    )
    .await;

    assert!(
        result.is_err(),
        "TA02 - withdraw_flow: transferring statechain_id {statechain_id} out of wallet1 must be REFUSED, because wallet1 also holds a DUPLICATED sibling coin (duplicate_index=1) for this same statechain_id — a ladder is rooted at exactly one funding outpoint, so the sibling can never be conveyed and sending the coin away would strand it. It was accepted instead."
    );

    let error_msg = result.err().unwrap().to_string();

    assert!(
        error_msg.contains(DUPLICATE_REFUSAL),
        "TA02 - withdraw_flow: transfer of statechain_id {statechain_id} was refused, but for the WRONG reason — expected the duplicate-deposit guard in transfer_sender::execute (\"{DUPLICATE_REFUSAL}\") to fire; any other error means this test proved nothing about duplicate-deposit protection. Got: {error_msg}"
    );

    let fee_rate = None;

    let result = mercuryrustlib::withdraw::execute(
        client_config,
        &wallet1.name,
        statechain_id,
        &core_wallet_address,
        fee_rate,
        Some(1),
    )
    .await;

    assert!(
        result.is_ok(),
        "TA02 - withdraw_flow: withdrawing the DUPLICATED coin (statechain_id {statechain_id}, duplicate_index=1) directly from wallet1 to {core_wallet_address} must succeed — withdrawing a duplicate outright is exactly the recovery path the duplicate-coin guard points to. Failed with: {:?}",
        result.as_ref().err()
    );

    mercuryrustlib::coin_status::update_coins(client_config, &wallet1.name).await?;

    let result = mercuryrustlib::transfer_sender::execute(
        client_config,
        &wallet2_transfer_adress,
        &wallet1.name,
        statechain_id,
        None,
        force_send,
        batch_id,
    )
    .await;

    assert!(
        result.is_err(),
        "TA02 - withdraw_flow: after the duplicate coin (duplicate_index=1) for statechain_id {statechain_id} was withdrawn, transferring the remaining coin must still be REFUSED, because the recipient would compute a different signature count than the sender's coin now reflects — accepting this transfer would hand the recipient a coin they can never fully validate. It was accepted instead."
    );

    let error_msg = result.err().unwrap().to_string();

    assert!(
        error_msg == "There have been withdrawals of other coins with this same statechain_id (possibly duplicates).\
        This transfer cannot be performed because the recipient would reject it due to the difference in signature count.\
        This coin can be withdrawn, however.",
        "TA02 - withdraw_flow: transfer of statechain_id {statechain_id} was refused, but for the WRONG reason — expected the withdrawn-duplicate signature-count guard in transfer_sender::execute to fire; any other error means this test proved nothing about duplicate-withdrawal protection. Got: {error_msg}"
    );

    let result = mercuryrustlib::withdraw::execute(
        client_config,
        &wallet1.name,
        statechain_id,
        &core_wallet_address,
        fee_rate,
        None,
    )
    .await;

    assert!(
        result.is_ok(),
        "TA02 - withdraw_flow: withdrawing the remaining (non-duplicated) coin for statechain_id {statechain_id} from wallet1 to {core_wallet_address} must succeed, since the guard above only blocks TRANSFERRING it, not withdrawing it. Failed with: {:?}",
        result.as_ref().err()
    );

    println!("TA02 - withdraw_flow: transfer refused by name with a duplicate present, the duplicate and then the coin were withdrawn cooperatively");

    Ok(())
}

async fn transfer_flow(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {
    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let (deposit_address, statechain_id) =
        stage_duplicate(client_config, wallet1, &core_wallet_address, 1000, 2000).await?;
    let statechain_id = statechain_id.as_str();

    let wallet2_transfer_adress =
        mercuryrustlib::transfer_receiver::new_transfer_address(client_config, &wallet2.name).await?;

    let batch_id = None;

    // The force flag used to override the duplicate guard and convey the coin over the flat
    // backup chain, INVALIDATING the sibling. There is no flat backup chain: the flag is inert and
    // the refusal is identical.
    let force_send = true;

    let result = mercuryrustlib::transfer_sender::execute(
        client_config,
        &wallet2_transfer_adress,
        &wallet1.name,
        statechain_id,
        None,
        force_send,
        batch_id.clone(),
    )
    .await;

    assert!(
        result.is_err(),
        "TA02 - transfer_flow: transferring statechain_id {statechain_id} out of wallet1 WITH force_send=true must be REFUSED exactly like the un-forced attempt — a duplicate carries no exit material and cannot ride a ladder conveyance, and there is no flat backup chain for a force flag to put it on. It was accepted instead."
    );

    let error_msg = result.err().unwrap().to_string();

    assert!(
        error_msg.contains(DUPLICATE_REFUSAL),
        "TA02 - transfer_flow: the forced transfer of statechain_id {statechain_id} was refused, but for the WRONG reason — expected the same duplicate-deposit guard (\"{DUPLICATE_REFUSAL}\"); any other error means the force flag reached some other lane. Got: {error_msg}"
    );

    // Nothing reached wallet2, and nothing in wallet1 was INVALIDATED: the sibling is still a
    // withdrawable DUPLICATED row and the coin is still CONFIRMED.
    let transfer_receive_result =
        mercuryrustlib::transfer_receiver::execute(client_config, &wallet2.name).await?;
    assert!(
        transfer_receive_result.received_statechain_ids.is_empty(),
        "TA02 - transfer_flow: the refused transfer must convey NOTHING — wallet2 received {:?}",
        transfer_receive_result.received_statechain_ids
    );

    mercuryrustlib::coin_status::update_coins(client_config, &wallet1.name).await?;
    let wallet1_rec: mercuryrustlib::Wallet =
        mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let rows: Vec<(u32, CoinStatus)> = wallet1_rec
        .coins
        .iter()
        .filter(|c| c.aggregated_address == Some(deposit_address.clone()))
        .map(|c| (c.duplicate_index, c.status.clone()))
        .collect();
    assert!(
        rows.contains(&(0, CoinStatus::CONFIRMED)) && rows.contains(&(1, CoinStatus::DUPLICATED)),
        "TA02 - transfer_flow: after the refusal wallet1 must still hold the CONFIRMED index-0 coin and the DUPLICATED index-1 sibling untouched (nothing INVALIDATED); rows: {rows:?}"
    );
    assert!(
        !rows.iter().any(|(_, s)| *s == CoinStatus::INVALIDATED || *s == CoinStatus::TRANSFERRED),
        "TA02 - transfer_flow: a refused transfer must leave no INVALIDATED or TRANSFERRED row; rows: {rows:?}"
    );

    let fee_rate = None;

    // Recovery: withdraw the duplicate cooperatively — the only thing a duplicate can do.
    let result = mercuryrustlib::withdraw::execute(
        client_config,
        &wallet1.name,
        statechain_id,
        &core_wallet_address,
        fee_rate,
        Some(1),
    )
    .await;
    assert!(
        result.is_ok(),
        "TA02 - transfer_flow: withdrawing the DUPLICATED coin (statechain_id {statechain_id}, duplicate_index=1) must succeed after the refused force-send. Failed with: {:?}",
        result.as_ref().err()
    );

    // The duplicate-index bookkeeping: a second withdrawal of index 1 finds no DUPLICATED row.
    let result = mercuryrustlib::withdraw::execute(
        client_config,
        &wallet1.name,
        statechain_id,
        &core_wallet_address,
        fee_rate,
        Some(1),
    )
    .await;

    assert!(
        result.is_err(),
        "TA02 - transfer_flow: withdrawing 'duplicate_index=1' for statechain_id {statechain_id} a SECOND time must be REFUSED — the sibling is already WITHDRAWING, so there is no longer a DUPLICATED row at index 1 and accepting this would mean the duplicate-index bookkeeping is broken. It was accepted instead."
    );

    let error_msg = result.err().unwrap().to_string();

    assert!(
        error_msg == "No duplicated coins associated with this statechain ID and index 1 were found",
        "TA02 - transfer_flow: the second withdraw of statechain_id {statechain_id} duplicate_index=1 was refused, but for the WRONG reason — expected the duplicated-index-not-found guard in withdraw::execute to fire. Got: {error_msg}"
    );

    // And the index-0 coin is still the owner's to withdraw.
    let result = mercuryrustlib::withdraw::execute(
        client_config,
        &wallet1.name,
        statechain_id,
        &core_wallet_address,
        fee_rate,
        None,
    )
    .await;
    assert!(
        result.is_ok(),
        "TA02 - transfer_flow: withdrawing the index-0 coin for statechain_id {statechain_id} must succeed. Failed with: {:?}",
        result.as_ref().err()
    );

    println!("TA02 - transfer_flow: force_send is inert — refused by name, nothing conveyed, nothing invalidated; both rows withdrawn cooperatively");

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
