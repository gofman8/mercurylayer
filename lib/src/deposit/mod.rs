use std::str::FromStr;

use crate::{error::MercuryError, utils::get_network, wallet::Coin};
use bitcoin::{hashes::sha256, PrivateKey, secp256k1, Address};
use secp256k1_zkp::{Message, Secp256k1, PublicKey};
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenID {
    pub token_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token_id: String,
    pub payment_method: String,
    pub deposit_address: Option<String>,
    pub fee: u64,
    pub confirmation_target: u64,
}

/// Request for FREE **derived-slot** deposit tokens (`POST /deposit/get_derived_token`): vouchers
/// for statechain slots created by SE-co-signed flows over an EXISTING statechain the requester
/// currently owns — off-chain split pieces/change, combine outputs, refresh re-anchors. These
/// slots re-house value already inside the SE, so they are not charged the on-chain onboarding
/// fee. `auth_sig` is the single-use endpoint-bound challenge response `"<nonce>:<sig>"` (audit
/// [15]): a schnorr signature by the PARENT's current auth key over
/// `sha256(nonce|"deposit/get_derived_token")`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DerivedTokenRequest {
    /// The parent statechain acting as the voucher (must exist at the SE; requester must own it).
    pub statechain_id: String,
    /// `"<nonce>:<sig>"` over the `deposit/get_derived_token` endpoint (see above).
    pub auth_sig: String,
    /// Tokens to mint under this one challenge — a split needs 2 (piece+change), a
    /// `transfer_many` batch N+1, a refresh 1. Counted against the parent's LIFETIME allowance
    /// (`max_derived_tokens_per_statechain`). Default 1.
    #[serde(default = "default_derived_token_count")]
    pub count: u32,
}

fn default_derived_token_count() -> u32 {
    1
}

/// Response to [`DerivedTokenRequest`]: the minted token ids, already CONFIRMED and payment-free
/// (each funds one `deposit/init/pod` like any other token).
#[derive(Debug, Serialize, Deserialize)]
pub struct DerivedTokenResponse {
    pub token_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DepositMsg1 {
    pub auth_key: String,
    pub token_id: String,
    pub signed_token_id: String,
    /// Single-use coin: once the SE co-signs one terminal spend it refuses any further spend (the
    /// off-chain RGB split/combine double-spend guard). Defaults to false (normal re-signable coin),
    /// so existing clients are unaffected.
    #[serde(default)]
    pub single_use: bool,
    /// Epoch deadline (unix seconds): the SE refuses to co-sign any new spend once its own clock
    /// passes the deadline (Stage 4). None = no epoch. `serde(default)` keeps old clients working.
    #[serde(default)]
    pub epoch_deadline: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DepositMsg1Response {
    pub server_pubkey: String,
    pub statechain_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DepositInitResult {
    pub server_pubkey: String,
    pub statechain_id: String,
    pub signed_statechain_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AggregatedPublicKey {
    pub aggregate_pubkey: String,
    pub aggregate_address: String,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn create_deposit_msg1(coin: &Coin, token_id: &str) -> Result<DepositMsg1, MercuryError>{
    create_deposit_msg1_with_options(coin, token_id, false, None)
}

/// Like [`create_deposit_msg1`] but lets the caller request a **single-use** coin (the SE refuses any
/// second spend once it co-signs one terminal spend — the off-chain RGB split/combine guard).
pub fn create_deposit_msg1_with_single_use(coin: &Coin, token_id: &str, single_use: bool) -> Result<DepositMsg1, MercuryError>{
    create_deposit_msg1_with_options(coin, token_id, single_use, None)
}

/// Full-control variant: request single-use and/or an epoch deadline (unix seconds, Stage 4). The SE
/// refuses any new co-signature once its clock passes `epoch_deadline`, so the owner must transact or
/// exit before then; unilateral exit needs no SE co-signature.
pub fn create_deposit_msg1_with_options(coin: &Coin, token_id: &str, single_use: bool, epoch_deadline: Option<u64>) -> Result<DepositMsg1, MercuryError>{
    let msg = Message::from_hashed_data::<sha256::Hash>(token_id.to_string().as_bytes());

    let secp = Secp256k1::new();
    let auth_secret_key = PrivateKey::from_wif(&coin.auth_privkey)?.inner;
    let keypair = secp256k1::KeyPair::from_seckey_slice(&secp, auth_secret_key.as_ref())?;
    let signed_token_id = secp.sign_schnorr(&msg, &keypair);

    let auth_xonly_pubkey = PublicKey::from_str(&coin.auth_pubkey)?.x_only_public_key().0;

    let deposit_msg_1 = DepositMsg1 {
        auth_key: auth_xonly_pubkey.to_string(),
        token_id: token_id.to_string(),
        signed_token_id: signed_token_id.to_string(),
        single_use,
        epoch_deadline,
    };

    Ok(deposit_msg_1)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn handle_deposit_msg_1_response(coin: &Coin, deposit_msg_1_response: &DepositMsg1Response) -> Result<DepositInitResult, MercuryError> {

    let secp = Secp256k1::new();

    let server_pubkey_share = PublicKey::from_str(&deposit_msg_1_response.server_pubkey).unwrap();

    let statechain_id = deposit_msg_1_response.statechain_id.to_string();

    let auth_secret_key = PrivateKey::from_wif(&coin.auth_privkey)?.inner;
    let keypair = secp256k1::KeyPair::from_seckey_slice(&secp, auth_secret_key.as_ref()).unwrap();

    let msg = Message::from_hashed_data::<sha256::Hash>(statechain_id.to_string().as_bytes());
    let signed_statechain_id = secp.sign_schnorr(&msg, &keypair);

    Ok(DepositInitResult {
        server_pubkey: server_pubkey_share.to_string(),
        statechain_id,
        signed_statechain_id: signed_statechain_id.to_string(),
    })
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn create_aggregated_address(coin: &Coin, network: String) -> Result<AggregatedPublicKey, MercuryError> {

    let network = get_network(&network)?;

    let secp = Secp256k1::new();

    let user_pubkey_share = PublicKey::from_str(&coin.user_pubkey)?;
    let server_pubkey_share = PublicKey::from_str(&coin.server_pubkey.as_ref().unwrap())?;

    let aggregate_pubkey = user_pubkey_share.combine(&server_pubkey_share)?;

    let aggregated_xonly_pubkey = aggregate_pubkey.x_only_public_key().0; 

    let aggregate_address = Address::p2tr(&secp, aggregated_xonly_pubkey, None, network);

    Ok(AggregatedPublicKey {
        aggregate_pubkey: aggregate_pubkey.to_string(),
        aggregate_address: aggregate_address.to_string(),
    })

}

#[cfg(test)]
mod derived_token_tests {
    use super::DerivedTokenRequest;

    // Wire-compat: `count` is optional and defaults to 1, so a minimal client that only ever needs
    // one derived slot (e.g. a refresh) can omit it.
    #[test]
    fn count_defaults_to_one() {
        let req: DerivedTokenRequest =
            serde_json::from_str(r#"{"statechain_id":"abc","auth_sig":"n:s"}"#).unwrap();
        assert_eq!(req.count, 1);
        let req: DerivedTokenRequest =
            serde_json::from_str(r#"{"statechain_id":"abc","auth_sig":"n:s","count":7}"#).unwrap();
        assert_eq!(req.count, 7);
    }
}