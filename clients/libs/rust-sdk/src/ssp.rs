//! SSP (Spark Service Provider parity): the Mercury↔Lightning gateway.
//!
//! An SSP holds a statechain wallet and an RLN (rgb-lightning-node) Lightning node, and serves
//! two atomic swaps:
//!
//! - **Pay** (Mercury → Lightning): the user latch-transfers an exact coin to the SSP bound to
//!   the invoice's payment hash (SE latch v2, external hash). The SSP pays the BOLT11; LN
//!   settlement hands it the preimage, which is simultaneously (a) the user's proof of payment
//!   and (b) the SSP's key to unlock the coin at the SE (`/transfer/unlock/preimage`). Neither
//!   side can cheat: no payment → latch expires, the user keeps the coin; payment → the SSP can
//!   always claim.
//! - **Receive** (Lightning → Mercury): the SSP latch-transfers an exact coin to the user bound
//!   to an SE-held preimage, and issues a HODL invoice on that hash. When the payer's HTLC is
//!   pending, the SSP confirms the latch (releasing the coin) — only then can it retrieve the
//!   preimage and claim the HTLC. The SE's `locked=false` gating makes coin-release a
//!   precondition of taking the Lightning money.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

use crate::wallet::SparkWallet;

/// The user-facing SSP interface: the three swap operations a wallet performs against an SSP,
/// plus `info`. Implemented by the in-process [`SspService`] (embeds the SSP's own wallet + RLN
/// node) AND by the remote [`SspClient`] (HTTP to a deployed `mercury-ssp` server). Wallet swap
/// methods take `&impl Ssp`, so the SAME `wallet.pay_lightning_invoice(&ssp, ..)` call works
/// whether `ssp` is local (tests/embedded) or a remote service (production). Note: `settle_receive`
/// is deliberately NOT on this trait — it is an SSP-backend operation, never something a user does.
#[async_trait]
pub trait Ssp: Send + Sync {
    async fn info(&self) -> Result<SspInfo>;
    async fn quote_pay(&self, invoice: &str) -> Result<PayQuote>;
    /// Pay the invoice for the coin the user already latch-transferred under `batch_id`; returns
    /// the Lightning preimage (the user's proof of payment).
    async fn execute_pay(&self, invoice: &str, batch_id: &str) -> Result<String>;
    async fn create_receive(&self, amount_sats: u64, receiver_address: &str) -> Result<ReceiveSwap>;
}

/// SSP identity/fee, from `GET /info`.
#[derive(Clone, Debug)]
pub struct SspInfo {
    pub ssp_address: String,
    pub fee_sats: u64,
}

/// Minimal HTTP client for a running rgb-lightning-node daemon.
#[derive(Clone)]
pub struct RlnClient {
    pub api: String,
}

impl RlnClient {
    pub fn new(api_url: &str) -> Self {
        RlnClient { api: api_url.trim_end_matches('/').to_string() }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let resp = reqwest::Client::new()
            .post(format!("{}/{path}", self.api))
            .json(&body)
            .timeout(Duration::from_secs(120))
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(anyhow!("RLN /{path} -> {status}: {v}"));
        }
        Ok(v)
    }

    pub async fn decode_invoice(&self, invoice: &str) -> Result<(u64, String)> {
        let v = self.post("decodelninvoice", json!({ "invoice": invoice })).await?;
        let amt_msat = v.get("amt_msat").and_then(|x| x.as_u64()).unwrap_or(0);
        let hash = v
            .get("payment_hash")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("invoice without payment_hash"))?
            .to_string();
        Ok((amt_msat, hash))
    }

    pub async fn ln_invoice(&self, amt_msat: u64, payment_hash: Option<&str>, expiry_sec: u64) -> Result<String> {
        let mut body = json!({ "amt_msat": amt_msat, "expiry_sec": expiry_sec });
        if let Some(h) = payment_hash {
            body["payment_hash"] = json!(h);
        }
        Ok(self
            .post("lninvoice", body)
            .await?
            .get("invoice")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("no invoice in response"))?
            .to_string())
    }

    pub async fn send_payment(&self, invoice: &str) -> Result<String> {
        Ok(self
            .post("sendpayment", json!({ "invoice": invoice }))
            .await?
            .get("payment_hash")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("no payment_hash in sendpayment response"))?
            .to_string())
    }

    /// (status, preimage) of an outbound payment by hash.
    pub async fn payment(&self, payment_hash: &str) -> Result<(String, Option<String>)> {
        let v = self
            .post(
                "getpayment",
                json!({ "payment_hash": payment_hash, "payment_type": "Outbound" }),
            )
            .await?;
        let p = v.get("payment").unwrap_or(&v);
        Ok((
            p.get("status").and_then(|x| x.as_str()).unwrap_or("Unknown").to_string(),
            p.get("preimage").and_then(|x| x.as_str()).map(|s| s.to_string()),
        ))
    }

    pub async fn invoice_status(&self, invoice: &str) -> Result<String> {
        Ok(self
            .post("invoicestatus", json!({ "invoice": invoice }))
            .await?
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("Unknown")
            .to_string())
    }

    pub async fn claim_hodl(&self, payment_hash: &str, preimage: &str) -> Result<()> {
        self.post(
            "claimhodlinvoice",
            json!({ "payment_hash": payment_hash, "payment_preimage": preimage }),
        )
        .await?;
        Ok(())
    }
}

/// Pure pre-payment gate (review C2/C3): before the SSP pays a Lightning invoice, every coin
/// latched to the swap must be a pending transfer addressed to the SSP (present in `pending`, which
/// is built ONLY from transfers the SSP can decrypt with its own key) and their total value must
/// cover `quote_sats` (invoice + fee). Returns the total on success. Extracted for unit testing.
fn check_latched_coins(
    latched_ids: &[String],
    pending: &[(String, u64)],
    quote_sats: u64,
) -> Result<u64> {
    if latched_ids.is_empty() {
        return Err(anyhow!("no coin latched to this swap — refusing to pay"));
    }
    let mut total: u64 = 0;
    for sid in latched_ids {
        let amt = pending
            .iter()
            .find(|(s, _)| s == sid)
            .map(|(_, a)| *a)
            .ok_or_else(|| anyhow!("latched coin {sid} is not a pending transfer addressed to the SSP — refusing to pay"))?;
        total = total.saturating_add(amt);
    }
    if total < quote_sats {
        return Err(anyhow!(
            "latched coin value {total} sats is below the required {quote_sats} (invoice + fee) — refusing to pay"
        ));
    }
    Ok(total)
}

/// Quote for paying a BOLT11 through the SSP.
#[derive(Clone, Debug)]
pub struct PayQuote {
    pub amount_sats: u64,
    pub fee_sats: u64,
    pub payment_hash: String,
    pub ssp_address: String,
}

/// An open Lightning-receive swap. `statechain_id` is the SSP-side coin being handed over: it is
/// `Some` when produced locally by [`SspService::create_receive`] (the SSP drives settlement), and
/// `None` when returned by the remote [`SspClient`] (a user only receives the invoice; the SSP
/// server settles). `settle_receive` (SSP-side) requires it to be `Some`.
#[derive(Clone, Debug)]
pub struct ReceiveSwap {
    pub batch_id: String,
    pub statechain_id: Option<String>,
    pub invoice: String,
    pub payment_hash: String,
}

/// The SSP service: a statechain wallet + an RLN node. Instantiable in-process (tests, embedded
/// LSPs) or wrapped by the `mercury-ssp` HTTP binary.
pub struct SspService {
    pub wallet: SparkWallet,
    pub rln: RlnClient,
    /// Flat service fee added on pay quotes (sats).
    pub fee_sats: u64,
}

impl SspService {
    pub fn new(wallet: SparkWallet, rln: RlnClient, fee_sats: u64) -> Self {
        SspService { wallet, rln, fee_sats }
    }

    /// Quote paying `invoice`: how many sats the user must latch over, and to which address.
    pub async fn quote_pay(&self, invoice: &str) -> Result<PayQuote> {
        let (amt_msat, payment_hash) = self.rln.decode_invoice(invoice).await?;
        if amt_msat == 0 {
            return Err(anyhow!("zero-amount invoices not supported"));
        }
        Ok(PayQuote {
            amount_sats: amt_msat / 1000,
            fee_sats: self.fee_sats,
            payment_hash,
            ssp_address: self.wallet.get_spark_address().await?,
        })
    }

    /// Execute a pay swap after the user latch-transferred the coin under `batch_id` bound to
    /// the invoice's payment hash. Pays the invoice, unlocks the coin with the LN preimage,
    /// claims it, and returns the preimage (the user's proof of payment).
    pub async fn execute_pay(&self, invoice: &str, batch_id: &str) -> Result<String> {
        let (amt_msat, invoice_hash) = self.rln.decode_invoice(invoice).await?;

        // The latch must be bound to this exact invoice.
        let latched_hash = mercuryrustlib::lightning_latch::get_payment_hash(
            self.wallet.client_config(),
            batch_id,
        )
        .await?
        .ok_or_else(|| anyhow!("no latch registered for batch {batch_id}"))?;
        if latched_hash != invoice_hash {
            return Err(anyhow!("latch hash does not match the invoice payment hash"));
        }

        // PRE-PAYMENT GATE (review C2/C3): identify which coin(s) this batch will hand us and
        // validate them BEFORE paying real Lightning money. Each latched coin must be (a) a pending
        // transfer genuinely addressed to the SSP — proven because `peek_pending_transfers` only
        // returns transfers this wallet can DECRYPT with its own auth key — and (b) collectively
        // worth at least the invoice + fee. Without (a) we would pay for a coin sent to someone
        // else; without (b) for an undersized coin. Both are unbounded fund-loss otherwise.
        let quote_sats = amt_msat / 1000 + self.fee_sats;
        let latched_ids = mercuryrustlib::lightning_latch::get_statechain_ids_by_batch_id(
            self.wallet.client_config(),
            batch_id,
        )
        .await?;
        if latched_ids.is_empty() {
            return Err(anyhow!("no coin latched under batch {batch_id} — refusing to pay"));
        }
        let pending = mercuryrustlib::transfer_receiver::peek_pending_transfers(
            self.wallet.client_config(),
            self.wallet.wallet_name(),
        )
        .await?;
        let pending_pairs: Vec<(String, u64)> =
            pending.into_iter().map(|p| (p.statechain_id, p.amount)).collect();
        check_latched_coins(&latched_ids, &pending_pairs, quote_sats)?;

        // Pay the invoice over Lightning.
        self.rln.send_payment(invoice).await?;
        let mut preimage: Option<String> = None;
        for _ in 0..60 {
            let (status, p) = self.rln.payment(&invoice_hash).await?;
            match status.as_str() {
                "Succeeded" => {
                    preimage = p;
                    break;
                }
                "Failed" => return Err(anyhow!("lightning payment failed")),
                _ => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
        let preimage = preimage.ok_or_else(|| anyhow!("payment did not settle in time"))?;

        // The preimage unlocks the latched coin at the SE; claim it.
        mercuryrustlib::lightning_latch::unlock_by_preimage(
            self.wallet.client_config(),
            batch_id,
            &preimage,
        )
        .await?;
        let mut claimed = self.wallet.claim().await?.claimed_transfers;
        if claimed == 0 {
            // One retry: relay/claim timing.
            tokio::time::sleep(Duration::from_secs(2)).await;
            claimed = self.wallet.claim().await?.claimed_transfers;
        }
        if claimed == 0 {
            // We paid over Lightning but could not take custody of the latched coin. Do NOT report
            // success (review M3): surface it so the operator can investigate the latch/preimage
            // rather than silently eating the loss.
            return Err(anyhow!(
                "paid the Lightning invoice for batch {batch_id} but claimed 0 transfers — the latched coin was not received; investigate before retrying"
            ));
        }
        Ok(preimage)
    }

    /// Open a receive swap: latch an exact coin over to `receiver_address` and issue a HODL
    /// invoice on the latch's SE-held payment hash. The payer pays the invoice; call
    /// [`Self::settle_receive`] to drive settlement.
    pub async fn create_receive(&self, amount_sats: u64, receiver_address: &str) -> Result<ReceiveSwap> {
        let statechain_id = self.wallet.ensure_exact_coin(amount_sats).await?;
        let pre = mercuryrustlib::lightning_latch::create_pre_image(
            self.wallet.client_config(),
            self.wallet.wallet_name(),
            &statechain_id,
        )
        .await?;
        mercuryrustlib::transfer_sender::execute(
            self.wallet.client_config(),
            receiver_address,
            self.wallet.wallet_name(),
            &statechain_id,
            None,
            false,
            Some(pre.batch_id.clone()),
        )
        .await?;
        let invoice = self
            .rln
            .ln_invoice(amount_sats * 1000, Some(&pre.hash), 3600)
            .await?;
        Ok(ReceiveSwap {
            batch_id: pre.batch_id,
            statechain_id: Some(statechain_id),
            invoice,
            payment_hash: pre.hash,
        })
    }

    /// Drive a receive swap to completion: once the payer's HTLC is pending, confirm the latch
    /// (releasing the coin to the receiver — the SE will not reveal the preimage before this),
    /// retrieve the preimage and claim the HODL invoice. Returns once the invoice is settled.
    pub async fn settle_receive(&self, swap: &ReceiveSwap) -> Result<()> {
        // settle_receive is SSP-side: the swap must carry the coin it minted. A remote-client swap
        // (statechain_id = None) is settled by the SSP server that created it, never here.
        let statechain_id = swap
            .statechain_id
            .as_deref()
            .ok_or_else(|| anyhow!("settle_receive requires a local (SSP-side) swap with a statechain_id"))?;
        // Wait for the HTLC.
        let mut pending = false;
        for _ in 0..90 {
            let st = self.rln.invoice_status(&swap.invoice).await?;
            if st == "Pending" || st == "Succeeded" {
                pending = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        if !pending {
            return Err(anyhow!("no HTLC arrived for the receive swap"));
        }

        // Release the coin, then (and only then) the SE reveals the preimage. The latch flips
        // once BOTH sides have unlocked — the receiver's side happens on their (background)
        // claim attempt — so retry the retrieve until the SE releases it.
        mercuryrustlib::lightning_latch::confirm_pending_invoice(
            self.wallet.client_config(),
            self.wallet.wallet_name(),
            statechain_id,
        )
        .await?;
        let mut preimage: Option<String> = None;
        for _ in 0..45 {
            match mercuryrustlib::lightning_latch::retrieve_pre_image(
                self.wallet.client_config(),
                self.wallet.wallet_name(),
                statechain_id,
                &swap.batch_id,
            )
            .await
            {
                Ok(p) => {
                    preimage = Some(p);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
        let preimage =
            preimage.ok_or_else(|| anyhow!("SE did not release the preimage (receiver has not claimed?)"))?;
        self.rln.claim_hodl(&swap.payment_hash, &preimage).await?;
        for _ in 0..30 {
            if self.rln.invoice_status(&swap.invoice).await? == "Succeeded" {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(anyhow!("hodl invoice did not settle"))
    }
}

/// The local SSP satisfies [`Ssp`] by forwarding to its inherent methods (fully-qualified to avoid
/// the trait/inherent name clash). Behaviour is byte-for-byte the inherent methods.
#[async_trait]
impl Ssp for SspService {
    async fn info(&self) -> Result<SspInfo> {
        Ok(SspInfo {
            ssp_address: self.wallet.get_spark_address().await?,
            fee_sats: self.fee_sats,
        })
    }
    async fn quote_pay(&self, invoice: &str) -> Result<PayQuote> {
        SspService::quote_pay(self, invoice).await
    }
    async fn execute_pay(&self, invoice: &str, batch_id: &str) -> Result<String> {
        SspService::execute_pay(self, invoice, batch_id).await
    }
    async fn create_receive(&self, amount_sats: u64, receiver_address: &str) -> Result<ReceiveSwap> {
        SspService::create_receive(self, amount_sats, receiver_address).await
    }
}

/// Remote SSP over HTTP — the client half of the `mercury-ssp` server. A user with only their own
/// wallet swaps against a DEPLOYED SSP: `SspClient::new(url)` then the same
/// `wallet.pay_lightning_invoice(&client, invoice)` / `wallet.create_lightning_invoice(&client, n)`
/// calls as the in-process path. Mirrors the server's `/info /quote /pay /receive` one-to-one.
#[derive(Clone)]
pub struct SspClient {
    pub base: String,
    http: reqwest::Client,
}

impl SspClient {
    pub fn new(base_url: &str) -> Self {
        SspClient {
            base: base_url.trim_end_matches('/').to_string(),
            // /pay blocks on the SSP's LN-settlement loop (~120s of polls) PLUS unlock+claim+SE
            // round-trips, so a SUCCESSFUL pay can approach ~150s+. The client timeout MUST exceed
            // the server's true worst case, or a genuine success surfaces as a timeout that is
            // indistinguishable from failure and could trigger an unsafe reclaim (review H1). 240s.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(240))
                .build()
                .unwrap(),
        }
    }

    /// POST returning a JSON OBJECT. Robust error detection (review [1]/[13]): a failure may arrive
    /// as (a) the server's own HTTP-200 `{"error": ".."}`, (b) a Rocket catcher (4xx/5xx) whose body
    /// is `{"error": {..object..}}` or HTML, or (c) a non-JSON/empty body. We treat any non-2xx
    /// status, any `error` field, or a body that is not a JSON object as an error — never letting a
    /// failure deserialize into a bogus all-zero PayQuote / empty preimage.
    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(self.http.post(format!("{}/{path}", self.base)).json(&body), path)
            .await
    }

    async fn request(&self, rb: reqwest::RequestBuilder, path: &str) -> Result<Value> {
        let resp = rb.header("Accept", "application/json").send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            let detail = v
                .get("error")
                .map(|e| e.to_string())
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(anyhow!("ssp /{path} -> {status}: {detail}"));
        }
        if let Some(e) = v.get("error") {
            return Err(anyhow!("ssp /{path}: {}", e));
        }
        if !v.is_object() {
            return Err(anyhow!("ssp /{path}: non-object response body"));
        }
        Ok(v)
    }
}

#[async_trait]
impl Ssp for SspClient {
    async fn info(&self) -> Result<SspInfo> {
        let v = self.request(self.http.get(format!("{}/info", self.base)), "info").await?;
        let addr = v["spark_address"].as_str().unwrap_or_default();
        if addr.is_empty() {
            return Err(anyhow!("ssp /info: no spark_address (server not ready?)"));
        }
        Ok(SspInfo {
            ssp_address: addr.to_string(),
            fee_sats: v["fee_sats"].as_u64().unwrap_or(0),
        })
    }
    async fn quote_pay(&self, invoice: &str) -> Result<PayQuote> {
        let v = self.post("quote", json!({ "invoice": invoice })).await?;
        Ok(PayQuote {
            amount_sats: v["amount_sats"].as_u64().unwrap_or(0),
            fee_sats: v["fee_sats"].as_u64().unwrap_or(0),
            payment_hash: v["payment_hash"].as_str().unwrap_or_default().to_string(),
            ssp_address: v["ssp_address"].as_str().unwrap_or_default().to_string(),
        })
    }
    async fn execute_pay(&self, invoice: &str, batch_id: &str) -> Result<String> {
        let v = self
            .post("pay", json!({ "invoice": invoice, "batch_id": batch_id }))
            .await?;
        v["preimage"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("ssp /pay: no preimage in response"))
    }
    async fn create_receive(&self, amount_sats: u64, receiver_address: &str) -> Result<ReceiveSwap> {
        let v = self
            .post(
                "receive",
                json!({ "amount_sats": amount_sats, "receiver_address": receiver_address }),
            )
            .await?;
        Ok(ReceiveSwap {
            batch_id: v["batch_id"].as_str().unwrap_or_default().to_string(),
            statechain_id: None, // the SSP server owns + settles the coin
            invoice: v["invoice"].as_str().unwrap_or_default().to_string(),
            payment_hash: v["payment_hash"].as_str().unwrap_or_default().to_string(),
        })
    }
}

impl SparkWallet {
    /// USER SIDE — pay a BOLT11 invoice through an SSP: quote, mint the exact coin (off-chain
    /// split if needed), latch it to the invoice's payment hash and hand it over, then let the
    /// SSP pay. Returns the preimage — cryptographic proof the invoice was paid. Trustless both
    /// ways: no payment → the latch expires and the coin stays yours. `ssp` may be a local
    /// [`SspService`] or a remote [`SspClient`].
    pub async fn pay_lightning_invoice(&self, ssp: &impl Ssp, invoice: &str) -> Result<String> {
        self.pay_lightning_invoice_reclaimable(ssp, invoice)
            .await
            .map_err(|(_coin_id, e)| e)
    }

    /// Like [`Self::pay_lightning_invoice`], but on failure returns the latched coin's statechain id
    /// alongside the error so the caller can [`Self::reclaim_lightning_payment`] it once the SE
    /// `batch_timeout` elapses. `Ok` is the preimage; `Err` is `(coin_statechain_id, error)`. The
    /// coin id is `None`-encoded as an empty string only if the failure happened before the coin was
    /// even minted (quote/mint stage) — in that case nothing was latched, so nothing to reclaim.
    pub async fn pay_lightning_invoice_reclaimable(
        &self,
        ssp: &impl Ssp,
        invoice: &str,
    ) -> std::result::Result<String, (String, anyhow::Error)> {
        let pre = async {
            let quote = ssp.quote_pay(invoice).await?;
            let total = quote.amount_sats + quote.fee_sats;
            let coin_id = self.ensure_exact_coin(total).await?;
            Ok::<_, anyhow::Error>((quote, coin_id))
        }
        .await;
        let (quote, coin_id) = match pre {
            Ok(v) => v,
            Err(e) => return Err((String::new(), e)), // nothing latched yet
        };

        // From here the coin exists and gets latched: any failure is reclaimable via `coin_id`.
        let latch_and_pay = async {
            let batch_id = mercuryrustlib::lightning_latch::create_external_hash_latch(
                self.client_config(),
                self.wallet_name(),
                &coin_id,
                &quote.payment_hash,
            )
            .await?;
            mercuryrustlib::transfer_sender::execute(
                self.client_config(),
                &quote.ssp_address,
                self.wallet_name(),
                &coin_id,
                None,
                false,
                Some(batch_id.clone()),
            )
            .await?;

            let preimage = ssp.execute_pay(invoice, &batch_id).await?;

            // Verify the proof of payment.
            use sha2::{Digest, Sha256};
            let digest = hex::encode(Sha256::digest(hex::decode(&preimage)?));
            if digest != quote.payment_hash {
                return Err(anyhow!("SSP returned an invalid preimage"));
            }
            Ok::<_, anyhow::Error>(preimage)
        }
        .await;

        latch_and_pay.map_err(|e| (coin_id, e))
    }

    /// USER SIDE — receive Lightning into a statechain coin via an SSP: returns a BOLT11 invoice
    /// to hand to the payer. When it is paid, the SSP releases the coin and the background
    /// watcher claims it (TransferClaimed event). `ssp` may be local or remote.
    pub async fn create_lightning_invoice(
        &self,
        ssp: &impl Ssp,
        amount_sats: u64,
    ) -> Result<ReceiveSwap> {
        let my_address = self.get_spark_address().await?;
        ssp.create_receive(amount_sats, &my_address).await
    }

    /// USER SIDE — reclaim a coin whose pay swap NEVER settled. When [`Self::pay_lightning_invoice`]
    /// (or `execute_pay`) returns an error because the SSP could not pay, the coin was latch-locked
    /// to the SSP but never unlocked — the key handover never completed, so it is still yours. Once
    /// the SE `batch_timeout` window has elapsed the latch is no longer held; re-transfer the coin
    /// to yourself (a fresh, un-batched transfer) to clear the stale batch-locked transfer and take
    /// full custody again. Returns the new statechain id of the reclaimed coin.
    ///
    /// SAFETY — read before automating (review H1/[2]): reclaim is only safe when the SSP will
    /// NOT settle. Two error cases are NOT proof of that and must NOT auto-trigger a reclaim:
    ///   1. A client-side timeout talking to a remote [`SspClient`] — the SSP may have paid and be
    ///      about to claim. (The 240s client timeout is sized above the server's worst case to make
    ///      this rare, but a re-queryable SSP `/pay` status endpoint is the real fix — TODO.)
    ///   2. An `execute_pay` error AFTER the SSP revealed the preimage (paid, then its claim step
    ///      hiccuped) — the SSP's background watcher will still claim, so reclaiming here double-
    ///      spends the SSP.
    /// The SE's batch lock only protects you until `batch_timeout`; after that this call succeeds
    /// even if the SSP paid. So only reclaim once you have POSITIVELY confirmed non-payment (e.g. no
    /// preimage exists for the invoice hash). This method itself only enforces the SE's batch-lock
    /// (it fails before `batch_timeout`); the non-payment confirmation is the caller's obligation.
    pub async fn reclaim_lightning_payment(&self, coin_statechain_id: &str) -> Result<()> {
        let me = self.get_spark_address().await?;
        // Fresh, un-batched transfer of the specific coin back to ourselves: the SE drops the stale
        // batch-locked transfer row and the coin's key rotates back under our control.
        mercuryrustlib::transfer_sender::execute(
            self.client_config(),
            &me,
            self.wallet_name(),
            coin_statechain_id,
            None,
            false,
            None,
        )
        .await?;
        let _ = self.claim().await; // pull the reclaimed coin back in
        Ok(())
    }
}

#[cfg(test)]
mod swap_tests {
    use sha2::{Digest, Sha256};

    // INV-14: a Lightning preimage proves payment iff sha256(preimage) == the invoice hash. The
    // pay flow asserts exactly this before accepting the SSP's returned preimage.
    #[test]
    fn preimage_matches_hash() {
        let preimage = [0x11u8; 32];
        let hash = hex::encode(Sha256::digest(preimage));
        // correct preimage validates
        assert_eq!(hex::encode(Sha256::digest(hex::decode(hex::encode(preimage)).unwrap())), hash);
        // a different preimage does not
        let wrong = [0x22u8; 32];
        assert_ne!(hex::encode(Sha256::digest(wrong)), hash);
    }

    // review C2/C3: the SSP pre-payment gate. Uses the module-private helper directly.
    use super::check_latched_coins;

    fn pend(pairs: &[(&str, u64)]) -> Vec<(String, u64)> {
        pairs.iter().map(|(s, a)| (s.to_string(), *a)).collect()
    }

    #[test]
    fn gate_accepts_addressed_and_sufficient() {
        // coin addressed to us, worth exactly invoice+fee
        let ids = vec!["sc1".to_string()];
        let pending = pend(&[("sc1", 25_000), ("other", 9)]);
        assert_eq!(check_latched_coins(&ids, &pending, 25_000).unwrap(), 25_000);
    }

    #[test]
    fn gate_rejects_coin_not_addressed_to_ssp() {
        // C2: the latched id is NOT among our decryptable pending transfers.
        let ids = vec!["sc_attacker".to_string()];
        let pending = pend(&[("sc1", 100_000)]);
        assert!(check_latched_coins(&ids, &pending, 25_000).is_err());
    }

    #[test]
    fn gate_rejects_undersized_coin() {
        // C3: addressed to us, but below invoice+fee.
        let ids = vec!["sc1".to_string()];
        let pending = pend(&[("sc1", 24_999)]);
        assert!(check_latched_coins(&ids, &pending, 25_000).is_err());
    }

    #[test]
    fn gate_rejects_empty_latch() {
        assert!(check_latched_coins(&[], &pend(&[("sc1", 1_000)]), 1).is_err());
    }

    #[test]
    fn gate_sums_multiple_latched_coins() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let pending = pend(&[("a", 10_000), ("b", 16_000)]);
        assert_eq!(check_latched_coins(&ids, &pending, 25_000).unwrap(), 26_000);
        // but if one of them isn't ours, reject wholesale
        let ids2 = vec!["a".to_string(), "c".to_string()];
        assert!(check_latched_coins(&ids2, &pending, 1).is_err());
    }
}
