//! TB01 — the simple transfer on the ONE coin shape: deposit → transfer → claim → cooperative
//! withdrawal, where a coin's only exit material is its TES-R ladder, established at the FIRST
//! MEMPOOL SIGHTING of the funding transaction.
//!
//! There is no flat absolute-locktime backup any more: `deposit::create_tx1` is gone, and
//! `coin_status::check_deposit` co-signs the ladder (`T`, `X_0`, `S_0`) the moment the deposit is
//! seen in the mempool, before any confirmation. So beyond the status walk this file always had, it
//! now measures the shape of the coin at every step:
//!
//!   * **at IN_MEMPOOL** — no block mined yet — the coin already has a `tesr-` ladder row rooted at
//!     its funding outpoint, the enclave has co-signed EXACTLY 3 times, the census
//!     `verify_bundle(.., 3, 0)` balances with the flat term ZERO, there is NO flat backup row under
//!     the statechain id, and `coin.locktime` is `None` (a laddered coin has no calendar);
//!   * **confirmation adds nothing**: the same trigger and the same 3 co-signs at UNCONFIRMED and at
//!     CONFIRMED — the ladder is established once, at first sight, never again at confirmation;
//!   * an IN_MEMPOOL / UNCONFIRMED coin still cannot be SENT (the coin-status guard, by name);
//!   * the conveyance co-signs NO per-hop backup: the sender still has zero flat rows afterwards;
//!   * **the received coin** carries a ladder that exits to the RECEIVER's own key, whose census
//!     balances with the flat term zero, with no flat backup row and `locktime == None`;
//!   * the current owner can still withdraw cooperatively.

use std::{env, process::Command, thread, time::Duration};

use anyhow::{anyhow, Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};

/// The number of FLAT backup rows under the bare statechain id — where the old `tx1` and every
/// per-hop backup used to live. `try_get_backup_txs` distinguishes "no row" (`None`) from a row
/// holding an empty vector; both are zero flat backups. A failed READ is an error, never zero.
async fn flat_backup_rows(client_config: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&client_config.pool, wallet_name, statechain_id)
        .await?
        .map(|rows| rows.len())
        .unwrap_or(0))
}

/// The enclave's ATTESTED co-signature count for the coin — the right-hand side of the census.
async fn se_num_sigs(client_config: &ClientConfig, statechain_id: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(statechain_id, client_config)
        .await?
        .ok_or_else(|| anyhow!("no /info/statechain record for {statechain_id}"))?
        .num_sigs)
}

/// The shape every laddered coin must have, at any status: a ladder row, no flat row, no calendar.
/// Returns the trigger txid so the caller can pin that the ladder is not re-established later.
async fn assert_laddered_shape(
    client_config: &ClientConfig,
    wallet_name: &str,
    coin: &mercuryrustlib::Coin,
    step: &str,
) -> Result<String> {
    let statechain_id = coin.statechain_id.as_ref().ok_or_else(|| anyhow!("{step}: coin has no statechain id"))?;
    let bundle = mercuryrustlib::tesr::load(client_config, wallet_name, statechain_id)
        .await?
        .ok_or_else(|| anyhow!(
            "TB01 - {step}: coin SC={statechain_id} (status {}) has NO `tesr-` ladder row. A coin's only \
             exit material is its ladder, established at first sight of the deposit; a coin booked \
             without one has no exit at all.",
            coin.status
        ))?;
    assert_eq!(
        bundle.f_txid,
        coin.utxo_txid.clone().unwrap_or_default(),
        "TB01 - {step}: the ladder must be rooted at the coin's own funding txid"
    );
    assert_eq!(
        bundle.f_vout,
        coin.utxo_vout.unwrap_or(u32::MAX),
        "TB01 - {step}: the ladder must be rooted at the coin's own funding vout"
    );
    let flat = flat_backup_rows(client_config, wallet_name, statechain_id).await?;
    assert_eq!(
        flat, 0,
        "TB01 - {step}: coin SC={statechain_id} has {flat} flat backup row(s). There is no flat \
         absolute-locktime backup on any coin: not at deposit and not at any hop."
    );
    assert!(
        coin.locktime.is_none(),
        "TB01 - {step}: coin SC={statechain_id} carries locktime {:?}. A laddered coin has no absolute \
         calendar; `coin.locktime` must be None for life.",
        coin.locktime
    );
    Ok(bundle.trigger.txid.clone())
}

async fn try_to_send_unconfirmed_coin(client_config: &ClientConfig, to_address: &str, wallet: &Wallet, statechain_id: &str) -> Result<()> {

    let batch_id = None;

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, to_address, &wallet.name, &statechain_id, None, force_send, batch_id).await;

    assert!(
        result.is_err(),
        "TB01 - [3/4] sending SC={statechain_id} before it is CONFIRMED must be REFUSED — an \
         unconfirmed deposit is not yet a spendable coin. It was accepted."
    );

    let error = result.err().unwrap();

    let error_message = format!("No coins with status CONFIRMED or IN_TRANSFER associated with this statechain ID were found");

    // The equality is the only thing separating a real refusal from a broken stack: an unreachable
    // server, a locked database or a timeout all satisfy `is_err()` too, and would let this
    // negative test pass without ever exercising the coin-status guard.
    assert!(
        error.to_string() == error_message,
        "TB01 - [3/4] refused, but for the WRONG reason.\n  expected: {error_message}\n  \
         actual:   {error}"
    );

    println!("TB01 - [3/4] send of not-yet-CONFIRMED coin SC={} correctly rejected: {}", &statechain_id[..8.min(statechain_id.len())], error_message);

    Ok(())
}

async fn sucessfully_transfer(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let amount = 10000;

    let address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone()));

    if new_coin.is_none() {
        return Err(anyhow!("Coin not found in wallet"));
    }

    let new_coin = new_coin.unwrap();

    assert!(new_coin.status == CoinStatus::INITIALISED);
    assert!(new_coin.amount == Some(amount));
    assert!(new_coin.statechain_id.is_some());

    let statechain_id = new_coin.statechain_id.clone().unwrap();

    // Nothing is co-signed for a coin that has not been funded: no ladder row, no flat row.
    assert!(
        mercuryrustlib::tesr::load(client_config, &wallet1.name, &statechain_id).await?.is_none(),
        "TB01 - [1] an INITIALISED (unfunded) coin must have no ladder yet — the ladder is signed at \
         first sight of the FUNDING transaction, not at address creation"
    );
    assert_eq!(flat_backup_rows(client_config, &wallet1.name, &statechain_id).await?, 0);

    println!("TB01 - [1] deposit address {} INITIALISED for {} sats in {} (SC={}); no exit material yet",
        &address[..12.min(address.len())],
        amount,
        wallet1.name,
        &statechain_id[..8.min(statechain_id.len())]);

    let _ = bitcoin_core::sendtoaddress(amount, &address)?;

    println!("TB01 - [2] sent {} sats on-chain to {}, waiting for electrs to index", amount, &address[..12.min(address.len())]);

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    println!("TB01 - [2] electrs indexed the deposit tx for {}", &address[..12.min(address.len())]);

    // FIRST SIGHT. This pass books the coin IN_MEMPOOL and, in the same pass, establishes its
    // ladder. An `Err` here is a deposit that was seen but could NOT be laddered — the coin stays
    // INITIALISED and is retried; that is a failure of this test, not a condition to wait out.
    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await.map_err(|e| anyhow!(
        "TB01 - [2] the first-sight pass failed: the deposit was seen but its ladder could not be \
         established, so the coin was NOT booked: {e}"
    ))?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::IN_MEMPOOL);

    // THE RULE, MEASURED AT ITS EARLIEST POINT: no block has been mined, and the coin already has
    // its whole exit — three co-signs, a ladder row, no flat backup, no calendar.
    let trigger_txid = assert_laddered_shape(client_config, &wallet1.name, new_coin, "[2] IN_MEMPOOL").await?;
    let num_sigs = se_num_sigs(client_config, &statechain_id).await?;
    assert_eq!(
        num_sigs, 3,
        "TB01 - [2] IN_MEMPOOL: the enclave must have co-signed EXACTLY 3 times for a fresh deposit \
         (T, X_0, S_0) — there is no `tx1`, so no fourth co-sign exists. Got {num_sigs}."
    );
    let bundle = mercuryrustlib::tesr::load(client_config, &wallet1.name, &statechain_id).await?.unwrap();
    mercuryrustlib::tesr::verify_bundle(&bundle, num_sigs, 0).map_err(|e| anyhow!(
        "TB01 - [2] IN_MEMPOOL: the census `se_num_sigs == tiers + superseded` with the flat term \
         ZERO must balance for a fresh deposit: {e}"
    ))?;

    println!("TB01 - [2] coin SC={} is IN_MEMPOOL and ALREADY LADDERED: trigger {}, num_sigs {}, flat rows 0, locktime None",
        &statechain_id[..8.min(statechain_id.len())], &trigger_txid[..8.min(trigger_txid.len())], num_sigs);

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    println!("TB01 - [3] {} issued transfer address {} for SC={}", wallet2.name, &wallet2_transfer_adress[..12.min(wallet2_transfer_adress.len())], &statechain_id[..8.min(statechain_id.len())]);

    try_to_send_unconfirmed_coin(&client_config, &wallet2_transfer_adress, &wallet1, &statechain_id).await?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(1, &core_wallet_address)?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::UNCONFIRMED);

    // Confirmation changes NOTHING about the exit material: same trigger, same 3 co-signs.
    let trigger_at_unconfirmed = assert_laddered_shape(client_config, &wallet1.name, new_coin, "[4] UNCONFIRMED").await?;
    assert_eq!(trigger_at_unconfirmed, trigger_txid, "TB01 - [4] the ladder must not be re-established at UNCONFIRMED");
    assert_eq!(
        se_num_sigs(client_config, &statechain_id).await?, 3,
        "TB01 - [4] UNCONFIRMED: still exactly 3 co-signs — a second pass must ADOPT the ladder on disk, \
         never spend three more irreversible co-signs on a rival one"
    );

    println!("TB01 - [4] mined 1 block; coin SC={} is UNCONFIRMED (target {} confirmations); ladder unchanged", &statechain_id[..8.min(statechain_id.len())], client_config.confirmation_target);

    try_to_send_unconfirmed_coin(&client_config, &wallet2_transfer_adress, &wallet1, &statechain_id).await?;

    let remaining_blocks = client_config.confirmation_target - 1;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    println!("TB01 - [5] mined {} remaining blocks to reach the confirmation target", remaining_blocks);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    let trigger_at_confirmed = assert_laddered_shape(client_config, &wallet1.name, new_coin, "[5] CONFIRMED").await?;
    assert_eq!(trigger_at_confirmed, trigger_txid, "TB01 - [5] the ladder must not be re-established at CONFIRMED");
    assert_eq!(
        se_num_sigs(client_config, &statechain_id).await?, 3,
        "TB01 - [5] CONFIRMED: still exactly 3 co-signs — confirmation is not an establishment event"
    );

    println!("TB01 - [5] coin SC={} ({} sats) is CONFIRMED and sendable; ladder unchanged since first sight", &statechain_id[..8.min(statechain_id.len())], amount);

    let batch_id = None;

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet.name, &statechain_id, None, force_send, batch_id).await;

    assert!(
        result.is_ok(),
        "TB01 - [6] the happy-path send must succeed: SC={statechain_id} is CONFIRMED and wallet1 \
         holds it. This is the baseline the whole file depends on — if it fails, nothing after it \
         means anything. Failed with: {:?}",
        result.as_ref().err()
    );

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;

    let wallet: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    let new_coin = wallet.coins.iter().find(|&coin| coin.aggregated_address == Some(address.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::IN_TRANSFER);

    // THE CONVEYANCE CO-SIGNED NO PER-HOP BACKUP. The sender still holds zero flat rows and its
    // retained ladder records the receiver-paying state it handed out — nothing else.
    assert_eq!(
        flat_backup_rows(client_config, &wallet1.name, &statechain_id).await?, 0,
        "TB01 - [6] the sender must hold NO flat backup after conveying: a per-hop backup would be a \
         co-sign the receiver's census cannot account for and a matured spend of F left in the \
         sender's hands"
    );
    let retained = mercuryrustlib::tesr::load(client_config, &wallet1.name, &statechain_id).await?
        .ok_or_else(|| anyhow!("TB01 - [6] the sender's retained ladder row vanished on conveyance"))?;
    assert!(
        !retained.conveyed_states.is_empty(),
        "TB01 - [6] the sender's retained ladder must record the receiver-paying state S' it co-signed"
    );
    assert!(new_coin.locktime.is_none(), "TB01 - [6] the sender's coin must still carry no calendar");

    println!("TB01 - [6] {} sent SC={} to {} ({}); sender coin now IN_TRANSFER, no backup co-signed", wallet1.name, &statechain_id[..8.min(statechain_id.len())], wallet2.name, &wallet2_transfer_adress[..12.min(wallet2_transfer_adress.len())]);

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

    // THE RECEIVED COIN: a ladder exiting to the receiver's OWN key, a balanced census with the
    // flat term zero, no flat row, no calendar.
    let received_trigger = assert_laddered_shape(client_config, &wallet2.name, new_coin, "[7] received").await?;
    assert_eq!(
        received_trigger, trigger_txid,
        "TB01 - [7] the received ladder must share the depositor's trigger — one T over F, conveyed, \
         not a second one"
    );
    let received_bundle = mercuryrustlib::tesr::load(client_config, &wallet2.name, &statechain_id).await?.unwrap();
    let receiver_key = mercurylib::transaction::get_user_backup_address(new_coin, "regtest".to_string())
        .map_err(|e| anyhow!("{e:?}"))?;
    assert_eq!(
        received_bundle.owner_exit_address, receiver_key,
        "TB01 - [7] Model A: the received ladder must exit to the RECEIVER's own seed-derived key"
    );
    let num_sigs_after_hop = se_num_sigs(client_config, &statechain_id).await?;
    mercuryrustlib::tesr::verify_bundle(&received_bundle, num_sigs_after_hop, 0).map_err(|e| anyhow!(
        "TB01 - [7] the receiver's census must balance with the flat term ZERO after one hop \
         (num_sigs {num_sigs_after_hop}): {e}"
    ))?;
    assert!(
        !received_bundle.superseded_states.is_empty(),
        "TB01 - [7] the sender's replaced owner state must be DISCLOSED to the receiver as superseded, \
         not hidden — that disclosure is what the census counts"
    );

    println!("TB01 - [7] SC={} is TRANSFERRED for {} and CONFIRMED for {}; received ladder exits to {}'s key, census balances at num_sigs {} with flat term 0", &statechain_id[..8.min(statechain_id.len())], wallet1.name, wallet2.name, wallet2.name, num_sigs_after_hop);

    let fee_rate = None;

    let result = mercuryrustlib::withdraw::execute(&client_config, &wallet2.name, &statechain_id, &core_wallet_address, fee_rate, None).await;

    assert!(
        result.is_ok(),
        "TB01 - [8] cooperative withdrawal must succeed: wallet2 received SC={statechain_id} and is \
         its current owner, so the SE should co-sign the exit. A failure here is the cooperative \
         exit path breaking, which is the path essentially every user takes. A laddered coin has \
         NO flat backup row, so a withdraw that still demands one is exactly this failure. \
         Failed with: {:?}",
        result.as_ref().err()
    );

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

    println!("TB01 - Transfer completed successfully: laddered at first sight, no flat backup at any step, no calendar");

    Ok(())
}
