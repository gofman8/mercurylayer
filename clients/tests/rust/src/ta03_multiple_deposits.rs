//! TA03 — multiple deposits to the SAME address, on the ONE coin shape.
//!
//! This file used to force-send a coin together with its duplicate deposits (`--force`), validate
//! the conveyed FLAT backup chain (INV-5: each backup's absolute locktime decrementing by
//! `interval`, `tx_n` increasing, each spending the previous), and have the receiver withdraw the
//! duplicates. None of that exists any more: a coin's only exit material is its TES-R ladder,
//! rooted at exactly ONE funding outpoint, and a duplicate deposit — a second UTXO paid to the
//! same aggregate address — has no exit material of its own and no slot in the receiver's census.
//! `transfer_sender::execute` therefore refuses a coin with duplicates BY NAME, whatever
//! `force_send` says and whether or not `duplicated_indexes` are named. Duplicates are recovered
//! the one way that is left: a cooperative withdrawal.
//!
//! What is measured, on the live stack:
//!   * four deposits to one address book ONE laddered coin (index 0: `tesr-` row, exactly 3
//!     co-signs, no flat row, `locktime == None`) and three `DUPLICATED` coins, none with a
//!     calendar;
//!   * EVERY conveyance of that coin is refused by name — with named indexes and without, with
//!     `force_send` and without, to another wallet and to itself — and the refusal is decided
//!     BEFORE anything irreversible: the ladder is byte-identical, the enclave count is still 3,
//!     the coin is still `CONFIRMED`, and the would-be receiver's mailbox is empty;
//!   * each duplicate is withdrawn cooperatively from the depositor, and each withdrawal is one more
//!     enclave co-sign the ladder's census cannot account for — which is exactly why, afterwards,
//!     the index-0 coin can still be WITHDRAWN but no longer CONVEYED (refused by name: "there have
//!     been withdrawals of other coins with this same statechain_id");
//!   * an UNCONFIRMED duplicate is refused the same way, and confirming it does not make the coin
//!     conveyable (the old "confirm, then force-send" recovery is gone with the flat chain).
//!
//! Run: the legacy sequence (tb01..tv01) on the regtest stack.

use std::{env, process::Command, thread, time::Duration};

use anyhow::{anyhow, Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};
use crate::sdk40_tesr_consensus::se_num_sigs;

/// The exact refusal `transfer_sender::execute` gives a coin with duplicate deposits.
const DUPLICATE_REFUSAL: &str = "has duplicate deposits";
/// The exact refusal it gives a coin whose duplicates have been WITHDRAWN.
const WITHDRAWN_DUPLICATE_REFUSAL: &str = "There have been withdrawals of other coins with this same statechain_id";

async fn deposit(amount_in_sats: u32, client_config: &ClientConfig, deposit_address: &str) -> Result<()> {

    let _ = bitcoin_core::sendtoaddress(amount_in_sats, &deposit_address)?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, amount_in_sats).await?;
        thread::sleep(Duration::from_secs(1));
    }

    Ok(())
}

/// The FLAT backup rows under the bare statechain id. `None` (no row) and `Some(empty)` are both
/// zero; a failed READ is an error, never zero.
async fn flat_backup_rows(client_config: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&client_config.pool, wallet_name, statechain_id)
        .await?
        .map(|rows| rows.len())
        .unwrap_or(0))
}

/// One conveyance attempt that must be refused by the duplicate-deposit rule, before any SE call.
async fn assert_convey_refused(
    client_config: &ClientConfig,
    to_address: &str,
    wallet_name: &str,
    statechain_id: &str,
    duplicated_indexes: Option<Vec<u32>>,
    force_send: bool,
    expected_fragment: &str,
    what: &str,
) -> Result<()> {
    let result = mercuryrustlib::transfer_sender::execute(client_config, to_address, wallet_name, statechain_id, duplicated_indexes.clone(), force_send, None).await;
    assert!(
        result.is_err(),
        "TA03 - {what}: conveying SC={statechain_id} (indexes {duplicated_indexes:?}, force_send {force_send}) must be \
         REFUSED — a duplicate carries no exit material and cannot ride a ladder conveyance. It was accepted."
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains(expected_fragment),
        "TA03 - {what}: refused, but for the WRONG reason. Expected a refusal naming \"{expected_fragment}\"; any other \
         error (unreachable server, timeout, locked db) would satisfy is_err() too and prove nothing. Got: {msg}"
    );
    println!("TA03 - {what}: REFUSED by name (indexes {duplicated_indexes:?}, force_send {force_send}): {msg}");
    Ok(())
}

/// The index-0 coin's ladder, byte-identical to `expected_trigger`, with the enclave count still
/// `expected_num_sigs` and no flat row: proof that a refused conveyance touched nothing.
async fn assert_ladder_untouched(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
    expected_trigger: &str,
    expected_num_sigs: u32,
    step: &str,
) -> Result<()> {
    let ladder = mercuryrustlib::tesr::load(client_config, wallet_name, statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TA03 - {step}: SC={statechain_id} lost its `tesr-` ladder row"))?;
    assert_eq!(ladder.trigger.txid, expected_trigger, "TA03 - {step}: the ladder must not have been re-established");
    assert!(ladder.conveyed_states.is_empty(), "TA03 - {step}: a refused conveyance must record NO outstanding conveyed state");
    let n = se_num_sigs(client_config, statechain_id).await?;
    assert_eq!(n, expected_num_sigs, "TA03 - {step}: the enclave count must be exactly {expected_num_sigs} — a refusal must happen BEFORE any co-sign");
    assert_eq!(flat_backup_rows(client_config, wallet_name, statechain_id).await?, 0, "TA03 - {step}: no flat backup row, ever");
    Ok(())
}

/// Four deposits to one address: ONE laddered coin plus three duplicates. Returns the deposit
/// address and the index-0 coin's statechain id.
async fn four_deposits(client_config: &ClientConfig, wallet1: &Wallet, amounts: [u32; 4]) -> Result<(String, String)> {

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let deposit_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amounts[0]).await?;

    for amount in amounts {
        deposit(amount, &client_config, &deposit_address).await?;
    }

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.duplicate_index == 0 && coin.status == CoinStatus::CONFIRMED);
    let duplicated_coin_1 = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.duplicate_index == 1 && coin.status == CoinStatus::DUPLICATED);
    let duplicated_coin_2 = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.duplicate_index == 2 && coin.status == CoinStatus::DUPLICATED);
    let duplicated_coin_3 = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.duplicate_index == 3 && coin.status == CoinStatus::DUPLICATED);

    assert!(new_coin.is_some());
    assert!(duplicated_coin_1.is_some());
    assert!(duplicated_coin_2.is_some());
    assert!(duplicated_coin_3.is_some());

    let new_coin = new_coin.unwrap();
    let statechain_id = new_coin.statechain_id.clone().unwrap();

    // ONE ladder, over the index-0 funding outpoint only; three co-signs; no flat row; no calendar
    // on the coin OR on any of its duplicates.
    let ladder = mercuryrustlib::tesr::load(client_config, &wallet1.name, &statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TA03 - the index-0 coin SC={statechain_id} was booked CONFIRMED with NO `tesr-` ladder row"))?;
    assert_eq!(ladder.f_txid, new_coin.utxo_txid.clone().unwrap_or_default(), "TA03 - the ladder is rooted at the INDEX-0 funding outpoint");
    assert_eq!(ladder.f_vout, new_coin.utxo_vout.unwrap_or(u32::MAX), "TA03 - the ladder is rooted at the INDEX-0 funding outpoint");
    assert_eq!(se_num_sigs(client_config, &statechain_id).await?, 3, "TA03 - four deposits to one address still co-sign exactly ONE ladder: T + X_0 + S_0");
    assert_eq!(flat_backup_rows(client_config, &wallet1.name, &statechain_id).await?, 0, "TA03 - no flat backup row for the coin or its duplicates");
    for coin in [new_coin, duplicated_coin_1.unwrap(), duplicated_coin_2.unwrap(), duplicated_coin_3.unwrap()] {
        assert!(coin.locktime.is_none(), "TA03 - duplicate index {} carries locktime {:?}; nothing on this coin has an absolute calendar", coin.duplicate_index, coin.locktime);
        assert!(coin.duplicate_index == 0 || coin.utxo_txid != new_coin.utxo_txid || coin.utxo_vout != new_coin.utxo_vout, "TA03 - a duplicate rests on its OWN outpoint");
    }

    println!("TA03 - four deposits {:?} to {} booked ONE laddered coin SC={} (T={}, num_sigs 3, no flat row) + 3 DUPLICATED coins, no calendars",
        amounts, &deposit_address[..12.min(deposit_address.len())], &statechain_id[..8], &ladder.trigger.txid[..8]);

    Ok((deposit_address, statechain_id))
}

async fn duplicates_cannot_be_conveyed_workflow(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet)  -> Result<()> {

    let (deposit_address, statechain_id) = four_deposits(client_config, wallet1, [1000, 2000, 2000, 1000]).await?;
    let trigger = mercuryrustlib::tesr::load(client_config, &wallet1.name, &statechain_id).await?.unwrap().trigger.txid;

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;
    let wallet1_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet1.name).await?;

    // EVERY conveyance shape the flat lane used to license is refused by name — before any co-sign.
    assert_convey_refused(client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, Some(vec![1, 3]), true, DUPLICATE_REFUSAL, "[1] force-send with indexes [1, 3] to wallet2").await?;
    assert_convey_refused(client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, Some(vec![1, 2, 3]), true, DUPLICATE_REFUSAL, "[2] force-send with every index to wallet2").await?;
    assert_convey_refused(client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, true, DUPLICATE_REFUSAL, "[3] force-send of the index-0 coin alone").await?;
    assert_convey_refused(client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, false, DUPLICATE_REFUSAL, "[4] plain send of the index-0 coin alone").await?;
    assert_convey_refused(client_config, &wallet1_transfer_adress, &wallet1.name, &statechain_id, Some(vec![1, 3]), true, DUPLICATE_REFUSAL, "[5] force-send to ITSELF with indexes [1, 3]").await?;
    assert_ladder_untouched(client_config, &wallet1.name, &statechain_id, &trigger, 3, "[6] after five refused conveyances").await?;

    // Nothing moved: every coin is exactly as booked, and wallet2's mailbox is empty.
    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1_rec = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let still_confirmed = wallet1_rec.coins.iter().filter(|c| c.aggregated_address == Some(deposit_address.clone()) && c.duplicate_index == 0 && c.status == CoinStatus::CONFIRMED).count();
    let still_duplicated = wallet1_rec.coins.iter().filter(|c| c.aggregated_address == Some(deposit_address.clone()) && c.status == CoinStatus::DUPLICATED).count();
    assert_eq!(still_confirmed, 1, "TA03 - [6] the index-0 coin must still be CONFIRMED (not IN_TRANSFER) after refused conveyances");
    assert_eq!(still_duplicated, 3, "TA03 - [6] all three duplicates must still be DUPLICATED");
    let nothing_received = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet2.name).await?;
    assert!(
        nothing_received.received_statechain_ids.is_empty(),
        "TA03 - [6] wallet2 must receive NOTHING: a refused conveyance posts no mailbox message. Got {:?}",
        nothing_received.received_statechain_ids
    );

    println!("TA03 - [6] five refused conveyances touched nothing: ladder T={} intact, num_sigs 3, index-0 CONFIRMED, 3 DUPLICATED, wallet2's mailbox empty", &trigger[..8]);

    // RECOVERY: each duplicate is withdrawn cooperatively, from the DEPOSITOR. Each withdrawal is
    // an enclave co-sign over the shared statechain key — one the ladder's census can never account
    // for, which is precisely why the coin becomes un-conveyable afterwards.
    let core_wallet_address = bitcoin_core::getnewaddress()?;

    let fee_rate = None;

    for (k, index) in [1u32, 2, 3].iter().enumerate() {
        let result = mercuryrustlib::withdraw::execute(&client_config, &wallet1.name, &statechain_id, &core_wallet_address, fee_rate, Some(*index)).await;
        assert!(
            result.is_ok(),
            "TA03 - [7] withdrawing DUPLICATE index {index} of SC={statechain_id} from wallet1 must succeed — a cooperative \
             withdrawal is the ONE recovery path a duplicate has, and if it fails the deposit is stranded. Failed with: {:?}",
            result.as_ref().err()
        );
        let n = se_num_sigs(client_config, &statechain_id).await?;
        assert_eq!(
            n, 3 + (k as u32) + 1,
            "TA03 - [7] each duplicate withdrawal is exactly one more enclave co-sign over the shared key (3 tiers + {} withdrawal(s))",
            k + 1
        );
        println!("TA03 - [7] duplicate index {index} withdrawn cooperatively to {}; num_sigs now {n}", &core_wallet_address[..12.min(core_wallet_address.len())]);
    }

    // AFTER the duplicate withdrawals the index-0 coin can no longer be conveyed — refused by name,
    // for the signature-count reason — but it CAN still be withdrawn.
    assert_convey_refused(client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, false, WITHDRAWN_DUPLICATE_REFUSAL, "[8] send of the index-0 coin after its duplicates were withdrawn").await?;
    let ladder = mercuryrustlib::tesr::load(client_config, &wallet1.name, &statechain_id).await?.ok_or_else(|| anyhow!("ladder row vanished"))?;
    assert_eq!(ladder.trigger.txid, trigger, "TA03 - [8] the ladder is still the deposit's");
    assert!(ladder.conveyed_states.is_empty(), "TA03 - [8] the refused conveyance recorded nothing");
    assert!(
        mercuryrustlib::tesr::verify_bundle(&ladder, se_num_sigs(client_config, &statechain_id).await?, 0).is_err(),
        "TA03 - [8] the census must NOT balance any more: three withdrawal co-signs are not ladder tiers — that imbalance is the reason the coin cannot be conveyed"
    );

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet1.name, &statechain_id, &core_wallet_address, fee_rate, None).await;
    assert!(
        result.is_ok(),
        "TA03 - [9] withdrawing the PRIMARY coin (duplicate index 0) of SC={statechain_id} from wallet1 must succeed after its \
         duplicates are gone — the refusal above blocks CONVEYING it, never withdrawing it. Failed with: {:?}",
        result.as_ref().err()
    );

    let _ = bitcoin_core::generatetoaddress(client_config.confirmation_target, &core_wallet_address)?;
    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1_rec = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let withdrawn = wallet1_rec.coins.iter()
        .filter(|c| c.statechain_id.as_deref() == Some(statechain_id.as_str()) && c.status == CoinStatus::WITHDRAWN)
        .count();
    assert_eq!(withdrawn, 4, "TA03 - [9] the coin and its three duplicates must all be WITHDRAWN after the withdrawals confirm");

    println!("TA03 - [9] index-0 coin withdrawn cooperatively; all 4 outpoints WITHDRAWN");

    Ok(())
}

async fn unconfirmed_duplicate_workflow(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let amount = 1000;

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let deposit_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    deposit(amount, &client_config, &deposit_address).await?;

    let amount = 1000;

    deposit(amount, &client_config, &deposit_address).await?;

    let amount = 2000;

    let _ = bitcoin_core::sendtoaddress(amount, &deposit_address)?;

    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.duplicate_index == 0 && coin.status == CoinStatus::CONFIRMED);
    let confirmed_duplicated_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::DUPLICATED && coin.amount == Some(1000));
    let unconfirmed_duplicated_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::DUPLICATED && coin.amount == Some(2000));

    assert!(new_coin.is_some());
    assert!(confirmed_duplicated_coin.is_some());
    assert!(unconfirmed_duplicated_coin.is_some());

    let new_coin = new_coin.unwrap();

    let statechain_id = new_coin.statechain_id.clone().unwrap();
    let trigger = mercuryrustlib::tesr::load(client_config, &wallet1.name, &statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TA03 - [U1] the index-0 coin SC={statechain_id} has no ladder"))?
        .trigger
        .txid;
    assert_eq!(se_num_sigs(client_config, &statechain_id).await?, 3, "TA03 - [U1] one ladder, three co-signs, whatever the duplicates' confirmation state");

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    // With one duplicate still UNCONFIRMED the conveyance is refused — by the same duplicate rule,
    // not by a confirmation check: an unconfirmed duplicate is a duplicate.
    assert_convey_refused(client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, Some(vec![1, 2]), true, DUPLICATE_REFUSAL, "[U2] force-send with an UNCONFIRMED duplicate").await?;
    assert_ladder_untouched(client_config, &wallet1.name, &statechain_id, &trigger, 3, "[U2] after the refused conveyance").await?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    // Confirming the duplicate changes nothing: the old recovery ("confirm, then --force") went with
    // the flat chain. The coin is refused exactly as before, and the ladder is still untouched.
    assert_convey_refused(client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, Some(vec![1, 2]), true, DUPLICATE_REFUSAL, "[U3] force-send after the duplicate CONFIRMED").await?;
    assert_ladder_untouched(client_config, &wallet1.name, &statechain_id, &trigger, 3, "[U3] after the second refused conveyance").await?;

    println!("TA03 - [U3] an unconfirmed duplicate and a confirmed one are refused identically; ladder T={} untouched", &trigger[..8]);

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

    duplicates_cannot_be_conveyed_workflow(&client_config, &wallet1, &wallet2).await?;

    unconfirmed_duplicate_workflow(&client_config, &wallet1, &wallet2).await?;

    println!("TA03 - Test \"Multiple Deposits in the Same Adress\" completed successfully: one ladder per coin, duplicates never conveyed, recovered by cooperative withdrawal");

    Ok(())
}
