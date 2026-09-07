use anyhow::{anyhow, Result, Ok};
use mercurylib::{deposit::{create_deposit_msg1_with_options, create_aggregated_address}, wallet::{Wallet, Coin}};

use crate::{client_config::ClientConfig, sqlite_manager::{get_wallet, update_wallet}};

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

    // ═══ REFUSE AN UNFUNDABLE DEPOSIT BEFORE THE USER SENDS ANYTHING ═══
    //
    // A coin's only exit material is its TES-R ladder, and the ladder is established at first sight
    // of this deposit. A deposit too small to fund three tiers therefore cannot become a usable
    // coin: `tesr::establish` refuses it (before any co-signature, deliberately), the coin is never
    // booked, and the satoshis sit at an address only a cooperative withdrawal could reach. Issuing
    // an address for such an amount invites exactly that, so the amount is checked HERE, where
    // refusing costs the user nothing but a corrected number.
    //
    // The floor is the builder's own (`mercurylib::tesr::ladder_floor`), not a second constant to
    // drift from it. A token onboarding pays the same floor; a carrier's coloured ladder costs more
    // still and is gated separately by `colored_ladder_floor`.
    {
        // `for_network` PANICS on an unrecognised name. This is the first thing a deposit touches,
        // and a panic here would take the caller down instead of telling it what is wrong, so the
        // checked variant is used and an unknown network is a named refusal.
        let p = mercurylib::tesr::TesrParams::for_network_checked(&wallet.network).ok_or_else(|| {
            anyhow!(
                "unknown network {:?}: refusing to issue a deposit address, because the TES-R \
                 schedule that sizes the coin's ladder cannot be resolved for it.",
                wallet.network
            )
        })?;
        let floor = mercurylib::tesr::ladder_floor(p.committed_fee_rate, mercurylib::tesr::DUST_LIMIT);
        if (amount as u64) < floor {
            return Err(anyhow!(
                "a deposit of {amount} sat cannot be laddered: a TES-R ladder costs {floor} sat at \
                 {} sat/vB (three tiers, each burning a committed fee plus the {} sat anchor, and a \
                 final state output that still clears the {} sat dust floor), and a coin without a \
                 ladder has no exit material at all. Refusing to issue a deposit address for an \
                 amount that could never become a usable coin — deposit at least {floor} sat.",
                p.committed_fee_rate,
                mercurylib::tesr::P2A_VALUE,
                mercurylib::tesr::DUST_LIMIT
            ));
        }
    }

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
/// current owner can draw on the parent's allowance.
///
/// Three outcomes, so the caller never silently spends paid onboarding tokens on a transient fault
/// (external review finding 4):
/// - `Ok(Some(ids))` — issued.
/// - `Ok(None)` — the SE does not OFFER derived issuance at all (endpoint absent → 404, or disabled
///   → 403). This is the only case where legacy onboarding-token fallback is appropriate.
/// - `Err(_)` — a real failure: transient (5xx), auth (401), allowance exhausted (429), or a
///   malformed reply. The caller MUST surface this, not quietly consume prepaid/paid tokens.
pub async fn get_derived_tokens(
    client_config: &ClientConfig,
    parent_coin: &Coin,
    parent_statechain_id: &str,
    count: u32,
) -> Result<Option<Vec<String>>> {
    let client = client_config.get_reqwest_client()?;

    // Fetch the owner-auth challenge inline so a 404 here (a server predating audit-[15]/derived
    // tokens) collapses to the same "not offered" outcome as a 404 on the derived route itself.
    let challenge_resp = client
        .get(&format!("{}/auth/challenge/{}", client_config.statechain_entity, parent_statechain_id))
        .send()
        .await?;
    if challenge_resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if challenge_resp.status() != reqwest::StatusCode::OK {
        return Err(anyhow!("auth challenge failed: {}", challenge_resp.text().await?));
    }
    let v: serde_json::Value = challenge_resp.json().await?;
    let nonce = v
        .get("nonce")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow!("no nonce in auth challenge response"))?;
    let sig = mercurylib::transfer::receiver::sign_message(
        &format!("{nonce}|deposit/get_derived_token"),
        parent_coin,
    )?;
    let auth_sig = format!("{nonce}:{sig}");

    let payload = mercurylib::deposit::DerivedTokenRequest {
        statechain_id: parent_statechain_id.to_string(),
        auth_sig,
        count,
    };

    let response = client
        .post(&format!("{}/deposit/get_derived_token", client_config.statechain_entity))
        .json(&payload)
        .send()
        .await?;

    let status = response.status();
    // 404 (route not mounted) or 403 (derived issuance disabled by the operator): the server does
    // not offer derived tokens → the caller may use the legacy onboarding path.
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::FORBIDDEN {
        return Ok(None);
    }
    if status != reqwest::StatusCode::OK {
        // Transient / auth / allowance-exhausted: a genuine failure the caller must surface.
        return Err(anyhow!("derived-token request failed ({status}): {}", response.text().await?));
    }

    let value = response.text().await?;
    let resp: mercurylib::deposit::DerivedTokenResponse = serde_json::from_str(value.as_str())?;
    if resp.token_ids.len() != count as usize {
        return Err(anyhow!(
            "SE returned {} derived tokens, expected {count}",
            resp.token_ids.len()
        ));
    }
    Ok(Some(resp.token_ids))
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
