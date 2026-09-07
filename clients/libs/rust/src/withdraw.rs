use crate::{client_config::ClientConfig, sqlite_manager::{get_wallet, update_wallet}, transaction::new_transaction, utils::info_config};
use anyhow::{anyhow, Result};
use chrono::Utc;
use electrum_client::ElectrumApi;
use mercurylib::wallet::{Activity, CoinStatus};


pub async fn execute(client_config: &ClientConfig, wallet_name: &str, statechain_id: &str, to_address: &str, fee_rate: Option<f64>, duplicated_index: Option<u32>) -> Result<()>{

    let mut wallet: mercurylib::wallet::Wallet = get_wallet(&client_config.pool, &wallet_name).await?;

    let is_address_valid = mercurylib::validate_address(to_address, &wallet.network)?;

    if !is_address_valid {
        return Err(anyhow!("Invalid address"));
    }

    // A coin has NO flat backup chain: its exit material is its TES-R ladder, established at
    // deposit, and a cooperative withdraw does not read it. The withdrawal transaction's locktime
    // is derived from the current tip alone (`calculate_block_height` with `is_withdrawal`), so the
    // backup-count argument the tx builder still carries is irrelevant here and is passed as 0.
    // (This used to refuse "No backup transaction associated with this statechain ID" when the
    // backup rows were absent — which, since the ladder replaced them, would have been every coin.)
    let qt_backup_tx: u32 = 0;

    // A statechain id can sit on several rows of one wallet: a coin sent to oneself, or one
    // re-received after an earlier hop, keeps its older row beside the live one. The rows used to
    // be told apart by LOCKTIME — each hop's backup was lower, so the newest row was the minimum.
    // A coin carries no absolute locktime any more (its ladder is its only exit material), so that
    // tie-break is gone and the row has to be chosen on what is actually known about it.
    //
    // Two rules, in order, and BOTH matter:
    //   * CONFIRMED before IN_TRANSFER. A row this wallet is in the middle of sending away is not
    //     the row to withdraw, and its auth key is the one being rotated away from.
    //   * within a status, the LAST matching row. `wallet.coins` is append-ordered, so a coin
    //     re-received after being sent keeps its stale outgoing row FIRST and its fresh row last.
    //     Taking the first live row instead hands `sign/first` the old auth key and the coordinator
    //     answers 401 "Signature does not match authentication key" — measured on tb02, whose
    //     step 8 withdraws a coin the wallet sent and then received back without an intervening
    //     status refresh, so the outgoing row is still IN_TRANSFER at that moment.
    let sid_matches =
        |c: &mercurylib::wallet::Coin| c.statechain_id.as_deref() == Some(statechain_id);
    let coin_index: Option<usize> = match duplicated_index {
        Some(index) => wallet.coins.iter().rposition(|c| {
            sid_matches(c) && c.status == CoinStatus::DUPLICATED && c.duplicate_index == index
        }),
        None => wallet
            .coins
            .iter()
            .rposition(|c| sid_matches(c) && c.status == CoinStatus::CONFIRMED)
            .or_else(|| {
                wallet
                    .coins
                    .iter()
                    .rposition(|c| sid_matches(c) && c.status == CoinStatus::IN_TRANSFER)
            })
            .or_else(|| {
                wallet
                    .coins
                    .iter()
                    .rposition(|c| sid_matches(c) && c.status != CoinStatus::DUPLICATED)
            }),
    };
    let coin: Option<&mut mercurylib::wallet::Coin> = coin_index.map(|i| &mut wallet.coins[i]);

    if coin.is_none() {

        match duplicated_index {
            Some(index) => { return Err(anyhow!("No duplicated coins associated with this statechain ID and index {} were found", index)); },
            None => { return Err(anyhow!("No coins associated with this statechain ID were found")) },
        }
    }

    let coin = coin.unwrap();

    if coin.amount.is_none() {
        return Err(anyhow::anyhow!("coin.amount is None"));
    }

    if coin.status != CoinStatus::CONFIRMED && coin.status != CoinStatus::IN_TRANSFER && coin.status != CoinStatus::DUPLICATED {
        return Err(anyhow::anyhow!("Coin status must be CONFIRMED or IN_TRANSFER or DUPLICATED to withdraw it. The current status is {}", coin.status));
    }

    let server_info = info_config(&client_config).await?;

    let fee_rate_sats_per_byte = match fee_rate {
        Some(fee_rate) => fee_rate,
        None => if server_info.fee_rate_sats_per_byte > client_config.max_fee_rate {
            client_config.max_fee_rate
        } else {
            server_info.fee_rate_sats_per_byte
        },
    };

    let signed_tx = new_transaction(
        client_config, 
        coin,
        &to_address,
        qt_backup_tx,
        true,
        None,
        &wallet.network,
        fee_rate_sats_per_byte,
        server_info.initlock,
        server_info.interval
    ).await?;

    if coin.public_nonce.is_none() {
        return Err(anyhow::anyhow!("coin.public_nonce is None"));
    }

    if coin.blinding_factor.is_none() {
        return Err(anyhow::anyhow!("coin.blinding_factor is None"));
    }

    if coin.statechain_id.is_none() {
        return Err(anyhow::anyhow!("coin.statechain_id is None"));
    }

    /*let backup_tx = BackupTx {
        tx_n: new_tx_n,
        tx: signed_tx.clone(),
        client_public_nonce: coin.public_nonce.as_ref().unwrap().to_string(),
        server_public_nonce: coin.server_public_nonce.as_ref().unwrap().to_string(),
        client_public_key: coin.user_pubkey.clone(),
        server_public_key: coin.server_pubkey.as_ref().unwrap().to_string(),
        blinding_factor: coin.blinding_factor.as_ref().unwrap().to_string(),
    };

    backup_txs.push(backup_tx);

    update_backup_txs(&client_config.pool, &coin.statechain_id.as_ref().unwrap(), &backup_txs).await?;*/

    let tx_bytes = hex::decode(&signed_tx)?;
    let txid = client_config.electrum_client.transaction_broadcast_raw(&tx_bytes)?;

    coin.tx_withdraw = Some(txid.to_string());
    coin.withdrawal_address = Some(to_address.to_string());
    coin.status = CoinStatus::WITHDRAWING;

    let date = Utc::now(); // This will get the current date and time in UTC
    let iso_string = date.to_rfc3339(); // Converts the date to an ISO 8601 string

    let activity = Activity {
        utxo: txid.to_string(),
        amount: coin.amount.unwrap(),
        action: "Withdraw".to_string(),
        date: iso_string
    };

    wallet.activities.push(activity);

    // Audit [15]: single-use, endpoint-bound owner auth for the irreversible withdraw/complete.
    let signed_statechain_id =
        crate::utils::fresh_auth(&client_config, statechain_id, coin, "withdraw/complete").await?;

    update_wallet(&client_config.pool, &wallet).await?;

    let is_there_more_duplicated_coins = wallet.coins.iter().any(|coin| {
        (coin.status == CoinStatus::DUPLICATED || coin.status == CoinStatus::CONFIRMED) &&
        duplicated_index.map_or(true, |index| coin.duplicate_index != index)
    });

    if !is_there_more_duplicated_coins {
        crate::utils::complete_withdraw(statechain_id, &signed_statechain_id, &client_config).await?;
    }

    Ok(())

}