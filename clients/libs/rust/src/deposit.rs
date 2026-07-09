use anyhow::{anyhow, Result, Ok};
use mercurylib::{deposit::{create_deposit_msg1_with_options, create_aggregated_address}, wallet::{Wallet, BackupTx, Coin}, transaction:: get_user_backup_address, utils::get_blockheight};

use crate::{client_config::ClientConfig, sqlite_manager::{get_wallet, update_wallet}, transaction::new_transaction, utils::info_config};

pub async fn get_deposit_bitcoin_address(client_config: &ClientConfig, wallet_name: &str, token_id: &str, amount: u32) -> Result<String> {
    get_deposit_bitcoin_address_inner(client_config, wallet_name, token_id, amount, false, None).await
}

/// Open a **single-use** deposit address: the SE refuses any second spend once it co-signs one
/// terminal spend of this coin (the off-chain RGB split/combine double-spend guard).
pub async fn get_deposit_bitcoin_address_single_use(client_config: &ClientConfig, wallet_name: &str, token_id: &str, amount: u32) -> Result<String> {
    get_deposit_bitcoin_address_inner(client_config, wallet_name, token_id, amount, true, None).await
}

/// Open a single-use deposit address with an **epoch deadline** (unix seconds, Stage 4): the SE
/// refuses to co-sign any new spend once its own clock passes `epoch_deadline`, so the owner must
/// transact or exit before then. Unilateral exit needs no SE co-signature.
pub async fn get_deposit_bitcoin_address_single_use_epoch(client_config: &ClientConfig, wallet_name: &str, token_id: &str, amount: u32, epoch_deadline: u64) -> Result<String> {
    get_deposit_bitcoin_address_inner(client_config, wallet_name, token_id, amount, true, Some(epoch_deadline)).await
}

async fn get_deposit_bitcoin_address_inner(client_config: &ClientConfig, wallet_name: &str, token_id: &str, amount: u32, single_use: bool, epoch_deadline: Option<u64>) -> Result<String> {

    let token_id = uuid::Uuid::parse_str(&token_id)?;
    // println!("Deposit: {} {} {}", wallet_name, token_id, amount);
    let wallet = get_wallet(&client_config.pool, &wallet_name).await?;
    let mut wallet = init(&client_config, &wallet, token_id, single_use, epoch_deadline).await?;

    let coin = wallet.coins.last_mut().unwrap();

    let aggregated_public_key = create_aggregated_address(&coin, wallet.network.clone())?;

    coin.amount = Some(amount);
    coin.aggregated_address = Some(aggregated_public_key.aggregate_address.clone());
    coin.aggregated_pubkey = Some(aggregated_public_key.aggregate_pubkey);
    coin.single_use = single_use;
    coin.epoch_deadline = epoch_deadline;

    update_wallet(&client_config.pool, &wallet).await?;

    Ok(aggregated_public_key.aggregate_address)
}

// When sending duplicated coins, the tx_n of the backup_tx must be different
pub async fn create_tx1(client_config: &ClientConfig, coin: &mut Coin, wallet_netwotk: &str, tx_n: u32) -> Result<BackupTx> {

    let to_address = get_user_backup_address(&coin, wallet_netwotk.to_string())?;

    let server_info = info_config(&client_config).await?;

    let fee_rate_sats_per_byte = if server_info.fee_rate_sats_per_byte > client_config.max_fee_rate {
        client_config.max_fee_rate
    } else {
        server_info.fee_rate_sats_per_byte
    };

    let signed_tx = new_transaction(
        &client_config, 
        coin, 
        &to_address, 
        0, 
        false, 
        None, 
        wallet_netwotk, 
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

    let backup_tx = BackupTx {
        tx_n,
        tx: signed_tx,
        client_public_nonce: coin.public_nonce.as_ref().unwrap().to_string(),
        server_public_nonce: coin.server_public_nonce.as_ref().unwrap().to_string(),
        client_public_key: coin.user_pubkey.clone(),
        server_public_key: coin.server_pubkey.as_ref().unwrap().to_string(),
        blinding_factor: coin.blinding_factor.as_ref().unwrap().to_string(),
        rgb_consignment: None,
        rgb_blinding: None,
    };

    let block_height = Some(get_blockheight(&backup_tx)?);
    coin.locktime = block_height;

    Ok(backup_tx)
}

pub async fn init(client_config: &ClientConfig, wallet: &Wallet, token_id: uuid::Uuid, single_use: bool, epoch_deadline: Option<u64>) -> Result<Wallet> {

    let mut wallet = wallet.clone();

    let coin = wallet.get_new_coin()?;

    wallet.coins.push(coin.clone());

    update_wallet(&client_config.pool, &wallet).await?;

    let deposit_msg_1 = create_deposit_msg1_with_options(&coin, &token_id.to_string(), single_use, epoch_deadline)?;

    // println!("deposit_msg_1: {:?}", deposit_msg_1);

    let endpoint = client_config.statechain_entity.clone();
    let path = "deposit/init/pod";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    let response = request.json(&deposit_msg_1).send().await?;

    if response.status() != 200 {
        let response_body = response.text().await?;
        return Err(anyhow!(response_body));
    }

    let value = response.text().await?;

    let deposit_msg_1_response: mercurylib::deposit::DepositMsg1Response = serde_json::from_str(value.as_str())?;

    let deposit_init_result = mercurylib::deposit::handle_deposit_msg_1_response(&coin, &deposit_msg_1_response)?;

    let coin = wallet.coins.last_mut().unwrap();

    coin.statechain_id = Some(deposit_init_result.statechain_id);
    coin.signed_statechain_id = Some(deposit_init_result.signed_statechain_id);
    coin.server_pubkey = Some(deposit_init_result.server_pubkey);

    update_wallet(&client_config.pool, &wallet).await?;

    Ok(wallet)
}

/// Mint `count` FREE **derived-slot** deposit tokens vouched by an existing statechain this wallet
/// currently owns (`parent_coin`, id `parent_statechain_id`) — for slots created by SE-co-signed
/// flows over it: off-chain split pieces/change, combine outputs, a refresh re-anchor. Owner-auth
/// is the audit-[15] single-use challenge signed with the parent coin's auth key, so only the
/// current owner can draw on the parent's allowance. Errors when the SE predates the endpoint,
/// has derived issuance disabled, or the parent's lifetime allowance is exhausted — callers fall
/// back to normal (possibly paid) tokens.
pub async fn get_derived_tokens(
    client_config: &ClientConfig,
    parent_coin: &Coin,
    parent_statechain_id: &str,
    count: u32,
) -> Result<Vec<String>> {
    let auth_sig = crate::utils::fresh_auth(
        client_config,
        parent_statechain_id,
        parent_coin,
        "deposit/get_derived_token",
    )
    .await?;

    let payload = mercurylib::deposit::DerivedTokenRequest {
        statechain_id: parent_statechain_id.to_string(),
        auth_sig,
        count,
    };

    let client = client_config.get_reqwest_client()?;
    let response = client
        .post(&format!("{}/deposit/get_derived_token", client_config.statechain_entity))
        .json(&payload)
        .send()
        .await?;

    if response.status() != 200 {
        let response_body = response.text().await?;
        return Err(anyhow!(response_body));
    }

    let value = response.text().await?;
    let resp: mercurylib::deposit::DerivedTokenResponse = serde_json::from_str(value.as_str())?;
    if resp.token_ids.len() != count as usize {
        return Err(anyhow!(
            "SE returned {} derived tokens, expected {count}",
            resp.token_ids.len()
        ));
    }
    Ok(resp.token_ids)
}

pub async fn get_token(client_config: &ClientConfig) -> Result<mercurylib::deposit::TokenResponse> {

    let endpoint = client_config.statechain_entity.clone();
    let path = "deposit/get_token";

    let client = client_config.get_reqwest_client()?;
    let request = client.get(&format!("{}/{}", endpoint, path));

    let response = request.send().await?;

    if response.status() != 200 {
        let response_body = response.text().await?;
        return Err(anyhow!(response_body));
    }

    let value = response.text().await?;

    let token: mercurylib::deposit::TokenResponse = serde_json::from_str(value.as_str())?;

    return Ok(token);
}
