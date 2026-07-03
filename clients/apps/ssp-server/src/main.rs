//! mercury-ssp — the SSP service over HTTP.
//!
//! Wraps `mercury_spark_sdk::ssp::SspService` (a statechain wallet + an RLN Lightning node):
//!   GET  /info                    -> { spark_address, fee_sats }
//!   POST /quote   { invoice }     -> { amount_sats, fee_sats, payment_hash, ssp_address }
//!   POST /pay     { invoice, batch_id } -> { preimage }         (user latched the coin first)
//!   POST /receive { amount_sats, receiver_address } -> { invoice, batch_id, payment_hash }
//!                                 (settlement is driven in the background)
//!
//! Env: SSP_WALLET_NAME, SSP_NETWORK (regtest|mainnet), SSP_RLN_API, SSP_FEE_SATS,
//!      SSP_SE_URL / SSP_ELECTRUM_URL (mainnet), ROCKET_PORT (default 8100).

#[macro_use]
extern crate rocket;

use mercury_spark_sdk::ssp::{RlnClient, SspService};
use mercury_spark_sdk::{SdkConfig, SparkWallet};
use rocket::serde::json::Json;
use rocket::State;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

struct App {
    ssp: Arc<SspService>,
}

#[derive(Deserialize)]
struct QuoteReq {
    invoice: String,
}

#[derive(Deserialize)]
struct PayReq {
    invoice: String,
    batch_id: String,
}

#[derive(Deserialize)]
struct ReceiveReq {
    amount_sats: u64,
    receiver_address: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

fn err(e: anyhow::Error) -> Json<Value> {
    Json(json!({ "error": e.to_string() }))
}

#[get("/info")]
async fn info(app: &State<App>) -> Json<Value> {
    match app.ssp.wallet.get_spark_address().await {
        Ok(addr) => Json(json!({ "spark_address": addr, "fee_sats": app.ssp.fee_sats })),
        Err(e) => err(e),
    }
}

#[post("/quote", format = "json", data = "<req>")]
async fn quote(app: &State<App>, req: Json<QuoteReq>) -> Json<Value> {
    match app.ssp.quote_pay(&req.invoice).await {
        Ok(q) => Json(json!({
            "amount_sats": q.amount_sats,
            "fee_sats": q.fee_sats,
            "payment_hash": q.payment_hash,
            "ssp_address": q.ssp_address,
        })),
        Err(e) => err(e),
    }
}

#[post("/pay", format = "json", data = "<req>")]
async fn pay(app: &State<App>, req: Json<PayReq>) -> Json<Value> {
    match app.ssp.execute_pay(&req.invoice, &req.batch_id).await {
        Ok(preimage) => Json(json!({ "preimage": preimage })),
        Err(e) => err(e),
    }
}

#[post("/receive", format = "json", data = "<req>")]
async fn receive(app: &State<App>, req: Json<ReceiveReq>) -> Json<Value> {
    match app.ssp.create_receive(req.amount_sats, &req.receiver_address).await {
        Ok(swap) => {
            // Drive settlement in the background: HTLC pending -> latch confirm -> claim.
            let ssp = app.ssp.clone();
            let swap_bg = swap.clone();
            tokio::spawn(async move {
                if let Err(e) = ssp.settle_receive(&swap_bg).await {
                    eprintln!("receive swap {} failed: {e}", swap_bg.batch_id);
                }
            });
            Json(json!({
                "invoice": swap.invoice,
                "batch_id": swap.batch_id,
                "payment_hash": swap.payment_hash,
            }))
        }
        Err(e) => err(e),
    }
}

#[launch]
async fn rocket() -> _ {
    let wallet_name = std::env::var("SSP_WALLET_NAME").unwrap_or_else(|_| "ssp".to_string());
    let network = std::env::var("SSP_NETWORK").unwrap_or_else(|_| "regtest".to_string());
    let rln_api = std::env::var("SSP_RLN_API").unwrap_or_else(|_| "http://127.0.0.1:3101".to_string());
    let fee_sats: u64 = std::env::var("SSP_FEE_SATS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);

    let mut cfg = match network.as_str() {
        "regtest" => SdkConfig::regtest(&wallet_name),
        _ => SdkConfig::mainnet(
            &wallet_name,
            &std::env::var("SSP_SE_URL").expect("SSP_SE_URL required"),
            &std::env::var("SSP_ELECTRUM_URL").expect("SSP_ELECTRUM_URL required"),
        ),
    };
    if let Ok(db) = std::env::var("SSP_DATABASE_FILE") {
        cfg.database_file = db;
    }

    let (wallet, _mnemonic) = SparkWallet::initialize(cfg, None)
        .await
        .expect("SSP wallet init failed");
    // Keep claiming incoming latched coins (pay swaps) automatically.
    let _bg = wallet.start_background();

    let ssp = Arc::new(SspService::new(wallet, RlnClient::new(&rln_api), fee_sats));
    rocket::build()
        .manage(App { ssp })
        .mount("/", routes![info, quote, pay, receive])
}
