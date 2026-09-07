//! TA01 — an ABANDONED half-signature (`/sign/first` reached, `/sign/second` never called) must not
//! wedge the coin, on the ONE coin shape.
//!
//! The recovery property is protocol-agnostic and unchanged: a sender that obtained a server nonce
//! for a co-sign and never finalised must still be able to convey the coin afterwards. The SHAPE of
//! the abandoned attempt follows the sender's current order of operations: the receiver-paying
//! state `S'` is co-signed (`/sign/first` + `/sign/second`) BEFORE `transfer/sender` opens the
//! transfer, and the SE refuses every co-sign while a transfer is open (the pending-transfer lock,
//! sdk85 [1]). So "reached sign/first, never called sign/second" is a client that committed nonces,
//! obtained the server nonce, and died before finalising — with NO transfer opened. That is what is
//! staged here, and the recovery is an ordinary send.
//! What this file now ALSO measures is the ladder the recovery runs over:
//!   * the deposit ladders the coin at first sight (3 co-signs, a `tesr-` row, no flat backup row,
//!     `locktime == None`);
//!   * the abandoned `/sign/first` is NOT a co-sign: the enclave's attested count stays at exactly
//!     3, so the census the receiver will run is not charged for a signature that never happened;
//!   * the recovery send co-signs exactly ONE tier (the receiver-paying `S'`) and no per-hop backup
//!     — the sender still has zero flat rows afterwards;
//!   * the received coin carries a ladder exiting to wallet2's own key, whose census balances with
//!     the flat term ZERO (`num_sigs == 4 == 3 tiers + 1 superseded`), with no flat backup row and
//!     `locktime == None`.
//!
//! Run: the legacy sequence (tb01..tv01) on the regtest stack.

use std::{env, process::Command, thread, time::Duration};
use anyhow::{anyhow, Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, create_and_commit_nonces, sqlite_manager::get_wallet, Coin, CoinStatus, SignFirstRequestPayload, SignFirstResponsePayload, Wallet};

use crate::{bitcoin_core, electrs};
use crate::sdk40_tesr_consensus::se_num_sigs;

/// The FLAT backup rows under the bare statechain id. `None` (no row) and `Some(empty)` are both
/// zero; a failed READ is an error, never zero.
async fn flat_backup_rows(client_config: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&client_config.pool, wallet_name, statechain_id)
        .await?
        .map(|rows| rows.len())
        .unwrap_or(0))
}

/// This function gets the server public nonce from the statechain entity.
pub async fn sign_first(client_config: &ClientConfig, sign_first_request_payload: &SignFirstRequestPayload) -> Result<String> {

    let endpoint = client_config.statechain_entity.clone();
    let path = "sign/first";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    let value = request.json(&sign_first_request_payload).send().await?.text().await?;

    let sign_first_response_payload: SignFirstResponsePayload = serde_json::from_str(value.as_str())?;

    let mut server_pubnonce_hex = sign_first_response_payload.server_pubnonce.to_string();

    if server_pubnonce_hex.starts_with("0x") {
        server_pubnonce_hex = server_pubnonce_hex[2..].to_string();
    }

    Ok(server_pubnonce_hex)
}

pub async fn new_transaction_only_sign_first(
    client_config: &ClientConfig,
    coin: &mut Coin) -> Result<()> {

    let coin_nonce = create_and_commit_nonces(&coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce);
    coin.public_nonce = Some(coin_nonce.public_nonce);
    coin.blinding_factor = Some(coin_nonce.blinding_factor);

    let _ = sign_first(&client_config, &coin_nonce.sign_first_request_payload).await?;

    Ok(())
}

/// Stage the abandoned attempt: commit nonces for a co-sign of this coin and obtain the server
/// nonce (`/sign/first`), then stop — no `/sign/second`, and NO `transfer/sender`: the sender's
/// order of operations co-signs `S'` before it opens the transfer, so a client that dies here has
/// opened nothing. (Opening the transfer first would trip the SE's pending-transfer lock on the
/// very `/sign/first` this stages — that is sdk85 [1]'s property, not this file's.)
pub async fn execute_only_sign_first(
    client_config: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str) -> Result<()>
{

    let mut wallet = get_wallet(&client_config.pool, &wallet_name).await?;

    let coin = wallet.coins
        .iter_mut()
        .filter(|tx| tx.statechain_id == Some(statechain_id.to_string())) // Filter coins with the specified statechain_id
        .min_by_key(|tx| tx.locktime.unwrap_or(u32::MAX)); // Find the one with the lowest locktime

    if coin.is_none() {
        return Err(anyhow!("No coins associated with this statechain ID were found"));
    }

    let coin = coin.unwrap();

    let _ = new_transaction_only_sign_first(client_config, coin).await?;

    Ok(())

}

async fn ta01(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let amount = 10000;

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
    let wallet1 = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    let statechain_id = new_coin.statechain_id.as_ref().unwrap();

    // The deposit laddered the coin: three co-signs, a ladder row, no flat row, no calendar.
    let deposit_ladder = mercuryrustlib::tesr::load(client_config, &wallet1.name, statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TA01 - the deposit booked SC={statechain_id} CONFIRMED with NO `tesr-` ladder row"))?;
    assert_eq!(se_num_sigs(client_config, statechain_id).await?, 3, "TA01 - a fresh deposit is exactly T + X_0 + S_0 on the enclave (no tx1)");
    assert_eq!(flat_backup_rows(client_config, &wallet1.name, statechain_id).await?, 0, "TA01 - no flat backup row at deposit");
    assert!(new_coin.locktime.is_none(), "TA01 - a laddered coin has no absolute calendar");

    println!("TA01 - wallet1 deposited SC={} ({} sats): ladder T={} signed at first sight, num_sigs 3", &statechain_id[..8], amount, &deposit_ladder.trigger.txid[..8]);

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    execute_only_sign_first(
        &client_config,
        &wallet1.name,
        &statechain_id).await?;

    // THE ABANDONED HALF-SIGNATURE IS NOT A CO-SIGN. A server nonce was issued and never used; the
    // enclave's attested count — the right-hand side of every receiver census — must not have
    // moved, or the census would be short by one forever for a signature nobody holds.
    assert_eq!(
        se_num_sigs(client_config, statechain_id).await?, 3,
        "TA01 - an abandoned /sign/first must NOT count as a co-sign: num_sigs must still be exactly 3"
    );
    let after_half = mercuryrustlib::tesr::load(client_config, &wallet1.name, statechain_id).await?.ok_or_else(|| anyhow!("ladder row vanished"))?;
    assert_eq!(after_half.trigger.txid, deposit_ladder.trigger.txid, "TA01 - the abandoned attempt touched no ladder material");
    assert!(after_half.conveyed_states.is_empty(), "TA01 - nothing was conveyed, so nothing may be recorded as outstanding");

    println!("TA01 - abandoned /sign/first for SC={}: num_sigs still 3, ladder untouched", &statechain_id[..8]);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1 = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let batch_id = None;

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;

    assert!(
        result.is_ok(),
        "TA01 - the RECOVERY send must succeed: an earlier attempt reached sign/first but never \
         called sign/second, and the coin must not be left wedged by that. If this fails, an \
         abandoned half-signed transfer permanently bricks the coin. Failed with: {:?}",
        result.as_ref().err()
    );

    // The recovery co-signed exactly ONE tier (S') and no per-hop backup.
    assert_eq!(
        se_num_sigs(client_config, statechain_id).await?, 4,
        "TA01 - the recovery send is exactly one more co-sign (S'): 3 + 1, with no backup and no charge for the abandoned attempt"
    );
    assert_eq!(flat_backup_rows(client_config, &wallet1.name, statechain_id).await?, 0, "TA01 - the sender holds NO flat backup after conveying");

    println!("TA01 - RECOVERY send of SC={} succeeded: num_sigs 4 (one S'), no flat backup", &statechain_id[..8]);

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet2.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    assert!(received_statechain_ids.contains(&statechain_id.to_string()));
    assert!(received_statechain_ids.len() == 1);

    let wallet2 = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;

    let new_coin = wallet2.coins.iter().find(|&coin| coin.statechain_id == Some(statechain_id.to_string())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    // THE RECEIVED COIN: a ladder exiting to wallet2's own key, a census that balances with the flat
    // term ZERO, no flat row, no calendar.
    let received = mercuryrustlib::tesr::load(client_config, &wallet2.name, statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TA01 - wallet2 booked SC={statechain_id} with NO `tesr-` ladder row — it has no exit"))?;
    assert_eq!(received.trigger.txid, deposit_ladder.trigger.txid, "TA01 - one T over F, conveyed, not a second one");
    assert_eq!(received.owner_exit_address, new_coin.backup_address, "TA01 - Model A: the received ladder exits to wallet2's OWN key");
    let num_sigs = se_num_sigs(client_config, statechain_id).await?;
    mercuryrustlib::tesr::verify_bundle(&received, num_sigs, 0)
        .map_err(|e| anyhow!("TA01 - the received ladder must pass the census with the flat term ZERO (num_sigs {num_sigs}): {e}"))?;
    assert_eq!(received.superseded_states.len(), 1, "TA01 - the sender's replaced S_0 is disclosed as the one superseded state");
    assert_eq!(flat_backup_rows(client_config, &wallet2.name, statechain_id).await?, 0, "TA01 - the receiver holds no flat backup row");
    assert!(new_coin.locktime.is_none(), "TA01 - the received coin carries no calendar");

    println!("TA01 - wallet2 received SC={}: ladder exits to its key, census balances at num_sigs {num_sigs} with flat term 0", &statechain_id[..8]);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1 = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet1.coins.iter().find(|&coin| coin.statechain_id == Some(statechain_id.to_string())).unwrap();

    assert!(new_coin.status == CoinStatus::TRANSFERRED);

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

    ta01(&client_config, &wallet1, &wallet2).await?;

    println!("TA01 - 'SignSecond not called' tested successfully: the abandoned half-signature is not a co-sign, the recovery conveys the ladder");

    Ok(())
}
