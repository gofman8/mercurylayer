//! Spark invoices: a self-describing payment request a payer can fulfill in one call. Encodes the
//! recipient's spark address plus the requested amount, optional asset (sats when absent), memo,
//! and expiry. Mirrors Spark's `createSatsInvoice`/`createTokensInvoice`/`fulfillSparkInvoice`.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::types::TransferResult;
use crate::wallet::SparkWallet;

/// Scheme prefix for the encoded invoice string.
const SCHEME: &str = "sparkinv1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SparkInvoice {
    pub version: u8,
    /// Recipient statechain address (ml1…/tml1…).
    pub address: String,
    /// Requested amount: sats when `asset_id` is None, else token units.
    pub amount: u64,
    /// None => a sats invoice; Some(contract id) => a token invoice.
    pub asset_id: Option<String>,
    pub memo: Option<String>,
    /// Unix seconds after which the invoice is expired.
    pub expiry_unix: Option<u64>,
}

/// Encode an invoice as `sparkinv1<hex(json)>`.
pub fn encode_spark_invoice(inv: &SparkInvoice) -> Result<String> {
    let json = serde_json::to_vec(inv)?;
    Ok(format!("{SCHEME}{}", hex::encode(json)))
}

/// Decode a `sparkinv1…` invoice string.
pub fn decode_spark_invoice(s: &str) -> Result<SparkInvoice> {
    let body = s
        .strip_prefix(SCHEME)
        .ok_or_else(|| anyhow!("not a spark invoice (missing {SCHEME} prefix)"))?;
    let bytes = hex::decode(body).map_err(|e| anyhow!("bad invoice hex: {e}"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

impl SparkWallet {
    /// Create a sats payment request payable to this wallet. Spark's `createSatsInvoice`.
    pub async fn create_sats_invoice(
        &self,
        amount: u64,
        memo: Option<String>,
        expiry_unix: Option<u64>,
    ) -> Result<String> {
        let address = self.get_spark_address().await?;
        encode_spark_invoice(&SparkInvoice {
            version: 1,
            address,
            amount,
            asset_id: None,
            memo,
            expiry_unix,
        })
    }

    /// Create a token payment request payable to this wallet. Spark's `createTokensInvoice`.
    pub async fn create_tokens_invoice(
        &self,
        asset_id: &str,
        amount: u64,
        memo: Option<String>,
        expiry_unix: Option<u64>,
    ) -> Result<String> {
        let address = self.get_spark_address().await?;
        encode_spark_invoice(&SparkInvoice {
            version: 1,
            address,
            amount,
            asset_id: Some(asset_id.to_string()),
            memo,
            expiry_unix,
        })
    }

    /// Pay a Spark invoice: decode it, check expiry, and transfer the requested sats or tokens to
    /// the embedded address. Spark's `fulfillSparkInvoice`.
    pub async fn fulfill_spark_invoice(&self, invoice: &str) -> Result<TransferResult> {
        let inv = decode_spark_invoice(invoice)?;
        if let Some(exp) = inv.expiry_unix {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now >= exp {
                return Err(anyhow!("invoice expired at {exp} (now {now})"));
            }
        }
        match inv.asset_id {
            Some(asset) => self.transfer_tokens(&asset, &inv.address, inv.amount).await,
            None => self.transfer(&inv.address, inv.amount).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_sats() {
        let inv = SparkInvoice {
            version: 1,
            address: "tml1qexample".into(),
            amount: 25_000,
            asset_id: None,
            memo: Some("coffee".into()),
            expiry_unix: Some(1_900_000_000),
        };
        let enc = encode_spark_invoice(&inv).unwrap();
        assert!(enc.starts_with("sparkinv1"));
        assert_eq!(decode_spark_invoice(&enc).unwrap(), inv);
    }

    #[test]
    fn roundtrip_tokens() {
        let inv = SparkInvoice {
            version: 1,
            address: "tml1qexample".into(),
            amount: 250,
            asset_id: Some("rgb:abc".into()),
            memo: None,
            expiry_unix: None,
        };
        let enc = encode_spark_invoice(&inv).unwrap();
        assert_eq!(decode_spark_invoice(&enc).unwrap(), inv);
    }

    #[test]
    fn rejects_non_invoice() {
        assert!(decode_spark_invoice("lnbc1...").is_err());
        assert!(decode_spark_invoice("sparkinv1zznothex").is_err());
    }
}
