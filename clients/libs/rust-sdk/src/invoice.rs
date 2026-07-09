//! Utexo invoices: a self-describing payment request a payer can fulfill in one call. Encodes the
//! recipient's utexo address plus the requested amount, optional asset (sats when absent), memo,
//! and expiry. Mirrors Spark's `createSatsInvoice`/`createTokensInvoice`/`fulfillUtexoInvoice`.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::types::TransferResult;
use crate::wallet::UtexoWallet;

/// Scheme prefix for the encoded invoice string.
const SCHEME: &str = "utexoinv1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UtexoInvoice {
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

/// Encode an invoice as `utexoinv1<hex(json)>`.
pub fn encode_utexo_invoice(inv: &UtexoInvoice) -> Result<String> {
    let json = serde_json::to_vec(inv)?;
    Ok(format!("{SCHEME}{}", hex::encode(json)))
}

/// Decode a `utexoinv1…` invoice string.
pub fn decode_utexo_invoice(s: &str) -> Result<UtexoInvoice> {
    let body = s
        .strip_prefix(SCHEME)
        .ok_or_else(|| anyhow!("not a utexo invoice (missing {SCHEME} prefix)"))?;
    let bytes = hex::decode(body).map_err(|e| anyhow!("bad invoice hex: {e}"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

impl UtexoWallet {
    /// Create a sats payment request payable to this wallet. Spark's `createSatsInvoice`.
    pub async fn create_sats_invoice(
        &self,
        amount: u64,
        memo: Option<String>,
        expiry_unix: Option<u64>,
    ) -> Result<String> {
        let address = self.get_utexo_address().await?;
        encode_utexo_invoice(&UtexoInvoice {
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
        let address = self.get_utexo_address().await?;
        encode_utexo_invoice(&UtexoInvoice {
            version: 1,
            address,
            amount,
            asset_id: Some(asset_id.to_string()),
            memo,
            expiry_unix,
        })
    }

    /// Pay a Utexo invoice: decode it, check expiry, and transfer the requested sats or tokens to
    /// the embedded address. Spark's `fulfillUtexoInvoice`.
    pub async fn fulfill_utexo_invoice(&self, invoice: &str) -> Result<TransferResult> {
        let inv = decode_utexo_invoice(invoice)?;
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
        let inv = UtexoInvoice {
            version: 1,
            address: "tml1qexample".into(),
            amount: 25_000,
            asset_id: None,
            memo: Some("coffee".into()),
            expiry_unix: Some(1_900_000_000),
        };
        let enc = encode_utexo_invoice(&inv).unwrap();
        assert!(enc.starts_with("utexoinv1"));
        assert_eq!(decode_utexo_invoice(&enc).unwrap(), inv);
    }

    #[test]
    fn roundtrip_tokens() {
        let inv = UtexoInvoice {
            version: 1,
            address: "tml1qexample".into(),
            amount: 250,
            asset_id: Some("rgb:abc".into()),
            memo: None,
            expiry_unix: None,
        };
        let enc = encode_utexo_invoice(&inv).unwrap();
        assert_eq!(decode_utexo_invoice(&enc).unwrap(), inv);
    }

    #[test]
    fn rejects_non_invoice() {
        assert!(decode_utexo_invoice("lnbc1...").is_err());
        assert!(decode_utexo_invoice("utexoinv1zznothex").is_err());
    }
}
