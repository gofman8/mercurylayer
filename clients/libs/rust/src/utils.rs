
use chrono::Utc;
use electrum_client::ElectrumApi;
use mercurylib::{transfer::receiver::StatechainInfoResponsePayload, utils::{InfoConfig, ServerConfig}, wallet::Activity, withdraw::WithdrawCompletePayload};
use anyhow::{anyhow, Result, Ok};
use reqwest::StatusCode;
use crate::client_config::ClientConfig;

pub async fn info_config(client_config: &ClientConfig) -> Result<InfoConfig>{

    let path = "info/config";

    let client = client_config.get_reqwest_client()?;
    let request = client.get(&format!("{}/{}", client_config.statechain_entity, path));

    let value = request.send().await?.text().await?;

    let server_config: ServerConfig = serde_json::from_str(value.as_str())?;

    let initlock = server_config.initlock;
    let interval = server_config.interval;

    let number_blocks = 3;
    let mut fee_rate_btc_per_kb = client_config.electrum_client.estimate_fee(number_blocks)?;

    // Why does it happen?
    if fee_rate_btc_per_kb <= 0.0 {
        fee_rate_btc_per_kb = 0.00001;
    }

    let fee_rate_sats_per_byte = fee_rate_btc_per_kb * 100000.0;

    Ok(InfoConfig {    
        initlock,
        interval,
        fee_rate_sats_per_byte,
    })
}

pub fn create_activity(utxo: &str, amount: u32, action: &str) -> Activity {

    let date = Utc::now(); // This will get the current date and time in UTC
    let iso_string = date.to_rfc3339(); // Converts the date to an ISO 8601 string

    let activity = Activity {
        utxo: utxo.to_string(),
        amount,
        action: action.to_string(),
        date: iso_string
    };

    activity
}

pub async fn get_statechain_info(statechain_id: &str, client_config: &ClientConfig) -> Result<Option<StatechainInfoResponsePayload>> {

    let path = format!("info/statechain/{}", statechain_id.to_string());

    let client = client_config.get_reqwest_client()?;
    let request = client.get(&format!("{}/{}", client_config.statechain_entity, path));

    let response = request.send().await?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let value = response.text().await?;

    let response: StatechainInfoResponsePayload = serde_json::from_str(value.as_str())?;

    Ok(Some(response))
}

/// Fetch a single-use SE auth challenge and return an endpoint-bound owner-auth token
/// `"<nonce>:<sig>"` for `coin` (audit [15]). Signs `sha256(nonce|endpoint)` with the coin's auth
/// key so a captured signature cannot be replayed against an irreversible owner op. Use in place of
/// the static `coin.signed_statechain_id` on nonce-protected endpoints.
pub async fn fresh_auth(
    client_config: &ClientConfig,
    statechain_id: &str,
    coin: &mercurylib::wallet::Coin,
    endpoint: &str,
) -> Result<String> {
    let client = client_config.get_reqwest_client()?;
    let resp = client
        .get(&format!("{}/auth/challenge/{}", client_config.statechain_entity, statechain_id))
        .send()
        .await?;
    if resp.status() != StatusCode::OK {
        return Err(anyhow!("auth challenge failed: {}", resp.text().await?));
    }
    let v: serde_json::Value = resp.json().await?;
    let nonce = v
        .get("nonce")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow!("no nonce in auth challenge response"))?;
    let sig = mercurylib::transfer::receiver::sign_message(&format!("{nonce}|{endpoint}"), coin)?;
    Ok(format!("{nonce}:{sig}"))
}

pub async fn complete_withdraw(statechain_id: &str, signed_statechain_id: &str, client_config: &ClientConfig) -> Result<()> {

    let endpoint = client_config.statechain_entity.clone();
    let path = "withdraw/complete";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    let delete_statechain_payload = WithdrawCompletePayload {
        statechain_id: statechain_id.to_string(),
        signed_statechain_id: signed_statechain_id.to_string(),
    };

    let response = request.json(&delete_statechain_payload).send().await?;

    if response.status() != 200 {
        let response_body = response.text().await?;
        return Err(anyhow!(response_body));
    }

    Ok(())

}