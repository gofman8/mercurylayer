use electrum_client::ElectrumApi;
use mercurylib::{transaction::{SignFirstRequestPayload, PartialSignatureRequestPayload, PartialSignatureResponsePayload, get_partial_sig_request, create_signature, new_backup_transaction}, wallet::Coin};
use anyhow::Result;
use reqwest::StatusCode;
use secp256k1_zkp::musig::MusigPartialSignature;
use serde_json::Value;
use crate::client_config::ClientConfig;

pub async fn new_transaction(
    client_config: &ClientConfig, 
    coin: &mut Coin, 
    to_address: &str, 
    qt_backup_tx: u32, 
    is_withdrawal: bool, 
    block_height: Option<u32>, 
    network: &str, 
    fee_rate_sats_per_byte: f64,
    initlock: u32,
    interval: u32) -> Result<String> {

    // TODO: validate address first

    let coin_nonce = mercurylib::transaction::create_and_commit_nonces(&coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce);
    coin.public_nonce = Some(coin_nonce.public_nonce);
    coin.blinding_factor = Some(coin_nonce.blinding_factor);

    let server_public_nonce = sign_first(&client_config, &coin_nonce.sign_first_request_payload).await?;

    coin.server_public_nonce = Some(server_public_nonce);

    let block_height = match block_height {
        Some(block_height) => block_height,
        None => {
            let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
            block_header.height as u32
        },
    };

    let partial_sig_request = get_partial_sig_request(
        &coin, 
        block_height, 
        initlock, 
        interval, 
        fee_rate_sats_per_byte,
        qt_backup_tx,
        to_address.to_string(),
        network.to_string(),
        is_withdrawal)?;

    let server_partial_sig_request = partial_sig_request.partial_signature_request_payload;

    let server_partial_sig = sign_second(&client_config, &server_partial_sig_request).await?;

    let client_partial_sig_hex = partial_sig_request.client_partial_sig;
    let server_partial_sig_hex = hex::encode(server_partial_sig.serialize());
    let msg = partial_sig_request.msg;
    let session_hex = partial_sig_request.encoded_session;
    let output_pubkey_hex = partial_sig_request.output_pubkey;

    let encoded_unsigned_tx = partial_sig_request.encoded_unsigned_tx;
    
    let signature = create_signature(msg, client_partial_sig_hex, server_partial_sig_hex, session_hex, output_pubkey_hex)?;

    let signed_tx = new_backup_transaction(encoded_unsigned_tx, signature)?;

    Ok(signed_tx)
}

/// **The SE's own words for a refusal, or the raw body when it did not speak JSON.**
///
/// Every route in this file needs this and each one used to inline it, which is how the same four
/// lines came to exist four times — and why the silent-degradation guard fired on the fourth copy
/// rather than the first. Written once, it is also reviewable once.
///
/// **It never turns a failure into a success.** The caller has already decided this is an error and
/// is on its way to returning one; all that is chosen here is the TEXT. The fallback is the raw body,
/// which is strictly MORE information than a generic message — the opposite of the degradation the
/// guard exists to catch. This matters on this route in particular: `collapse_grant`'s refusals are
/// six distinct named gates, and a client that cannot tell them apart cannot act on any of them.
fn se_error_detail(value: &str) -> String {
    serde_json::from_str::<Value>(value)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| value.to_string())
}

/// This function gets the server public nonce from the statechain entity.
pub async fn sign_first(client_config: &ClientConfig, sign_first_request_payload: &SignFirstRequestPayload) -> Result<String> {

    let endpoint = client_config.statechain_entity.clone();
    let path = "sign/first";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    // let value = request.json(&sign_first_request_payload).send().await?.text().await?;

    let response = request.json(&sign_first_request_payload).send().await?;

    let status = response.status();

    let value = response.text().await?;

    if status != StatusCode::OK{
        // The SE's error body is usually {"message": ".."}, but under load a panic/5xx can return a
        // different JSON shape (Rocket's default {"error": {..}}) or non-JSON. Never unwrap the
        // "message" field — fall back to the raw body so a server hiccup surfaces as a clean Err
        // instead of a client-side panic that crashes the caller's task.
        let detail = se_error_detail(value.as_str());
        return Err(anyhow::anyhow!("sign/first failed ({}): {}", status, detail));
    }

    let sign_first_response_payload: mercurylib::transaction::SignFirstResponsePayload = serde_json::from_str(value.as_str())?;

    let mut server_pubnonce_hex = sign_first_response_payload.server_pubnonce.to_string();

    if server_pubnonce_hex.starts_with("0x") {
        server_pubnonce_hex = server_pubnonce_hex[2..].to_string();
    }

    Ok(server_pubnonce_hex)
}

/// **[REQ-56] Mint the secnonce a collapse will consume.**
///
/// The same request as [`sign_first`], to a different route, for a reason that took a live
/// measurement to see: `sign/first` refuses `410 Gone` once a coin's spend budget is exhausted, and a
/// root worth collapsing is a root that has been SPLIT — which is exactly what exhausts it. Every
/// genuine tree measured on the live server (13 of the 14 roots holding more than one leaf) was in
/// that state, so the collapse could never obtain the nonce its own route requires.
///
/// See `server/src/endpoints/collapse.rs` for why exempting this one route from that gate does not
/// weaken it: the ordinary route re-checks the budget itself, so a collapse nonce cannot be spent on
/// an ordinary transaction.
pub async fn collapse_first(
    client_config: &ClientConfig,
    sign_first_request_payload: &SignFirstRequestPayload,
) -> Result<String> {
    let endpoint = client_config.statechain_entity.clone();
    let client = client_config.get_reqwest_client()?;
    let response = client
        .post(&format!("{}/{}", endpoint, "collapse/first"))
        .json(&sign_first_request_payload)
        .send()
        .await?;

    let status = response.status();
    let value = response.text().await?;
    if status != StatusCode::OK {
        let detail = se_error_detail(value.as_str());
        return Err(anyhow::anyhow!("collapse/first failed ({}): {}", status, detail));
    }

    let payload: mercurylib::transaction::SignFirstResponsePayload =
        serde_json::from_str(value.as_str())?;
    let mut server_pubnonce_hex = payload.server_pubnonce.to_string();
    if server_pubnonce_hex.starts_with("0x") {
        server_pubnonce_hex = server_pubnonce_hex[2..].to_string();
    }
    Ok(server_pubnonce_hex)
}

/// **[REQ-56 / REQ-82] Ask the SE for its half of a collapse, and return its answer whole.**
///
/// The SE's refusals are the interesting half of this route — six distinct gates, each naming its own
/// cause — so a failure is surfaced with the SE's own words rather than a generic message. A caller
/// that cannot tell "does not pay every unreleased frontier leaf in full" from "session does not
/// reproduce the disclosed transaction" cannot act on either.
pub async fn collapse_grant(
    client_config: &ClientConfig,
    payload: &mercurylib::transaction::CollapseGrantRequestPayload,
) -> Result<mercurylib::transaction::CollapseGrantResponse> {
    let endpoint = client_config.statechain_entity.clone();
    let client = client_config.get_reqwest_client()?;
    let response = client
        .post(&format!("{}/{}", endpoint, "collapse_grant"))
        .json(payload)
        .send()
        .await?;

    let status = response.status();
    let value = response.text().await?;
    if status != StatusCode::OK {
        let detail = se_error_detail(value.as_str());
        return Err(anyhow::anyhow!("collapse_grant refused ({}): {}", status, detail));
    }

    Ok(serde_json::from_str::<mercurylib::transaction::CollapseGrantResponse>(value.as_str())?)
}

pub async fn sign_second(client_config: &ClientConfig, partial_sig_request: &PartialSignatureRequestPayload) -> Result<MusigPartialSignature> {
    let endpoint = client_config.statechain_entity.clone();
    let path = "sign/second";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    let response = request.json(&partial_sig_request).send().await?;

    let status = response.status();

    let value = response.text().await?;

    if status != StatusCode::OK {
        // Mirror sign/first (which already checks status): the SE's error body is {"message": ..}.
        // A 409 here is the SE REFUSING MuSig2 nonce reuse — signing twice over one secnonce with a
        // different challenge would leak the SE key share (INVALIDATION-SPEC IVL-ERR-9); a 400 is a
        // malformed session. Surface the typed status so the caller can distinguish "SE refused" from
        // "garbage response" instead of failing on an opaque serde parse error (adversarial-log
        // review: sign/second previously swallowed the status and mis-reported these refusals).
        let detail = se_error_detail(value.as_str());
        return Err(anyhow::anyhow!("sign/second failed ({}): {}", status, detail));
    }

    let response: PartialSignatureResponsePayload = serde_json::from_str(value.as_str())?;

    let mut server_partial_sig_hex = response.partial_sig.to_string();

    if server_partial_sig_hex.starts_with("0x") {
        server_partial_sig_hex = server_partial_sig_hex[2..].to_string();
    }

    let server_partial_sig_bytes = hex::decode(server_partial_sig_hex)?;

    let server_partial_sig = MusigPartialSignature::from_slice(server_partial_sig_bytes.as_slice())?;

    Ok(server_partial_sig)
}