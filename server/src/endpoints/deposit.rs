use std::str::FromStr;

use bitcoin::hashes::{sha256, Hash};
use rocket::{serde::json::Json, response::status, State, http::Status};
use secp256k1_zkp::{XOnlyPublicKey, schnorr::Signature, Message, Secp256k1, PublicKey};
use serde::{Serialize, Deserialize};
use serde_json::{Value, json};
use crate::{server::StateChainEntity, server_config::Enclave};

/// Audit [26]: cap on outstanding (unspent) token rows, bounding DB write-amplification from
/// looped issuance. Checked by BOTH the free faucet (`get_token_no_server`, unauthenticated) and
/// the derived-token endpoint (`get_derived_token`, authenticated but free).
const MAX_UNSPENT_TOKENS: i64 = 100_000;

pub async fn get_token_no_server(statechain_entity: &State<StateChainEntity>, config: &crate::server_config::ServerConfig) -> status::Custom<Json<Value>>  {

    // Free token generation on mainnet is a deliberate operator OPT-IN (`free_tokens_on_mainnet`):
    // run onboarding unpriced and defer the pricing machinery until spam actually appears — the
    // outstanding-token cap below (audit [26]) stays on as the spam brake either way.
    if config.network == "mainnet" && !config.free_tokens_on_mainnet {
        let response_body = json!({
            "error": "Internal Server Error",
            "message": "Token generation not supported on mainnet (set free_tokens_on_mainnet = true to allow free onboarding, or configure token_server_url)."
        });

        return status::Custom(Status::InternalServerError, Json(response_body));
    }

    // Audit [26]: this endpoint is unauthenticated when free, so cap the number of unspent
    // token rows to bound DB write-amplification if a caller loops it.
    if crate::database::deposit::count_unspent_tokens(&statechain_entity.pool).await >= MAX_UNSPENT_TOKENS {
        return status::Custom(
            Status::TooManyRequests,
            Json(json!({ "message": "too many outstanding deposit tokens; consume some before requesting more" })),
        );
    }

    let token_id = uuid::Uuid::new_v4().to_string();

    crate::database::deposit::insert_new_token(&statechain_entity.pool, &token_id).await;

    let token = mercurylib::deposit::TokenResponse {
        token_id,
        payment_method: "free".to_string(),
        deposit_address: None,
        fee: 0,
        confirmation_target: 0,
    };

    let response_body = json!(token);

    return status::Custom(Status::Ok, Json(response_body));
}

pub async fn get_token_from_server(config: &crate::server_config::ServerConfig) -> status::Custom<Json<Value>>  {

    let client: reqwest::Client = reqwest::Client::new();
    let request = client.get(&format!("{}/token/token_gen", config.token_server_url.as_ref().unwrap()));

    let value = match request.send().await {
        Ok(response) => {
            let text = response.text().await.unwrap();
            text
        },
        Err(err) => {
            let response_body = json!({
                "message": err.to_string()
            });

            let err = err.status();
            let status = if err.is_some() {
                Status::from_code(err.unwrap().as_u16()).unwrap_or(Status::InternalServerError)
            } else {
                Status::InternalServerError
            };

        
            return status::Custom(status, Json(response_body));
        },
    };

    let response: serde_json::Value = serde_json::from_str(value.as_str()).expect(&format!("failed to parse: {}", value.as_str()));

    let token_id = response.get("token_id").unwrap().as_str().unwrap().to_string();
    let deposit_address = response.get("deposit_address").unwrap().as_str().unwrap().to_string();
    let fee = response.get("fee").unwrap().as_u64().unwrap();
    let confirmation_target = response.get("confirmation_target").unwrap().as_u64().unwrap();

    let token = mercurylib::deposit::TokenResponse {
        token_id,
        payment_method: "onchain".to_string(),
        deposit_address: Some(deposit_address),
        fee,
        confirmation_target,
    };

    let response_body = json!(token);

    return status::Custom(Status::Ok, Json(response_body));
}

#[get("/deposit/get_token")]
pub async fn get_token(statechain_entity: &State<StateChainEntity>) -> status::Custom<Json<Value>>  {

    let config = crate::server_config::ServerConfig::load();

    if config.token_server_url.is_none() {
        return get_token_no_server(statechain_entity, &config).await;
    } else {
        return get_token_from_server(&config).await;
    }
}

/// FREE **derived-slot** tokens: vouchers for statechain slots created by SE-co-signed flows over
/// an EXISTING statechain — off-chain split pieces/change, combine outputs, refresh re-anchors.
/// Such slots re-house value already inside the SE (no new on-chain onboarding surface), so they
/// are never routed to the token server: the SE mints them itself, on any network, marked with
/// their parent (`tokens.derived_from`).
///
/// Gates, in order:
/// 1. issuance enabled (`max_derived_tokens_per_statechain > 0`);
/// 2. requested `count` within the per-parent allowance;
/// 3. global outstanding-token cap (audit [26], shared with the faucet);
/// 4. OWNER auth: the audit-[15] single-use endpoint-bound challenge, signed by the parent's
///    CURRENT auth key — one consumed nonce mints the whole batch (a `transfer_many` needs N+1
///    slots but one authorization);
/// 5. per-parent LIFETIME cap, counted over `derived_from` (spent tokens included) — fail CLOSED
///    on a read error.
///
/// The blind SE cannot see how a slot is later funded, so a dishonest owner could point a fresh
/// L1 deposit at a derived slot to dodge a paid deployment's onboarding fee; the lifetime cap
/// bounds that, and `max_derived_tokens_per_statechain = 0` disables the endpoint outright
/// (TRUST-MODEL §7 records the residual).
#[post("/deposit/get_derived_token", format = "json", data = "<payload>")]
pub async fn get_derived_token(
    statechain_entity: &State<StateChainEntity>,
    payload: Json<mercurylib::deposit::DerivedTokenRequest>,
) -> status::Custom<Json<Value>> {
    let config = crate::server_config::ServerConfig::load();
    let cap = config.max_derived_tokens_per_statechain;

    if cap == 0 {
        return status::Custom(
            Status::Forbidden,
            Json(json!({ "message": "derived-token issuance is disabled on this server; use deposit/get_token" })),
        );
    }
    let count = payload.count;
    if count == 0 || count > cap {
        return status::Custom(
            Status::BadRequest,
            Json(json!({ "message": format!("count must be between 1 and {cap}") })),
        );
    }
    // Audit [26]: the same outstanding-token brake as the faucet — authenticated callers must not
    // amplify DB writes unboundedly either.
    if crate::database::deposit::count_unspent_tokens(&statechain_entity.pool).await
        >= MAX_UNSPENT_TOKENS
    {
        return status::Custom(
            Status::TooManyRequests,
            Json(json!({ "message": "too many outstanding deposit tokens; consume some before requesting more" })),
        );
    }
    // Owner auth: consumes the single-use nonce ONLY on a valid signature; also proves the parent
    // statechain exists (the auth key is looked up in statechain_data). Checked BEFORE the
    // per-parent count so an unauthenticated caller cannot probe issuance counts by statechain_id.
    if !crate::endpoints::utils::validate_signature_nonce(
        &statechain_entity.pool,
        &payload.auth_sig,
        &payload.statechain_id,
        "deposit/get_derived_token",
    )
    .await
    {
        return status::Custom(
            Status::Unauthorized,
            Json(json!({ "message": "signature does not match the statechain's authentication key (fresh auth/challenge nonce required)" })),
        );
    }
    // Per-parent LIFETIME allowance — fail CLOSED on a read error (audit [1] discipline).
    let issued = match crate::database::deposit::count_derived_tokens(
        &statechain_entity.pool,
        &payload.statechain_id,
    )
    .await
    {
        Ok(n) => n,
        Err(_) => {
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "could not read the derived-token allowance; retry" })),
            );
        }
    };
    if issued + count as i64 > cap as i64 {
        return status::Custom(
            Status::TooManyRequests,
            Json(json!({ "message": format!(
                "statechain {} has {} of its {cap} lifetime derived tokens already issued; {count} more would exceed the allowance",
                payload.statechain_id, issued
            ) })),
        );
    }

    match crate::database::deposit::insert_new_derived_tokens(
        &statechain_entity.pool,
        &payload.statechain_id,
        count,
    )
    .await
    {
        Ok(token_ids) => status::Custom(
            Status::Ok,
            Json(json!(mercurylib::deposit::DerivedTokenResponse { token_ids })),
        ),
        Err(_) => status::Custom(
            Status::ServiceUnavailable,
            Json(json!({ "message": "could not mint derived tokens; retry" })),
        ),
    }
}

fn get_random_enclave_index(statechain_id: &str, enclaves: &Vec<Enclave>) -> Result<usize, String> {
    let index_from_statechain_id = get_enclave_index_from_statechain_id(statechain_id, enclaves.len() as u32);

    let selected_enclave = enclaves.get(index_from_statechain_id).unwrap();
    if selected_enclave.allow_deposit {
        return Ok(index_from_statechain_id);
    } else {
        for (i, enclave) in enclaves.iter().enumerate() {
            if enclave.allow_deposit {
                return Ok(i);
            }
        }
    }

    Err("No valid enclave found with allow_deposit set to true".to_string())
}

fn get_enclave_index_from_statechain_id(statechain_id: &str, enclave_array_len: u32) -> usize {
    let hash = sha256::Hash::hash(statechain_id.as_bytes());
    let hash_bytes = hash.as_byte_array();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash_bytes[..16]);
    let random_number = u128::from_be_bytes(bytes);

    return (random_number % enclave_array_len as u128) as usize;
}

struct TokenStatusResponse {
    confirmed: bool,
    spent: bool,
    err: bool,
    status: Option<Status>,
    err_message: Option<String>
}

pub async fn check_token_status(token_id: &str) -> TokenStatusResponse{

    let config = crate::server_config::ServerConfig::load();

    let client: reqwest::Client = reqwest::Client::new();
    let request = client.get(&format!("{}/token/token_verify/{}", config.token_server_url.as_ref().unwrap(), token_id));

    let value = match request.send().await {
        Ok(response) => {
            let text = response.text().await.unwrap();
            text
        },
        Err(err) => {
            let message = err.to_string();

            let err = err.status();
            let status = if err.is_some() {
                Status::from_code(err.unwrap().as_u16()).unwrap_or(Status::InternalServerError)
            } else {
                Status::InternalServerError
            };

            return TokenStatusResponse {
                confirmed: false,
                spent: false,
                err: true,
                status: Some(status),
                err_message: Some(message),
            };
        },
    };

    let response: serde_json::Value = serde_json::from_str(value.as_str()).expect(&format!("failed to parse: {}", value.as_str()));

    let confirmed = response.get("confirmed").unwrap().as_bool().unwrap();
    let spent = response.get("spent").unwrap().as_bool().unwrap();

    return TokenStatusResponse {
        confirmed,
        spent,
        err: false,
        status: None,
        err_message: None,
    };
}

#[post("/deposit/init/pod", format = "json", data = "<deposit_msg1>")]
pub async fn post_deposit(statechain_entity: &State<StateChainEntity>, deposit_msg1: Json<mercurylib::deposit::DepositMsg1>) -> status::Custom<Json<Value>> {

    let statechain_entity = statechain_entity.inner();

    // Audit [19]: 400 on malformed pre-auth input instead of panicking (per-request DoS surface).
    let auth_key = match XOnlyPublicKey::from_str(&deposit_msg1.auth_key) {
        Ok(k) => k,
        Err(_) => return status::Custom(Status::BadRequest, Json(json!({ "message": "invalid auth_key" }))),
    };
    let token_id = deposit_msg1.token_id.clone();
    let signed_token_id = match Signature::from_str(&deposit_msg1.signed_token_id.to_string()) {
        Ok(s) => s,
        Err(_) => return status::Custom(Status::BadRequest, Json(json!({ "message": "invalid signed_token_id" }))),
    };

    let msg = Message::from_hashed_data::<sha256::Hash>(token_id.to_string().as_bytes());

    let secp = Secp256k1::new();
    if !secp.verify_schnorr(&signed_token_id, &msg, &auth_key).is_ok() {

        let response_body = json!({
            "message": "Signature does not match authentication key."
        });
    
        return status::Custom(Status::InternalServerError, Json(response_body));

    }

    let is_existing_key = crate::database::deposit::check_existing_key(&statechain_entity.pool, &auth_key).await;

    if is_existing_key {
        let response_body = json!({
            "message": "The authentication key is already assigned to a statecoin."
        });
    
        return status::Custom(Status::BadRequest, Json(response_body));
    }

   let token_info = crate::database::deposit::get_token_info(&statechain_entity.pool, &token_id).await;

   if token_info.is_none() {
        let response_body = json!({
            "error": "Deposit Error",
            "message": "Token ID not found."
        });

        return status::Custom(Status::NotFound, Json(response_body));
    }

    let token_info = token_info.unwrap();

    if token_info.spent {
        let response_body = json!({
            "message": "Token already spent."
        });

        return status::Custom(Status::Gone, Json(response_body));
    }

    if !token_info.confirmed {

        let token_status_response = check_token_status(&token_id).await;

        if token_status_response.err {
            let response_body = json!({
                "message": token_status_response.err_message.unwrap()
            });
        
            return status::Custom(token_status_response.status.unwrap(), Json(response_body));
        }

        if token_status_response.spent {
            let response_body = json!({
                "message": "Token already spent."
            });
    
            return status::Custom(Status::Gone, Json(response_body));
        }

        if !token_status_response.confirmed {
            let response_body = json!({
                "message": "Token not confirmed."
            });
        
            return status::Custom(Status::Gone, Json(response_body));
        }
    }

    // [REQ-68] `user_public_key` is forwarded so the SE can DERIVE the coin's aggregate itself at the
    // only moment it holds both halves, instead of trusting a caller-supplied `agg_pubkey` later.
    // Omitted entirely when the client sent nothing, because the lockbox refuses a malformed key
    // rather than ignoring it — an empty string is malformed, not absent.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GetPublicKeyRequestPayload {
        statechain_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        user_public_key: Option<String>,
    }

    let statechain_id = uuid::Uuid::new_v4().as_simple().to_string();

    let config = crate::server_config::ServerConfig::load();

    let enclave_index = get_random_enclave_index(&statechain_id, &config.enclaves).unwrap();

    let lockbox_endpoint = config.enclaves.get(enclave_index).unwrap().url.clone();
    let path = "get_public_key";

    let client: reqwest::Client = reqwest::Client::new();
    let request = client.post(&format!("{}/{}", lockbox_endpoint, path));

    let payload = GetPublicKeyRequestPayload {
        statechain_id: statechain_id.clone(),
        user_public_key: match deposit_msg1.user_public_key.trim() {
            "" => None,
            k => Some(k.to_string()),
        },
    };

    let value = match request.json(&payload).send().await {
        Ok(response) => {
            // [REQ-68] A refusal here means the SE could not bind this coin's aggregate. Surface it as
            // a refusal rather than feeding the error body to the JSON parser, which would panic and
            // report an unrelated fault.
            //
            // The body read propagates rather than defaulting to "": an unreadable reply is "I do not
            // know whether the SE minted a key", and defaulting turns that into a deposit that looks
            // like it failed for a reason we can name. Fail closed and say which step was unreadable.
            let status = response.status();
            let text = match response.text().await {
                Ok(t) => t,
                Err(err) => {
                    return status::Custom(
                        Status::InternalServerError,
                        Json(json!({
                            "message": format!(
                                "the enclave replied {} but its body could not be read ({err}); \
                                 refusing rather than treating an unreadable reply as empty",
                                status.as_u16()
                            )
                        })),
                    );
                }
            };
            if !status.is_success() {
                return status::Custom(
                    Status::from_code(status.as_u16()).unwrap_or(Status::InternalServerError),
                    Json(json!({ "message": format!("enclave refused key generation: {}", text) })),
                );
            }
            text
        },
        Err(err) => {
            let response_body = json!({
                "error": "Internal Server Error",
                "message": err.to_string()
            });

            return status::Custom(Status::InternalServerError, Json(response_body));
        },
    };

    #[derive(Serialize, Deserialize)]
    pub struct PublicNonceRequestPayload<'r> {
        server_pubkey: &'r str,
    }

    let response: PublicNonceRequestPayload = serde_json::from_str(value.as_str()).expect(&format!("failed to parse: {}", value.as_str()));

    let mut server_pubkey_hex = response.server_pubkey.to_string();

    if server_pubkey_hex.starts_with("0x") {
        server_pubkey_hex = server_pubkey_hex[2..].to_string();
    }

    let server_pubkey = PublicKey::from_str(&server_pubkey_hex).unwrap();

    // [FATAL-B] Bind statechain_id -> aggregate authoritatively. If the client sent its owner signing
    // share, compute the coin's aggregate x-only key (owner_share + enclave_share) and record it UNIQUE
    // per sid, so a receiver can later verify a coin's on-chain aggregate against this server record
    // instead of a sender-supplied user key — closing the split decoy-counter path. Old clients send an
    // empty user_public_key => store NULLs and keep the legacy behaviour.
    let (user_pubkey_opt, aggregate_xonly_opt): (Option<PublicKey>, Option<[u8; 32]>) =
        if deposit_msg1.user_public_key.trim().is_empty() {
            (None, None)
        } else {
            match PublicKey::from_str(&deposit_msg1.user_public_key) {
                Ok(user_pk) => match user_pk.combine(&server_pubkey) {
                    Ok(agg) => (Some(user_pk), Some(agg.x_only_public_key().0.serialize())),
                    Err(_) => return status::Custom(Status::BadRequest, Json(json!({ "message": "invalid user_public_key (aggregate combine failed)" }))),
                },
                Err(_) => return status::Custom(Status::BadRequest, Json(json!({ "message": "invalid user_public_key" }))),
            }
        };

    let epoch_deadline = deposit_msg1.epoch_deadline.map(|e| e as i64);
    crate::database::deposit::insert_new_deposit(&statechain_entity.pool, &token_id, &auth_key, &server_pubkey, &statechain_id, enclave_index as i32, deposit_msg1.single_use, epoch_deadline, user_pubkey_opt.as_ref(), aggregate_xonly_opt.as_ref()).await;

    crate::database::deposit::set_token_spent(&statechain_entity.pool, &token_id).await;

    let deposit_msg1_response = mercurylib::deposit::DepositMsg1Response {
        server_pubkey: server_pubkey.to_string(),
        statechain_id,
    };

    let response_body = json!(deposit_msg1_response);

    status::Custom(Status::Ok, Json(response_body))
}
