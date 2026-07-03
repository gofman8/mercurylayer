//! Lightning interop via the Mercury lightning latch — Spark's preimage-swap parity.
//!
//! Spark routes Lightning through SSPs with preimage-locked transfers. Mercury has the same
//! primitive natively: the **lightning latch**. A statechain coin is transferred under a
//! `batch_id` whose unlock is gated on an SE-held preimage; the counterparty (any LSP running a
//! Lightning node) pays/receives the corresponding Lightning payment against the payment hash and
//! the coin unlocks when the swap leg completes.
//!
//! Roles: the coin OWNER swaps a coin for a Lightning payment (the LSP plays Spark's SSP). The
//! SDK exposes the protocol legs; invoice creation/payment on the Lightning side is the LSP's
//! node's job (BOLT11 handling is deliberately out of the wallet's trust base).

use anyhow::{anyhow, Result};

use crate::wallet::SparkWallet;

/// An in-flight Lightning swap: the coin is latch-transferred; the Lightning payment for
/// `payment_hash` settles it.
#[derive(Clone, Debug)]
pub struct LightningSwap {
    pub batch_id: String,
    /// SHA256 payment hash the Lightning invoice must use.
    pub payment_hash: String,
    /// The coin handed to the counterparty (locked until the swap settles).
    pub statechain_id: String,
}

impl SparkWallet {
    /// Start a Lightning swap: latch-transfer the coin with `statechain_id` (or the largest
    /// confirmed coin) to `counterparty_address` (the LSP), locked on a fresh SE-held preimage.
    /// Hand the returned `payment_hash` to the counterparty — they pay your Lightning invoice
    /// bound to that hash. Settle with [`Self::settle_lightning_swap`] once the Lightning payment
    /// arrives; the coin then unlocks for the counterparty and the preimage is revealed to you.
    pub async fn start_lightning_swap(
        &self,
        counterparty_address: &str,
        statechain_id: Option<String>,
    ) -> Result<LightningSwap> {
        let _guard = self.inner.wallet_lock.lock().await;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;
        let coin_id = match statechain_id {
            Some(id) => id,
            None => record
                .coins
                .iter()
                .filter(|c| {
                    c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0
                })
                .max_by_key(|c| c.amount.unwrap_or_default())
                .and_then(|c| c.statechain_id.clone())
                .ok_or_else(|| anyhow!("no confirmed coin to swap"))?,
        };

        let pre = mercuryrustlib::lightning_latch::create_pre_image(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &coin_id,
        )
        .await?;

        mercuryrustlib::transfer_sender::execute(
            &self.inner.cc,
            counterparty_address,
            &self.inner.config.wallet_name,
            &coin_id,
            None,
            false,
            Some(pre.batch_id.clone()),
        )
        .await?;

        Ok(LightningSwap {
            batch_id: pre.batch_id,
            payment_hash: pre.hash,
            statechain_id: coin_id,
        })
    }

    /// Counterparty side: fetch the SE-registered payment hash for a swap's `batch_id` and check
    /// it matches what the initiator claims — then pay the Lightning invoice bound to it.
    pub async fn get_swap_payment_hash(&self, batch_id: &str) -> Result<Option<String>> {
        mercuryrustlib::lightning_latch::get_payment_hash(&self.inner.cc, batch_id).await
    }

    /// Initiator side: settle the swap after the Lightning payment for `payment_hash` arrived.
    /// Unlocks the coin for the counterparty and returns the preimage (hex) — reveal it to settle
    /// the Lightning HTLC if your invoice was a hodl invoice.
    pub async fn settle_lightning_swap(&self, swap: &LightningSwap) -> Result<String> {
        mercuryrustlib::lightning_latch::confirm_pending_invoice(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &swap.statechain_id,
        )
        .await?;
        let preimage = mercuryrustlib::lightning_latch::retrieve_pre_image(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &swap.statechain_id,
            &swap.batch_id,
        )
        .await?;
        Ok(preimage)
    }
}
