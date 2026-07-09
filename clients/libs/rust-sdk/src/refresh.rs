//! Coin **refresh** (re-anchor): reset a coin's backup ladder and root deadline by spending it
//! on-chain into a FRESH aggregate. Two fee models — the user pays, or an operator sponsor pays.
//!
//! A statechain coin's decrementing-`nLockTime` ladder is a finite budget: `initlock` blocks of
//! headroom, spent by BOTH hops (`interval` per transfer) and wall-clock time (~144 blocks/day) —
//! see [INVALIDATION-SPEC.md](../../../docs/spark/INVALIDATION-SPEC.md). When it nears the floor the
//! coin becomes un-transferable (the receiver rejects a backup whose locktime is at/below the tip,
//! `MercuryError::LocktimeTooLow`) and must be moved to L1. There is deliberately no off-chain
//! renewal (a split-to-self resets a *leaf* ladder but not the tree's root deadline). `refresh` is
//! the on-chain reset: ONE SE-co-signed transaction spends the coin's current 2-of-2 outpoint into
//! a fresh deposit aggregate (a new `statechain_id`, same owner). Because the old outpoint is now
//! spent, **every previous owner's backup is permanently invalidated** (they double-spend a spent
//! input); the new coin gets a fresh deposit height ⇒ a fresh full ladder AND a fresh root
//! deadline. It works for a flat coin or a sub-coin (the exit branch is materialized first). Note
//! refresh is COOPERATIVE — it needs the SE (a fresh co-signed spend); if the SE is gone you exit
//! unilaterally instead.
//!
//! ## Two fee models
//!
//! The on-chain fee physically comes from the coin — the re-anchor tx is single-input (the SE is a
//! blind co-signer with no on-chain wallet and cannot co-fund it). The two "ways" differ in whether
//! the user is reimbursed:
//!
//! - [`SparkWallet::refresh`] — **user pays**. The refreshed coin is `amount − fee`.
//! - [`SparkWallet::refresh_sponsored`] — **operator pays**. The re-anchor runs as above, then a
//!   funded operator sponsor reimburses the fee OFF-CHAIN (an instant, free statechain transfer), so
//!   the user's TOTAL balance is preserved. The user ends with two coins: the refreshed
//!   `amount − fee` coin plus a `fee` rebate coin. The sponsor is any funded wallet/service (in a
//!   deployment, an SSP-like operator); the rebate half is also exposed as
//!   [`SparkWallet::rebate_refresh_fee`] so a remote sponsor can drive it.

use anyhow::{anyhow, Result};
use mercurylib::wallet::CoinStatus;

use crate::transfer::{backup_fee_rate, BACKUP_TX_VBYTES};
use crate::types::TransferResult;
use crate::wallet::SparkWallet;

/// Outcome of a refresh: the old coin is spent on-chain (its backups are now dead) and a new coin
/// with a fresh ladder is (or is becoming) confirmed at a fresh aggregate.
#[derive(Clone, Debug)]
pub struct RefreshResult {
    /// The statechain id that was refreshed (now `WITHDRAWING`/`WITHDRAWN` — its outpoint is spent).
    pub old_statechain_id: String,
    /// The fresh coin's statechain id. Confirms via the watcher/`claim()`, like any deposit.
    pub new_statechain_id: String,
    pub old_amount_sats: u64,
    /// The re-anchored coin's amount: `old_amount_sats − fee_sats`.
    pub new_amount_sats: u64,
    pub fee_sats: u64,
    /// The re-anchor tx (also the fresh deposit's funding tx).
    pub refresh_txid: String,
    /// Sats an operator sponsor reimbursed off-chain (`0` for the user-pays [`SparkWallet::refresh`];
    /// `fee_sats + DUST_LIMIT` after [`SparkWallet::refresh_sponsored`] — the smallest off-chain-payable
    /// amount covering the sub-dust fee, so the user ends ≥ whole).
    pub rebate_sats: u64,
}

impl SparkWallet {
    /// **User-pays** refresh: re-anchor `statechain_id` on-chain into a fresh aggregate, resetting
    /// its ladder and root deadline. The fee is taken from the coin (refreshed coin = `amount −
    /// fee`). One on-chain transaction; the previous outpoint is spent, so all old backups are
    /// invalidated. The new coin confirms asynchronously (watcher/`claim()`), like a deposit.
    ///
    /// `fee_rate` (sat/vB) is capped at the client's `max_fee_rate`; `None` uses the SE-quoted rate.
    /// Errors if the coin is not `CONFIRMED`, carries an RGB allocation, or is too small to cover
    /// the fee above the dust floor.
    pub async fn refresh(&self, statechain_id: &str, fee_rate: Option<f64>) -> Result<RefreshResult> {
        self.reanchor(statechain_id, fee_rate).await
    }

    /// **Operator-pays** refresh: the same on-chain re-anchor as [`Self::refresh`] (the fee is
    /// physically drawn from the coin — the tx is single-input and the SE holds no funds), followed
    /// by an OFF-CHAIN rebate from `sponsor` to this wallet so the user is made (at least) whole.
    ///
    /// The refresh fee (~112 sats at 1 sat/vB) is BELOW the dust floor, and an off-chain transfer
    /// can't mint a piece below `min_split_output` (dust + the piece's own backup fee). So the
    /// smallest amount the operator can send off-chain that covers the fee is
    /// `fee_sats + DUST_LIMIT`; the sponsor rebates that, over-compensating the user by the dust
    /// floor (330 sats). `rebate_sats` reports the actual amount rebated (≥ `fee_sats`), so the
    /// user's total balance ends ≥ the original. The rebate arrives as a normal incoming transfer;
    /// claim it via the watcher/`claim()`. `sponsor` is a funded operator wallet.
    pub async fn refresh_sponsored(
        &self,
        statechain_id: &str,
        sponsor: &SparkWallet,
        fee_rate: Option<f64>,
    ) -> Result<RefreshResult> {
        let mut res = self.reanchor(statechain_id, fee_rate).await?;
        // The rebate must be off-chain-payable: at least the fee, but no smaller than a mintable
        // piece. Since min_split_output(rate) == DUST_LIMIT + fee for this 112-vB tx, the tightest
        // payable rebate is fee + DUST_LIMIT (the operator absorbs the dust-floor rounding).
        let rebate = res.fee_sats + crate::transfer::DUST_LIMIT;
        // Reimburse off-chain to this wallet's stable receive address. The re-anchor above already
        // released the wallet lock, so this (and the sponsor's own transfer) do not deadlock.
        let user_addr = self.get_spark_address().await?;
        sponsor
            .rebate_refresh_fee(&user_addr, rebate)
            .await
            .map_err(|e| anyhow!("re-anchor succeeded but the sponsor rebate failed: {e}"))?;
        res.rebate_sats = rebate;
        Ok(res)
    }

    /// Operator side of a sponsored refresh: send `fee_sats` off-chain to `to_spark_address` to
    /// reimburse a user's refresh fee. A thin, discoverable wrapper over [`Self::transfer`] — a
    /// remote sponsor service calls this after (or when notified that) the user re-anchored.
    pub async fn rebate_refresh_fee(
        &self,
        to_spark_address: &str,
        fee_sats: u64,
    ) -> Result<TransferResult> {
        self.transfer(to_spark_address, fee_sats).await
    }

    /// The shared re-anchor primitive (fee physically from the coin). See [`Self::refresh`].
    async fn reanchor(&self, statechain_id: &str, fee_rate: Option<f64>) -> Result<RefreshResult> {
        let (fresh_addr, new_statechain_id, amount, amount_out, fee_sats, rate) = {
            // Serialize against the background watcher; none of the calls below re-take this lock.
            let _guard = self.inner.wallet_lock.lock().await;

            // 1. Locate the coin; only a CONFIRMED, non-carrier coin can be re-anchored.
            let record = self.record().await?;
            let coin = record
                .coins
                .iter()
                .find(|c| {
                    c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0
                })
                .ok_or_else(|| anyhow!("no coin with statechain id {statechain_id}"))?;
            if coin.status != CoinStatus::CONFIRMED {
                return Err(anyhow!(
                    "coin {statechain_id} is {:?}, not CONFIRMED — only a confirmed coin can be refreshed",
                    coin.status
                ));
            }
            let amount = coin.amount.unwrap_or_default() as u64;
            let carriers = self.token_carrier_outpoints().await?;
            if crate::wallet::coin_outpoint(coin).map_or(false, |o| carriers.contains(&o)) {
                return Err(anyhow!(
                    "coin {statechain_id} carries an RGB allocation; refreshing it as a plain re-anchor would destroy the tokens — move the asset off this coin first"
                ));
            }

            // 2. Deterministic fee: the re-anchor tx is 1-in-1-out P2TR (BACKUP_TX_VBYTES = 112 vB).
            //    Compute the fee with the SAME rate passed to withdraw so the on-chain output value
            //    equals what the fresh deposit expects (check_deposit matches on exact value).
            let rate = match fee_rate {
                Some(r) => r.min(self.inner.cc.max_fee_rate),
                None => backup_fee_rate(&self.inner.cc).await?,
            };
            let fee_sats = (BACKUP_TX_VBYTES as f64 * rate).ceil() as u64;
            let amount_out = amount.checked_sub(fee_sats).ok_or_else(|| {
                anyhow!("coin {statechain_id} ({amount} sats) is too small to cover the refresh fee {fee_sats} sats")
            })?;

            // 3. Fresh deposit aggregate for EXACTLY amount_out (mints a new statechain_id).
            let fresh_addr = self.get_deposit_address(amount_out).await?;
            let after_dep = self.record().await?;
            let new_statechain_id = after_dep
                .coins
                .iter()
                .find(|c| c.aggregated_address.as_deref() == Some(fresh_addr.as_str()))
                .and_then(|c| c.statechain_id.clone())
                .ok_or_else(|| anyhow!("fresh deposit coin not found after get_deposit_address"))?;
            (fresh_addr, new_statechain_id, amount, amount_out, fee_sats, rate)
        };

        // 4. Re-anchor: SE-co-signed spend of the old outpoint to the fresh aggregate (branch
        //    materialized first for a sub-coin). The explicit rate makes the output == amount_out.
        self.withdraw(&fresh_addr, Some(vec![statechain_id.to_string()]), Some(rate))
            .await?;

        // 5. The withdraw broadcast IS the fresh deposit's funding tx. The new coin confirms via the
        //    watcher (fresh ladder via create_tx1 at the new deposit height); the old coin is now
        //    WITHDRAWING → WITHDRAWN, its outpoint spent and its old backups dead.
        let after_wd = self.record().await?;
        let refresh_txid = after_wd
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(statechain_id))
            .and_then(|c| c.tx_withdraw.clone())
            .unwrap_or_default();

        Ok(RefreshResult {
            old_statechain_id: statechain_id.to_string(),
            new_statechain_id,
            old_amount_sats: amount,
            new_amount_sats: amount_out,
            fee_sats,
            refresh_txid,
            rebate_sats: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The re-anchor fee is exactly ceil(BACKUP_TX_VBYTES * rate), and the refreshed amount is
    // amount - fee — the value the fresh deposit must expect (check_deposit matches on exact value).
    // A sponsored refresh rebates exactly that fee off-chain, so the user's total is preserved.
    #[test]
    fn refresh_fee_and_amount_arithmetic() {
        let fee = |rate: f64| (BACKUP_TX_VBYTES as f64 * rate).ceil() as u64;
        assert_eq!(fee(1.0), 112, "1-in-1-out P2TR at 1 sat/vB");
        assert_eq!(fee(2.0), 224);
        assert_eq!(fee(10.5), 1176, "ceil(112 * 10.5) = 1176");
        // User-pays: refreshed coin = amount - fee.
        let amount = 100_000u64;
        assert_eq!(amount - fee(1.0), 99_888);
        // Operator-pays: the sub-dust fee can't be rebated exactly off-chain (min piece = dust +
        // backup fee), so the operator rebates fee + DUST_LIMIT and the user ends >= whole.
        let rebate = fee(1.0) + crate::transfer::DUST_LIMIT; // 112 + 330 = 442
        assert_eq!(rebate, 442);
        assert_eq!((amount - fee(1.0)) + rebate, amount + crate::transfer::DUST_LIMIT);
    }
}
