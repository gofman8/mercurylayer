
use crate::utils::create_activity;
use std::str::FromStr;

use bitcoin::Address;
use electrum_client::{ElectrumApi, ListUnspentRes};
use mercurylib::{utils::is_enclave_pubkey_part_of_coin, wallet::{Activity, BackupTx, Coin, CoinStatus}};
use anyhow::{anyhow, Result, Ok};

use crate::{client_config::ClientConfig, sqlite_manager::{get_wallet, update_wallet, insert_backup_txs}, deposit::create_tx1};

struct DepositResult {
    activity: Activity,
    /// None for single-use coins (off-chain RGB tree nodes): they skip the unilateral-exit backup tx,
    /// so the SE produces exactly one signature (the terminal spend) and single-use is `>=1`.
    backup_tx: Option<BackupTx>,
}

async fn check_deposit(client_config: &ClientConfig, coin: &mut Coin, wallet_netwotk: &str) -> Result<Option<DepositResult>> {

    if coin.statechain_id.is_none() && coin.utxo_txid.is_none() && coin.utxo_vout.is_none() {
        if coin.status != CoinStatus::INITIALISED {
            return Err(anyhow!("Coin does not have a statechain ID, a UTXO and the status is not INITIALISED"));
        } else {
            return Ok(None);
        }
    }

    let mut utxo: Option<ListUnspentRes> = None;

    let address = Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(client_config.network)?;

    let utxo_list =  client_config.electrum_client.script_list_unspent(&address.script_pubkey())?;

    for unspent in utxo_list {
        if unspent.value == coin.amount.unwrap() as u64 {
            utxo = Some(unspent);
            break;
        }
    }

    // No deposit found. No change in the coin status
    if utxo.is_none() {
        return Ok(None);
        // return Err(anyhow!("There is no UTXO with the address {} and the amount {}", coin.aggregated_address.as_ref().unwrap(), coin.amount.unwrap()));
    }

    let utxo = utxo.unwrap();

    // Coin amounts are stored as u32 sats (SPEC §14 "amount width"). Refuse to book a deposit whose
    // value would silently truncate on the `as u32` cast below, rather than mis-accounting it.
    if utxo.value > u32::MAX as u64 {
        return Err(anyhow!(
            "deposit UTXO value {} sats exceeds the u32 coin-amount ceiling (~42.9 BTC); refusing to book a truncated amount",
            utxo.value
        ));
    }

    // IN_MEMPOOL. there is nothing to do
    if utxo.height == 0 && coin.status == CoinStatus::IN_MEMPOOL {
        return Ok(None);
    }

    let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
    let blockheight = block_header.height;

    let mut deposit_result: Option<DepositResult> = None;

    if coin.status == CoinStatus::INITIALISED {
        let utxo_txid = utxo.tx_hash.to_string();
        let utxo_vout = utxo.tx_pos as u32;

        if coin.status != CoinStatus::INITIALISED {
            return Err(anyhow!("The coin with the public key {} is not in the INITIALISED state", coin.user_pubkey.to_string()));
        }
    
        coin.utxo_txid = Some(utxo_txid.to_string());
        coin.utxo_vout = Some(utxo_vout);
    
        coin.status = CoinStatus::IN_MEMPOOL;

        // Single-use coins skip the unilateral-exit backup tx (the tree branch is the exit), so the SE
        // co-signs only the one terminal spend and single-use enforcement is a clean `>= 1`.
        let backup_tx = if coin.single_use {
            None
        } else {
            Some(create_tx1(client_config, coin, wallet_netwotk, 1u32).await?)
        };

        let activity_utxo = format!("{}:{}", utxo.tx_hash.to_string(), utxo.tx_pos);

        let activity = Some(create_activity(&activity_utxo, utxo.value as u32, "deposit"));

        deposit_result = Some(DepositResult {
            activity: activity.unwrap(),
            backup_tx
        });
    }

    if utxo.height > 0 {

        let confirmations = blockheight - utxo.height + 1;

        coin.status = CoinStatus::UNCONFIRMED;

        if confirmations as u32 >= client_config.confirmation_target {
            coin.status = CoinStatus::CONFIRMED;
        }
    }

    Ok(deposit_result)
}

async fn check_transfer(client_config: &ClientConfig, coin: &Coin) -> Result<bool> {

    if coin.statechain_id.is_none() {
        return Err(anyhow!("Coin does not have a statechain ID"));
    }

    let statechain_id = coin.statechain_id.as_ref().unwrap();

    let statechain_info = crate::utils::get_statechain_info(statechain_id, &client_config).await?;

    // if the statechain info is not found, we assume the coin has been transferred
    if statechain_info.is_none() {
        return Ok(true);
    }

    let statechain_info = statechain_info.unwrap();

    let enclave_public_key = statechain_info.enclave_public_key;

    // if the enclave's public key is no longer part of the coin, the coin has been transferred
    let is_transferred = !is_enclave_pubkey_part_of_coin(&coin, &enclave_public_key)?;

    return Ok(is_transferred);
}

async fn check_withdrawal(client_config: &ClientConfig, coin: &mut Coin) -> Result<()> {

    let mut txid: Option<String> = None;

    if coin.tx_withdraw.is_some() {
        txid = Some(coin.tx_withdraw.as_ref().unwrap().to_string());
    }

    if coin.tx_cpfp.is_some() {
        if txid.is_some() {
            return Err(anyhow!("Coin has both tx_withdraw and tx_cpfp"));
        }
        txid = Some(coin.tx_cpfp.as_ref().unwrap().to_string());
    }

    if txid.is_none() {
        // A coin can be WITHDRAWING because of a UNILATERAL exit rather than a cooperative one — an
        // in-ladder split child is routed that way, since its funding `SP.out[j]` is un-broadcast and
        // there is no confirmed outpoint for a cooperative withdraw to spend. Such a coin has neither
        // a withdrawal tx NOR a withdrawal address: its progress is tracked by the pre-signed exit
        // chain, not by watching one txid. Nothing to check here — treating it as an error made every
        // subsequent status poll fail for the life of the coin.
        if coin.withdrawal_address.is_none() {
            return Ok(());
        }
        return Err(anyhow!("Coin does not have tx_withdraw or tx_cpfp"));
    }

    let txid = txid.unwrap();

    if coin.withdrawal_address.is_none() {
        return Err(anyhow!("Coin does not have withdrawal_address"));
    }

    let address = Address::from_str(&coin.withdrawal_address.as_ref().unwrap())?.require_network(client_config.network)?;

    let utxo_list =  client_config.electrum_client.script_list_unspent(&address.script_pubkey())?;

    let mut utxo: Option<ListUnspentRes> = None;

    for unspent in utxo_list {
        if unspent.tx_hash.to_string() == txid {
            utxo = Some(unspent);
            break;
        }
    }

    if utxo.is_none() {
        // sometimes the transaction has not yet been transmitted to the specified Electrum server
        // return Err(anyhow!("There is no UTXO with the address {} and the txid {}", coin.withdrawal_address.as_ref().unwrap(), txid));
        return Ok(());
    }

    let utxo = utxo.unwrap();

    if utxo.height > 0 {

        let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
        let blockheight = block_header.height;

        let confirmations = blockheight - utxo.height + 1;

        if confirmations as u32 >= client_config.confirmation_target {
            coin.status = CoinStatus::WITHDRAWN;
        }
    }


    Ok(())
}

async fn check_for_duplicated(client_config: &ClientConfig, existing_coins: &Vec<Coin>) -> Result<Vec<Coin>>{

    let mut duplicated_coin_list : Vec<Coin> = Vec::new();

    for coin in existing_coins.iter() {

        if coin.status != CoinStatus::IN_MEMPOOL && coin.status != CoinStatus::UNCONFIRMED && coin.status != CoinStatus::CONFIRMED {
            continue;
        }

        let address = Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(client_config.network)?;

        let utxo_list =  client_config.electrum_client.script_list_unspent(&address.script_pubkey())?;

        let mut max_duplicated_index = existing_coins.iter()
            .filter(|c|  c.statechain_id == coin.statechain_id )
            .map(|coin| coin.duplicate_index)
            .max()
            .unwrap();

        for unspent in utxo_list {

            let utxo_exists = existing_coins.iter().any(|coin| {
                coin.utxo_txid == Some(unspent.tx_hash.to_string()) &&
                coin.utxo_vout == Some(unspent.tx_pos as u32)
            });

            if utxo_exists {
                continue;
            }

            // u32 amount ceiling (SPEC §14): skip an oversized duplicate rather than truncate it.
            if unspent.value > u32::MAX as u64 {
                println!(
                    "skipping duplicate UTXO {}:{} — value {} sats exceeds the u32 coin-amount ceiling",
                    unspent.tx_hash, unspent.tx_pos, unspent.value
                );
                continue;
            }

            max_duplicated_index = max_duplicated_index + 1;

            let mut duplicated_coin = coin.clone();
            duplicated_coin.status = CoinStatus::DUPLICATED;
            duplicated_coin.utxo_txid = Some(unspent.tx_hash.to_string());
            duplicated_coin.utxo_vout = Some(unspent.tx_pos as u32);
            duplicated_coin.amount = Some(unspent.value as u32);
            duplicated_coin.duplicate_index = max_duplicated_index;
            duplicated_coin_list.push(duplicated_coin);
        }
    }

    Ok(duplicated_coin_list)

}

pub async fn update_coins(client_config: &ClientConfig, wallet_name: &str) -> Result<()> {
    
    let mut wallet: mercurylib::wallet::Wallet = get_wallet(&client_config.pool, &wallet_name).await?;

    let network = wallet.network.clone();

    for coin in wallet.coins.iter_mut() {

        if coin.status == CoinStatus::INITIALISED || coin.status == CoinStatus::IN_MEMPOOL || coin.status == CoinStatus::UNCONFIRMED {
        
            let deposit_result = check_deposit(client_config, coin, &network).await?;
        
            if deposit_result.is_some() {
                let deposit_result = deposit_result.unwrap();
                let activity = deposit_result.activity;
                wallet.activities.push(activity);
                if let Some(backup_tx) = deposit_result.backup_tx {
                    insert_backup_txs(&client_config.pool, &wallet.name, &coin.statechain_id.as_ref().unwrap(), &[backup_tx].to_vec()).await?;
                }
            }
        } else if coin.status == CoinStatus::IN_TRANSFER {

            let is_transferred = check_transfer(client_config, coin).await?;

            if is_transferred {
                coin.status = CoinStatus::TRANSFERRED;
            }

        } else if coin.status == CoinStatus::WITHDRAWING {
            check_withdrawal(client_config, coin).await?;
        }
    }

    let duplicated_coins = check_for_duplicated(client_config, &wallet.coins).await?;

    wallet.coins.extend(duplicated_coins);

    // invalidate duplicated coins that were not transferred
    for i in 0..wallet.coins.len() {
        if wallet.coins[i].status == CoinStatus::DUPLICATED {
            let is_transferred = (0..wallet.coins.len()).any(|j| 
                i != j && // Skip comparing with self
                wallet.coins[j].statechain_id == wallet.coins[i].statechain_id &&
                wallet.coins[j].locktime == wallet.coins[i].locktime &&
                wallet.coins[j].status == CoinStatus::TRANSFERRED
            );
            if is_transferred {
                wallet.coins[i].status = CoinStatus::INVALIDATED;
            }
        }
    }

    update_wallet(&client_config.pool, &wallet).await?;

    Ok(())
}