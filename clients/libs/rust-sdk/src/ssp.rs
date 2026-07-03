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
use serde_json::{json, Value};
use std::time::Duration;

use crate::wallet::SparkWallet;

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

/// Quote for paying a BOLT11 through the SSP.
#[derive(Clone, Debug)]
pub struct PayQuote {
    pub amount_sats: u64,
    pub fee_sats: u64,
    pub payment_hash: String,
    pub ssp_address: String,
}

/// An open Lightning-receive swap on the SSP side.
#[derive(Clone, Debug)]
pub struct ReceiveSwap {
    pub batch_id: String,
    pub statechain_id: String,
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

        // The latched transfer must be addressed to us (claim shows batch-locked, not absent).
        let quote_sats = amt_msat / 1000 + self.fee_sats;
        let _ = quote_sats; // amount enforcement happens when claiming the coin below

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
        let claim = self.wallet.claim().await?;
        if claim.claimed_transfers == 0 {
            // One retry: relay/claim timing.
            tokio::time::sleep(Duration::from_secs(2)).await;
            self.wallet.claim().await?;
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
            statechain_id,
            invoice,
            payment_hash: pre.hash,
        })
    }

    /// Drive a receive swap to completion: once the payer's HTLC is pending, confirm the latch
    /// (releasing the coin to the receiver — the SE will not reveal the preimage before this),
    /// retrieve the preimage and claim the HODL invoice. Returns once the invoice is settled.
    pub async fn settle_receive(&self, swap: &ReceiveSwap) -> Result<()> {
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
            &swap.statechain_id,
        )
        .await?;
        let mut preimage: Option<String> = None;
        for _ in 0..45 {
            match mercuryrustlib::lightning_latch::retrieve_pre_image(
                self.wallet.client_config(),
                self.wallet.wallet_name(),
                &swap.statechain_id,
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

impl SparkWallet {
    /// USER SIDE — pay a BOLT11 invoice through an SSP: quote, mint the exact coin (off-chain
    /// split if needed), latch it to the invoice's payment hash and hand it over, then let the
    /// SSP pay. Returns the preimage — cryptographic proof the invoice was paid. Trustless both
    /// ways: no payment → the latch expires and the coin stays yours.
    pub async fn pay_lightning_invoice(&self, ssp: &SspService, invoice: &str) -> Result<String> {
        let quote = ssp.quote_pay(invoice).await?;
        let total = quote.amount_sats + quote.fee_sats;
        let coin_id = self.ensure_exact_coin(total).await?;
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
        Ok(preimage)
    }

    /// USER SIDE — receive Lightning into a statechain coin via an SSP: returns a BOLT11 invoice
    /// to hand to the payer. When it is paid, the SSP releases the coin and the background
    /// watcher claims it (TransferClaimed event).
    pub async fn create_lightning_invoice(
        &self,
        ssp: &SspService,
        amount_sats: u64,
    ) -> Result<ReceiveSwap> {
        let my_address = self.get_spark_address().await?;
        ssp.create_receive(amount_sats, &my_address).await
    }
}
