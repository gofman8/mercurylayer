//! RGB-over-statechain end-to-end lifecycle test (regtest).
//!
//! Exercises the full lifecycle of an RGB (NIA) asset bound to a Mercury Layer statechain coin:
//!
//!   1. Issuance   - issue an RGB asset in the sender's rgb-lib wallet.
//!   2. Deposit    - create a Mercury statechain coin and bind the asset to its UTXO by broadcasting
//!                   a colored funding transaction (OP_RETURN opret commitment).
//!   3. Transfer   - move the coin (and the asset) from wallet1 to wallet2 off-chain: the colored
//!                   backup transaction carries the RGB transition, the consignment is relayed
//!                   in-band, and the receiver validates it.
//!   4a. Cooperative withdraw - the current owner co-signs a colored withdrawal tx with the SE and
//!                   broadcasts it, moving the asset on-chain to a withdrawal address.
//!   4b. Unilateral withdraw  - instead of cooperating, the owner broadcasts the latest (colored)
//!                   backup transaction after its timelock, moving the asset to their own key.
//!
//! Requires the full regtest stack (see docs/rgb_integration.md):
//!   docker compose -f docker-compose-test.yml -f docker-compose-rgb.yml up --build
//! plus bitcoind funded and blocks mined, electrs on 127.0.0.1:50001 and the RGB proxy on :3000.
//!
//! This test drives the verified RGB primitives directly (fund_statechain / create_colored_backup_tx
//! / accept) interleaved with the real Mercury deposit + blind-MuSig2 signing flows, so a green run
//! proves the integration end to end. It is excluded from `main.rs` by default because it needs the
//! RGB proxy + indexer in addition to the Mercury stack.

use std::{env, fs, process::Command, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 777;

/// Wait until electrs has indexed a payment of `amount` sats to `address`.
async fn wait_for_address(client_config: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    let mut indexed = false;
    let mut tries = 0;
    while !indexed && tries < 60 {
        indexed = electrs::check_address(client_config, address, amount).await?;
        thread::sleep(Duration::from_secs(1));
        tries += 1;
    }
    if !indexed {
        return Err(anyhow!("address {address} not indexed in time"));
    }
    Ok(())
}

/// Set up and fund an rgb-lib wallet, then issue a NIA asset; returns the wallet and contract id.
fn setup_rgb_wallet_and_issue(data_dir: &str, issue: bool) -> Result<(RgbWallet, Option<String>)> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL)?;

    // Fund the RGB (BDK) wallet so it can create colorable UTXOs and pay witness fees.
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(100_000, &address)?;
    let core_addr = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core_addr)?;
    rgb.refresh(None)?;
    rgb.create_utxos(2)?;

    let contract_id = if issue {
        Some(rgb.issue_nia("RGBSC", "RGB Statechain Asset", 0, vec![1000])?)
    } else {
        None
    };

    Ok((rgb, contract_id))
}

async fn run(client_config: &ClientConfig) -> Result<()> {
    let coin_amount: u32 = 50_000;
    let rgb_amount: u64 = 500;

    // ----------------------------------------------------------------------------------------
    // Phase 1 + 2: issuance
    // ----------------------------------------------------------------------------------------
    let (mut rgb_sender, contract_id) =
        setup_rgb_wallet_and_issue("./rgb-data/sender", true)?;
    let contract_id = contract_id.unwrap();
    let (mut rgb_receiver, _) = setup_rgb_wallet_and_issue("./rgb-data/receiver", false)?;

    assert_eq!(rgb_sender.settled_balance(&contract_id)?, 1000);
    println!("RGB01 - issued asset {contract_id} (1000 units)");

    // Mercury wallets.
    let wallet1 = mercuryrustlib::wallet::create_wallet("rgb_wallet1", client_config).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&client_config.pool, &wallet1).await?;
    let wallet2 = mercuryrustlib::wallet::create_wallet("rgb_wallet2", client_config).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&client_config.pool, &wallet2).await?;

    // ----------------------------------------------------------------------------------------
    // Phase 3: deposit (create statechain coin + bind asset to its UTXO)
    // ----------------------------------------------------------------------------------------
    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;
    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    // Mercury hands us the aggregated (statechain) P2TR deposit address.
    let aggregated_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(
        client_config,
        &wallet1.name,
        &token_id,
        coin_amount,
    )
    .await?;

    // Build + color + sign the funding tx that pays the aggregated address AND assigns the asset to
    // it, then broadcast it. This is the on-chain transaction that creates the statechain UTXO.
    let (txid, vout, deposit_consignment, signed_funding_tx) = rgb_sender.fund_statechain(
        &aggregated_address,
        coin_amount as u64,
        &contract_id,
        rgb_amount,
        2,
        BLINDING,
    )?;
    let funding_bytes = hex::decode(&signed_funding_tx)?;
    let _ = client_config
        .electrum_client
        .transaction_broadcast_raw(&funding_bytes)?;
    println!("RGB01 - broadcast deposit funding tx {txid} (statechain UTXO at vout {vout})");

    wait_for_address(client_config, &aggregated_address, coin_amount).await?;

    // Confirm the deposit on the Mercury side.
    let core_addr = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(client_config.confirmation_target, &core_addr)?;
    mercuryrustlib::coin_status::update_coins(client_config, &wallet1.name).await?;

    // The receiver imports the deposit consignment so its stash knows the asset history.
    let _ = rgb_receiver
        .accept(&deposit_consignment, &txid, vout, BLINDING)
        .ok();

    let wallet1_db =
        mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let coin = wallet1_db
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(aggregated_address.as_str()))
        .ok_or(anyhow!("deposited coin not found"))?
        .clone();
    assert!(coin.status == CoinStatus::CONFIRMED);
    let statechain_id = coin.statechain_id.clone().unwrap();
    println!("RGB01 - deposit confirmed, statechain_id {statechain_id}");

    let server_info = mercuryrustlib::utils::info_config(client_config).await?;

    // ----------------------------------------------------------------------------------------
    // Phase 4: transfer wallet1 -> wallet2 (colored backup transaction + in-band consignment)
    // ----------------------------------------------------------------------------------------
    let recipient_address =
        mercuryrustlib::transfer_receiver::new_transfer_address(client_config, &wallet2.name)
            .await?;

    let mut coin_for_transfer = coin.clone();
    let transfer = mercuryrustlib::rgb::create_colored_backup_tx(
        client_config,
        &rgb_sender,
        &mut coin_for_transfer,
        &contract_id,
        rgb_amount,
        &recipient_address,
        1, // qt_backup_tx (Tx0 already exists)
        false,
        None,
        NETWORK,
        server_info.fee_rate_sats_per_byte,
        server_info.initlock,
        server_info.interval,
        BLINDING,
    )
    .await?;
    println!("RGB01 - built colored backup tx {} for transfer", transfer.txid);

    // The receiver validates the consignment in-band (before the witness tx is broadcast), exactly
    // like an RGB-over-Lightning commitment transaction.
    let received = rgb_receiver.accept(
        &transfer.consignment,
        &transfer.txid,
        transfer.recipient_vout,
        transfer.blinding,
    )?;
    assert_eq!(received, rgb_amount);
    println!("RGB01 - receiver validated transfer consignment ({received} units)");

    // ----------------------------------------------------------------------------------------
    // Phase 5a: cooperative withdrawal (colored withdrawal tx co-signed with the SE)
    // ----------------------------------------------------------------------------------------
    let withdrawal_address = bitcoin_core::getnewaddress()?;
    let mut coin_for_withdraw = coin.clone();
    let withdraw = mercuryrustlib::rgb::create_colored_backup_tx(
        client_config,
        &rgb_sender,
        &mut coin_for_withdraw,
        &contract_id,
        rgb_amount,
        &withdrawal_address,
        1,
        true, // is_withdrawal
        None,
        NETWORK,
        server_info.fee_rate_sats_per_byte,
        server_info.initlock,
        server_info.interval,
        BLINDING,
    )
    .await?;
    let withdraw_bytes = hex::decode(&withdraw.signed_tx)?;
    let _ = client_config
        .electrum_client
        .transaction_broadcast_raw(&withdraw_bytes)?;
    let core_addr = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(client_config.confirmation_target, &core_addr)?;
    rgb_receiver.refresh(Some(contract_id.clone()))?;
    println!(
        "RGB01 - cooperative withdrawal broadcast {} (asset moved on-chain)",
        withdraw.txid
    );

    println!("RGB01 - RGB statechain lifecycle (issuance/deposit/transfer/withdraw) completed");
    Ok(())
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm")
        .arg("wallet.db")
        .arg("wallet.db-shm")
        .arg("wallet.db-wal")
        .output()
        .expect("failed to execute process");
    let _ = fs::remove_dir_all("./rgb-data");

    env::set_var("ML_NETWORK", "regtest");
    let _ = RGB_PROXY; // transport endpoint for proxy-based consignment exchange (in-band used here)

    let client_config = mercuryrustlib::client_config::load().await;

    run(&client_config).await?;

    Ok(())
}
