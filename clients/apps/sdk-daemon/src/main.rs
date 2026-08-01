//! mercury-utexo-sdkd — JSON-lines stdio daemon over mercury-utexo-sdk.
//!
//! Protocol: one JSON object per line on stdin/stdout.
//!   request:  {"id": 1, "method": "get_balance", "params": {...}}
//!   response: {"id": 1, "result": ...} | {"id": 1, "error": "..."}
//!   event:    {"event": "TransferClaimed", "data": {...}}     (after `start_background`)
//!
//! One wallet per process (initialize first). Non-Rust SDKs (nodejs/web-native hosts) spawn this
//! binary and mirror the method surface 1:1.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

struct State {
    wallet: Option<UtexoWallet>,
    bg: Option<tokio::task::JoinHandle<()>>,
}

fn cfg_from_params(p: &Value) -> Result<SdkConfig> {
    let name = p
        .get("wallet_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("wallet_name required"))?;
    let network = p.get("network").and_then(|v| v.as_str()).unwrap_or("regtest");
    let mut cfg = match network {
        "regtest" => SdkConfig::regtest(name),
        "bitcoin" | "mainnet" => SdkConfig::mainnet(
            name,
            p.get("statechain_entity_url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("statechain_entity_url required for mainnet"))?,
            p.get("electrum_url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("electrum_url required for mainnet"))?,
        ),
        other => return Err(anyhow!("unsupported network {other}")),
    };
    if let Some(v) = p.get("statechain_entity_url").and_then(|v| v.as_str()) {
        cfg.statechain_entity_url = v.to_string();
    }
    if let Some(v) = p.get("electrum_url").and_then(|v| v.as_str()) {
        cfg.electrum_url = v.to_string();
    }
    if let Some(v) = p.get("database_file").and_then(|v| v.as_str()) {
        cfg.database_file = v.to_string();
    }
    if let Some(v) = p.get("rgb_proxy_url").and_then(|v| v.as_str()) {
        cfg.rgb_proxy_url = Some(v.to_string());
    }
    if let Some(v) = p.get("rgb_data_dir").and_then(|v| v.as_str()) {
        cfg.rgb_data_dir = Some(v.to_string());
    }
    if let Some(v) = p.get("poll_interval_secs").and_then(|v| v.as_u64()) {
        cfg.poll_interval_secs = v;
    }
    Ok(cfg)
}

async fn dispatch(state: &Arc<Mutex<State>>, method: &str, params: &Value) -> Result<Value> {
    // initialize is the only method allowed without a wallet.
    if method == "initialize" {
        let cfg = cfg_from_params(params)?;
        let mnemonic = params.get("mnemonic").and_then(|v| v.as_str());
        let (wallet, out_mnemonic) = UtexoWallet::initialize(cfg, mnemonic).await?;
        let mut st = state.lock().await;
        st.wallet = Some(wallet);
        return Ok(json!({ "mnemonic": out_mnemonic }));
    }

    // import_recovery_bundle also runs without a wallet (it CREATES one from a full backup — the
    // complete recovery of local exit material the SE cannot re-serve; review B7).
    if method == "import_recovery_bundle" {
        let cfg = cfg_from_params(params)?;
        let bundle = params
            .get("bundle_json")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("bundle_json required"))?;
        let (wallet, out_mnemonic) = UtexoWallet::import_recovery_bundle(cfg, bundle).await?;
        let mut st = state.lock().await;
        st.wallet = Some(wallet);
        return Ok(json!({ "mnemonic": out_mnemonic }));
    }

    let wallet = {
        let st = state.lock().await;
        st.wallet
            .clone()
            .ok_or_else(|| anyhow!("call initialize first"))?
    };

    let p = |k: &str| -> Result<String> {
        params
            .get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("param '{k}' required"))
    };
    let pu64 = |k: &str| -> Result<u64> {
        params
            .get(k)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow!("param '{k}' required"))
    };

    Ok(match method {
        "get_utexo_address" => json!(wallet.get_utexo_address().await?),
        "export_recovery_bundle" => json!(wallet.export_recovery_bundle().await?),
        "get_identity_public_key" => json!(wallet.get_identity_public_key().await?),
        "get_balance" => serde_json::to_value(wallet.get_balance().await?)?,
        "get_token_balances" => {
            // Token balances are u64 RAW units and must not ride JSON as numbers: every JS
            // consumer parses JSON numbers as f64 doubles and silently rounds above 2^53 (an
            // 18-decimals asset gets there with a supply of 10). Balances travel as STRINGS;
            // amounts a client SENDS stay numbers (clients cap them at MAX_SAFE_INTEGER).
            Value::Array(
                wallet
                    .get_token_balances()
                    .await?
                    .into_iter()
                    .map(|t| {
                        json!({
                            "asset_id": t.asset_id, "ticker": t.ticker, "name": t.name,
                            "precision": t.precision,
                            "balance": t.balance.to_string(), "total": t.total.to_string(),
                        })
                    })
                    .collect(),
            )
        }
        "get_deposit_address" => json!(wallet.get_deposit_address(pu64("amount_sats")?).await?),
        "add_prepaid_token" => {
            wallet.add_prepaid_token(&p("token_id")?).await;
            json!(true)
        }
        "claim" => serde_json::to_value(wallet.claim().await?)?,
        "start_background" => {
            let mut st = state.lock().await;
            if st.bg.is_none() {
                // Forward wallet events to stdout as JSON-lines events.
                let mut rx = wallet.subscribe();
                tokio::spawn(async move {
                    while let Ok(ev) = rx.recv().await {
                        let (name, data) = match ev {
                            WalletEvent::DepositConfirmed { address, amount_sats } => (
                                "DepositConfirmed",
                                json!({"address": address, "amount_sats": amount_sats}),
                            ),
                            WalletEvent::TransferClaimed { statechain_ids } => (
                                "TransferClaimed",
                                json!({"statechain_ids": statechain_ids}),
                            ),
                            WalletEvent::TokenTransferClaimed { asset_id, amount, statechain_id } => (
                                "TokenTransferClaimed",
                                // amount = u64 raw units — as a STRING (same reason as
                                // get_token_balances: JS doubles round above 2^53).
                                json!({"asset_id": asset_id, "amount": amount.to_string(), "statechain_id": statechain_id}),
                            ),
                            WalletEvent::BalanceUpdate { balance } => (
                                "BalanceUpdate",
                                serde_json::to_value(balance).unwrap_or(Value::Null),
                            ),
                            WalletEvent::ExitBranchConflict { statechain_id } => (
                                "ExitBranchConflict",
                                json!({"statechain_id": statechain_id}),
                            ),
                            WalletEvent::ExitDeadlineApproaching { statechain_id, deadline_block, tip } => (
                                "ExitDeadlineApproaching",
                                json!({"statechain_id": statechain_id, "deadline_block": deadline_block, "tip": tip}),
                            ),
                            WalletEvent::CoinRefreshed { old_statechain_id, new_statechain_id, fee_sats } => (
                                "CoinRefreshed",
                                json!({"old_statechain_id": old_statechain_id, "new_statechain_id": new_statechain_id, "fee_sats": fee_sats}),
                            ),
                            WalletEvent::TokenCarrierMaterialized { statechain_id, deadline_block, tip } => (
                                "TokenCarrierMaterialized",
                                json!({"statechain_id": statechain_id, "deadline_block": deadline_block, "tip": tip}),
                            ),
                            WalletEvent::LadderEstablished { statechain_id } => (
                                "LadderEstablished",
                                json!({"statechain_id": statechain_id}),
                            ),
                            WalletEvent::LadderDefended { statechain_id, tiers_broadcast } => (
                                "LadderDefended",
                                json!({"statechain_id": statechain_id, "tiers_broadcast": tiers_broadcast}),
                            ),
                            // The coin is FLAT-ONLY (no exit ladder). `reason` uses the stable
                            // wire spellings ("rgb-carrier", "funding-not-onchain", ...).
                            WalletEvent::LadderSkipped { statechain_id, reason } => (
                                "LadderSkipped",
                                json!({"statechain_id": statechain_id, "reason": reason.as_str()}),
                            ),
                            // 🔴 [F3] A deadline-critical pass is BLIND — it did not run, it did not
                            // "find nothing". `pass` uses the stable wire spellings ("auto-exit",
                            // "defend-ladders"). A host MUST alert on this: while it repeats, no
                            // clawback and no hostile trigger is being raced for this wallet. The
                            // same state is readable on demand via `watchtower_faults`.
                            WalletEvent::WatchtowerBlind { pass, detail } => (
                                "WatchtowerBlind",
                                json!({"pass": pass.as_str(), "detail": detail}),
                            ),
                            // [CTES-R] A coloured exit finished and the engine now sees the asset at
                            // the exit tip. Hosts that cache balances should re-read them here.
                            WalletEvent::ColoredExitTipRegistered { statechain_id, outpoint } => (
                                "ColoredExitTipRegistered",
                                json!({"statechain_id": statechain_id, "outpoint": outpoint}),
                            ),
                            // 🔴 The exit landed but the engine was NOT updated: every UTXO-driven
                            // rgb-lib view for this wallet is stale until the tip is registered.
                            WalletEvent::ColoredExitTipUnregistered { statechain_id, detail } => (
                                "ColoredExitTipUnregistered",
                                json!({"statechain_id": statechain_id, "detail": detail}),
                            ),
                        };
                        let line = json!({"event": name, "data": data}).to_string();
                        let mut out = tokio::io::stdout();
                        let _ = out.write_all(line.as_bytes()).await;
                        let _ = out.write_all(b"\n").await;
                        let _ = out.flush().await;
                    }
                });
                st.bg = Some(wallet.start_background());
            }
            json!(true)
        }
        "transfer" => serde_json::to_value(
            wallet
                .transfer(&p("receiver_address")?, pu64("amount_sats")?)
                .await?,
        )?,
        "split_coin" => {
            let (piece, change) = wallet
                .split_coin(&p("statechain_id")?, pu64("piece_sats")?)
                .await?;
            json!({"piece_statechain_id": piece, "change_statechain_id": change})
        }
        "transfer_tokens" => serde_json::to_value(
            wallet
                .transfer_tokens(&p("asset_id")?, &p("receiver_address")?, pu64("amount")?)
                .await?,
        )?,
        "get_token_funding_address" => json!(wallet.get_token_funding_address().await?),
        "issue_token" => json!(
            wallet
                .issue_token(
                    &p("ticker")?,
                    &p("name")?,
                    params.get("precision").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
                    pu64("supply")?,
                )
                .await?
        ),
        "start_lightning_swap" => {
            let coin = params
                .get("statechain_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let swap = wallet
                .start_lightning_swap(&p("counterparty_address")?, coin)
                .await?;
            json!({"batch_id": swap.batch_id, "payment_hash": swap.payment_hash, "statechain_id": swap.statechain_id})
        }
        "get_swap_payment_hash" => json!(wallet.get_swap_payment_hash(&p("batch_id")?).await?),
        "settle_lightning_swap" => {
            let swap = mercury_utexo_sdk::lightning::LightningSwap {
                batch_id: p("batch_id")?,
                payment_hash: params
                    .get("payment_hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                statechain_id: p("statechain_id")?,
            };
            json!(wallet.settle_lightning_swap(&swap).await?)
        }
        "withdraw" => {
            let coins = params.get("statechain_ids").and_then(|v| {
                v.as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
            });
            let fee_rate = params.get("fee_rate").and_then(|v| v.as_f64());
            serde_json::to_value(wallet.withdraw(&p("to_address")?, coins, fee_rate).await?)?
        }
        "unilateral_exit" => {
            let coins = params.get("statechain_ids").and_then(|v| {
                v.as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
            });
            serde_json::to_value(wallet.unilateral_exit(coins, None).await?)?
        }
        "get_activities" => serde_json::to_value(wallet.get_activities().await?)?,
        // [F3] Poll-side twin of the `WatchtowerBlind` event: which deadline-critical passes are
        // currently blind. `[]` means every pass that has run, ran with full visibility — a host
        // that shows "protected" must read this, not merely observe an absence of errors.
        "watchtower_faults" => Value::Array(
            wallet
                .watchtower_faults()
                .await
                .into_iter()
                .map(|f| {
                    json!({
                        "pass": f.pass.as_str(),
                        "detail": f.detail,
                        "consecutive_failures": f.consecutive_failures,
                        "since_unix": f.since_unix,
                        "last_unix": f.last_unix,
                    })
                })
                .collect(),
        ),
        // [F2] The TES-R defence also runs unconditionally in `start_background` (once per block);
        // this exposes an explicit extra pass for a host that wants to force one.
        "defend_ladders" => json!(wallet.defend_ladders().await?),
        other => return Err(anyhow!("unknown method {other}")),
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let state = Arc::new(Mutex::new(State { wallet: None, bg: None }));
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                emit(&json!({"id": null, "error": format!("bad request: {e}")})).await;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

        let response = match dispatch(&state, method, &params).await {
            Ok(result) => json!({"id": id, "result": result}),
            Err(e) => json!({"id": id, "error": e.to_string()}),
        };
        emit(&response).await;
    }
    Ok(())
}

async fn emit(v: &Value) {
    let mut out = tokio::io::stdout();
    let _ = out.write_all(v.to_string().as_bytes()).await;
    let _ = out.write_all(b"\n").await;
    let _ = out.flush().await;
}
