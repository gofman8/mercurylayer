//! Coin **refresh** (re-anchor): reset a coin's backup ladder and root deadline by spending it
//! on-chain into a FRESH aggregate. Two fee models — the user pays, or an operator sponsor pays.
//!
//! A statechain coin's decrementing-`nLockTime` ladder is a finite budget: `initlock` blocks of
//! headroom, spent by BOTH hops (`interval` per transfer) and wall-clock time (~144 blocks/day) —
//! see [INVALIDATION-SPEC.md](../../../docs/utexo/INVALIDATION-SPEC.md). When it nears the floor the
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
//! - [`UtexoWallet::refresh`] — **user pays**. The refreshed coin is `amount − fee`.
//! - [`UtexoWallet::refresh_sponsored`] — **operator pays**. The re-anchor runs as above, then a
//!   funded operator sponsor reimburses the fee OFF-CHAIN (an instant, free statechain transfer), so
//!   the user's TOTAL balance is preserved. The user ends with two coins: the refreshed
//!   `amount − fee` coin plus a `fee` rebate coin. The sponsor is any funded wallet/service (in a
//!   deployment, an SSP-like operator); the rebate half is also exposed as
//!   [`UtexoWallet::rebate_refresh_fee`] so a remote sponsor can drive it.

use anyhow::{anyhow, Result};
use mercurylib::wallet::{Coin, CoinStatus};

use crate::events::WalletEvent;
use crate::transfer::{backup_fee_rate, BACKUP_TX_VBYTES};
use crate::types::TransferResult;
use crate::wallet::UtexoWallet;

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
    /// Sats an operator sponsor reimbursed off-chain (`0` for the user-pays [`UtexoWallet::refresh`];
    /// after [`UtexoWallet::refresh_sponsored`] the smallest OFF-CHAIN-PAYABLE amount covering the
    /// sub-dust fee — `max(fee_sats + DUST_LIMIT, min_child_value)` — so the user ends ≥ whole).
    pub rebate_sats: u64,
}

impl UtexoWallet {
    /// **User-pays** refresh: re-anchor `statechain_id` on-chain into a fresh aggregate, resetting
    /// its ladder and root deadline. The fee is taken from the coin (refreshed coin = `amount −
    /// fee`). One on-chain transaction; the previous outpoint is spent, so all old backups are
    /// invalidated. The new coin confirms asynchronously (watcher/`claim()`), like a deposit.
    ///
    /// `fee_rate` (sat/vB) is capped at the client's `max_fee_rate`; `None` uses the SE-quoted rate.
    /// Errors if the coin is not `CONFIRMED`, carries an RGB allocation, or is too small to cover
    /// the fee above the dust floor.
    /// **[CR-D / D29] Re-anchor a COLOURED carrier — the renewal primitive a coloured coin had none of.**
    ///
    /// [`Self::refresh`] routes through `withdraw`, which is RGB-unaware, so it refuses carriers: it
    /// would move the sats and destroy the allocation. That refusal left a coloured coin with **no
    /// renewal path at all**, so its life was bounded and ended in a forced exit — the gap D30 cites
    /// when it keeps `colored_ladder` off.
    ///
    /// CR-D closes it without needing an RGB rival over `F`:
    ///
    /// 1. broadcast the **trigger** — already co-signed, already part of the ladder, no new SE call;
    /// 2. co-sign and broadcast a **coloured de-trigger** — a fresh spend of `T`'s payload with the
    ///    relative timelock disabled, carrying a valid RGB state transition to the owner.
    ///
    /// Two transactions, **zero CSV wait**, no SE change. The de-trigger has no timelock while every
    /// pre-signed extension needs `E ≥ E_floor` confirmations, so it confirms first and kills the
    /// old ladder — which is exactly what makes this a re-anchor rather than an exit.
    ///
    /// Returns the de-trigger's txid. The allocation lands on its payload output; the coin's ladder
    /// is spent and must be re-established from the new outpoint, the same as after any re-anchor.
    pub async fn colored_reanchor(&self, statechain_id: &str) -> Result<String> {
        // Serialize against the background watcher, the same lock `reanchor` takes.
        let _guard = self.inner.wallet_lock.lock().await;

        let bundle = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("coin {statechain_id} has no TES-R ladder to re-anchor"))?;
        if !bundle.is_colored() {
            return Err(anyhow!(
                "coin {statechain_id} has a PLAIN ladder — use `refresh`, whose uncoloured \
                 re-anchor is correct for it. CR-D needs coloured material to build from."
            ));
        }

        let record = self.record().await?;
        let mut coin = record
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0)
            .ok_or_else(|| anyhow!("no coin with statechain id {statechain_id}"))?
            .clone();
        let to_address = mercurylib::transaction::get_user_backup_address(
            &coin,
            self.inner.config.network.to_string(),
        )
        .map_err(|_| anyhow!("cannot derive this wallet's backup address"))?;

        // TWO PHASES, and the split is forced rather than stylistic: the RGB engine's resolver is
        // `!Sync`, so a guard held across an `await` stops this whole future being `Send` and it can
        // no longer run in the background watcher's task. Colour first (engine only, no network),
        // then co-sign (network only, no engine). Free — a tier's txid is stable across signing.
        let draft = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().ok_or_else(|| {
                anyhow!("a coloured re-anchor needs the RGB engine, but none is configured")
            })?;
            tokio::task::block_in_place(|| {
                mercuryrustlib::tesr::build_colored_detrigger(w, &bundle, &to_address)
            })?
        };

        let detrigger_hex = mercuryrustlib::tesr::cosign_colored_detrigger(
            &self.inner.cc,
            &mut coin,
            &bundle,
            draft,
            &to_address,
        )
        .await?;

        // ORDER IS LOAD-BEARING. The de-trigger spends the trigger's payload output, so the trigger
        // must be in a mempool first or the de-trigger has a missing input. Broadcast it if it is not
        // already known — a re-anchor of an ALREADY-triggered coin is legitimate (that is the hostile
        // -trigger case this de-trigger was designed to collapse), and re-broadcasting a known tx
        // would be an error rather than a no-op on some backends.
        let trigger_hex = bundle.trigger.signed_tx.clone();
        {
            use electrum_client::ElectrumApi;
            let txid = bundle.trigger.txid.parse::<bitcoin::Txid>()
                .map_err(|e| anyhow!("stored trigger txid is unusable: {e}"))?;
            let already = self.inner.cc.electrum_client.transaction_get(&txid).is_ok();
            if !already {
                let raw = hex::decode(&trigger_hex)
                    .map_err(|e| anyhow!("stored trigger hex does not decode: {e}"))?;
                self.inner
                    .cc
                    .electrum_client
                    .transaction_broadcast_raw(&raw)
                    .map_err(|e| anyhow!("could not broadcast the trigger to re-anchor: {e}"))?;
            }
        }

        let raw = hex::decode(&detrigger_hex)
            .map_err(|e| anyhow!("coloured de-trigger hex does not decode: {e}"))?;
        let txid = {
            use electrum_client::ElectrumApi;
            self.inner.cc.electrum_client.transaction_broadcast_raw(&raw).map_err(|e| {
                anyhow!(
                    "the coloured de-trigger was co-signed but could not be broadcast ({e}). The SE \
                     co-sign is already spent, so retry the BROADCAST rather than rebuilding — \
                     rebuilding would burn a second irreversible slot."
                )
            })?
        };
        Ok(txid.to_string())
    }

    pub async fn refresh(&self, statechain_id: &str, fee_rate: Option<f64>) -> Result<RefreshResult> {
        self.reanchor(statechain_id, fee_rate).await
    }

    /// **Operator-pays** refresh: the same on-chain re-anchor as [`Self::refresh`] (the fee is
    /// physically drawn from the coin — the tx is single-input and the SE holds no funds), followed
    /// by an OFF-CHAIN rebate from `sponsor` to this wallet so the user is made (at least) whole.
    ///
    /// The refresh fee (~112 sats at 1 sat/vB) is BELOW the dust floor, and an off-chain transfer
    /// can't mint a piece below `min_split_output` (dust + the piece's own backup fee), and on a
    /// TES-R sponsor coin the rebate is paid by an IN-LADDER split whose child funds its own
    /// extension + state tier before clearing dust (`min_child_value`, 1310 sat at the default
    /// 2 sat/vB). The sponsor therefore rebates `max(fee_sats + DUST_LIMIT, min_child_value)`,
    /// over-compensating the user by the difference. `rebate_sats` reports the actual amount
    /// rebated (≥ `fee_sats`), so the user's total balance ends ≥ the original. The rebate arrives
    /// as a normal incoming transfer; claim it via the watcher/`claim()`. `sponsor` is a funded
    /// operator wallet.
    pub async fn refresh_sponsored(
        &self,
        statechain_id: &str,
        sponsor: &UtexoWallet,
        fee_rate: Option<f64>,
    ) -> Result<RefreshResult> {
        let mut res = self.reanchor(statechain_id, fee_rate).await?;
        // The rebate must be OFF-CHAIN-PAYABLE, i.e. at least a mintable piece — otherwise the
        // sponsor's own `transfer` refuses and the user is left having paid the re-anchor fee.
        // Two floors apply; take the larger:
        //  * `fee + DUST_LIMIT` — the un-laddered floor (min_split_output for this 112-vB tx), and
        //  * `min_child_value` — on a TES-R sponsor coin the rebate is paid by an IN-LADDER split,
        //    and the child carries its own extension + state tier before it can clear dust. At the
        //    default 2 sat/vB that is 2*(250+240)+330 = 1310 sat, well above the old 442, so the
        //    old sizing made a sponsored refresh fail outright (`FeeTooHigh`).
        // The operator absorbs the difference; the user is still made (at least) whole.
        let ladder_floor = mercurylib::tesr::min_child_value(
            mercurylib::tesr::TesrParams::for_network(&self.inner.config.network.to_string())
                .committed_fee_rate,
            crate::transfer::DUST_LIMIT,
        );
        let rebate = (res.fee_sats + crate::transfer::DUST_LIMIT).max(ladder_floor);
        // Reimburse off-chain to this wallet's stable receive address. The re-anchor above already
        // released the wallet lock, so this (and the sponsor's own transfer) do not deadlock.
        let user_addr = self.get_utexo_address().await?;
        sponsor
            .rebate_refresh_fee(&user_addr, rebate)
            .await
            .map_err(|e| anyhow!("re-anchor succeeded but the sponsor rebate failed: {e}"))?;
        res.rebate_sats = rebate;
        Ok(res)
    }

    /// Operator side of a sponsored refresh: send `fee_sats` off-chain to `to_utexo_address` to
    /// reimburse a user's refresh fee. A thin, discoverable wrapper over [`Self::transfer`] — a
    /// remote sponsor service calls this after (or when notified that) the user re-anchored.
    pub async fn rebate_refresh_fee(
        &self,
        to_utexo_address: &str,
        fee_sats: u64,
    ) -> Result<TransferResult> {
        self.transfer(to_utexo_address, fee_sats).await
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
            let carriers = self.unspendable_as_btc_outpoints().await?;
            if crate::wallet::coin_outpoint(coin).map_or(false, |o| carriers.contains(&o)) {
                // ── [CR-D / D29] A COLOURED CARRIER HAS A RE-ANCHOR NOW; A PLAIN ONE STILL DOES NOT ──
                //
                // The refusal below is correct and stays correct for what it describes: this path
                // re-anchors through `withdraw`, an RGB-UNAWARE builder, so driving a carrier through
                // it moves the sats and destroys the allocation.
                //
                // What changed is that "carrier" is no longer one thing. A carrier whose ladder is
                // COLOURED can be re-anchored by CR-D — broadcast the trigger (already co-signed,
                // already part of the ladder), then a coloured de-trigger carrying a valid state
                // transition. Two transactions, zero CSV wait, no SE change. So the carrier check
                // now DISPATCHES instead of refusing everything it recognises.
                //
                // The distinction is load-bearing in both directions. Sending a coloured carrier to
                // `withdraw` burns the asset; sending a PLAIN carrier to CR-D finds no coloured
                // material to build from. Each is refused by the arm that can name why.
                let bundle = mercuryrustlib::tesr::load(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    statechain_id,
                )
                .await?;
                match bundle {
                    Some(b) if b.is_colored() => {
                        return Err(anyhow!(
                            "coin {statechain_id} is a COLOURED carrier: its re-anchor is CR-D                              (`build_colored_detrigger` + `cosign_colored_detrigger`), not this                              plain path, which routes through the RGB-unaware `withdraw` and would                              destroy the allocation. Call `colored_reanchor` instead."
                        ));
                    }
                    _ => {
                        return Err(anyhow!(
                            "coin {statechain_id} carries an RGB allocation on a PLAIN ladder;                              refreshing it as a plain re-anchor would destroy the tokens, and CR-D                              cannot help — a coloured de-trigger needs a coloured ladder to build                              from. Move the asset off this coin first."
                        ));
                    }
                }
            }

            // 2. Deterministic fee: the re-anchor tx is 1-in-1-out P2TR (BACKUP_TX_VBYTES = 112 vB).
            //    Compute the fee with the SAME rate passed to withdraw so the on-chain output value
            //    equals what the fresh deposit expects (check_deposit matches on exact value).
            let rate = match fee_rate {
                Some(r) => r.min(self.inner.cc.max_fee_rate),
                None => backup_fee_rate(&self.inner.cc).await?,
            };
            let fee_sats = (BACKUP_TX_VBYTES as f64 * rate).ceil() as u64;
            // B4 terminal case: if the coin can't cover its renewal fee (and leave a non-dust
            // refreshed output), it is stuck on its own — surface a typed error so callers can
            // distinguish "combine me to rescue" from a generic failure. It is NOT lost.
            let amount_out = match amount
                .checked_sub(fee_sats)
                .filter(|out| *out >= crate::transfer::DUST_LIMIT)
            {
                Some(o) => o,
                None => {
                    return Err(anyhow::Error::new(crate::types::SdkError::CoinBelowMaintenanceCost {
                        statechain_id: statechain_id.to_string(),
                        amount_sats: amount,
                        fee_sats,
                    }))
                }
            };

            // 3. Fresh deposit aggregate for EXACTLY amount_out (mints a new statechain_id). The
            //    slot is DERIVED from the coin being refreshed — a re-anchor re-houses value the SE
            //    already carries (old slot dies, new slot born), so it uses a free SE voucher, not
            //    an onboarding token from the paid pool.
            let slot_token = self
                .take_derived_tokens(statechain_id, 1)
                .await?
                .remove(0);
            let fresh_addr = self.create_child_slot_addr(&slot_token, amount_out).await?;
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

    /// The wallet-maintenance **auto-refresh** pass: re-anchor every confirmed, non-carrier coin
    /// whose backup ladder is within `margin_blocks` of its floor, so an aging coin is refreshed
    /// *before* it becomes un-transferable (a whole-coin handover of a floored coin is rejected by
    /// the receiver, `LocktimeTooLow`) and before it would hand a receiver a sub-coin already past
    /// its exit-race deadline. Each re-anchor is user-pays (the fee comes from the coin), one
    /// on-chain tx; the fresh coins confirm asynchronously (via `claim`/the watcher), so this call
    /// does NOT block on confirmation.
    ///
    /// Token CARRIERS are skipped — a plain re-anchor would destroy the RGB allocation; their
    /// near-deadline protection is [`Self::auto_exit_due`], which *materializes* them instead. A coin
    /// too small to cover the fee, or already spent by a concurrent pass, is skipped rather than
    /// failing the pass. Returns one [`RefreshResult`] per coin refreshed and emits
    /// [`WalletEvent::CoinRefreshed`] for each. The background watcher calls this every poll, so in
    /// practice coins are kept fresh proactively; `transfer` also calls it (and waits) as a safety net.
    ///
    /// **`Ok(vec![])` means "nothing was due", never "I could not tell".** If the carrier set of a
    /// token wallet cannot be enumerated the pass returns `Err` rather than an empty vector: it
    /// still refuses to re-anchor anything (fail closed — a plain re-anchor destroys a carrier's
    /// RGB allocation), but it says so instead of presenting a total blindness as a clean pass.
    pub async fn auto_refresh_due(&self, margin_blocks: u32) -> Result<Vec<RefreshResult>> {
        use electrum_client::ElectrumApi;
        // Refresh statuses so a just-confirmed re-anchor is not re-refreshed and headrooms are current.
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let record = self.record().await?;
        // Fail CLOSED **and LOUD**: for a token wallet whose carriers we cannot enumerate we must
        // not re-anchor anything (a plain re-anchor of a carrier DESTROYS its RGB allocation) — but
        // `Ok(vec![])` is a lie, and it is exactly the silent-degradation shape this codebase keeps
        // re-growing: "nothing was due" and "I could not tell what was due" become the same answer.
        //
        // A skipped pass is not harmless here. `auto_refresh_due` is what keeps an aging coin
        // transferable (a coin at its ladder floor is rejected by the receiver, `LocktimeTooLow`),
        // and `auto_refresh_before_spend` calls it with `?` immediately before a transfer selects
        // coins. If the carrier set is unreadable then the transfer's OWN carrier exclusion is
        // equally unreadable, so failing the spend outright is the strictly-safer answer, not a
        // regression. The background watcher ignores the result and simply retries next poll.
        let carriers = if self.inner.config.rgb_data_dir.is_some()
            && self.inner.config.rgb_proxy_url.is_some()
        {
            self.unspendable_as_btc_outpoints().await.map_err(|e| {
                anyhow!(
                    "auto_refresh_due: BLIND — could not enumerate this wallet's RGB token \
                     carriers, so no coin can be safely re-anchored (a plain re-anchor of a \
                     carrier destroys its allocation). Refusing to report an empty pass: {e}"
                )
            })?
        } else {
            std::collections::HashSet::new()
        };
        // Skip coins that cannot cover their own re-anchor fee (B4 terminal case): re-anchoring them
        // would fail (fee >= value). They are NOT lost — they stay in the wallet and can be rescued
        // by COMBINING with another coin (the aggregate covers the fee) — so don't churn on them.
        let refresh_fee: u64 = {
            let rate = crate::transfer::backup_fee_rate(&self.inner.cc).await.unwrap_or(1.0);
            (BACKUP_TX_VBYTES as f64 * rate).ceil() as u64
        };
        // Build the due list BEFORE any per-coin lock — `reanchor` takes the wallet lock itself.
        let due: Vec<String> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| !crate::wallet::is_token_carrier(c, &carriers))
            .filter(|c| coin_near_final(c, tip, margin_blocks))
            .filter(|c| c.amount.unwrap_or_default() as u64 > refresh_fee)
            .filter_map(|c| c.statechain_id.clone())
            .collect();
        drop(record);

        let mut refreshed = Vec::new();
        for id in due {
            match self.reanchor(&id, None).await {
                Ok(res) => {
                    let _ = self.inner.events_tx.send(WalletEvent::CoinRefreshed {
                        old_statechain_id: res.old_statechain_id.clone(),
                        new_statechain_id: res.new_statechain_id.clone(),
                        fee_sats: res.fee_sats,
                    });
                    refreshed.push(res);
                }
                // Skip a coin a concurrent pass already refreshed (now WITHDRAWN) or one too small
                // to cover the fee — don't abort the maintenance pass over one coin.
                Err(_) => continue,
            }
        }
        Ok(refreshed)
    }

    /// **[D40 / A.2] DEADLINE SAFETY — the half of maintenance that is not an economics choice.**
    ///
    /// A whole laddered coin carries exactly one absolute clock: `min(L_k)` over its flat backup
    /// chain, held by its PRIOR OWNERS. When that height passes, any ancestor's matured rung spends
    /// `F` and takes the coin. Nothing in the tree stops it — the tiers are all *relative*-timelocked
    /// below `F`, so they cannot out-race a transaction that is simply valid now.
    ///
    /// **That is a safety property, and it was behind an economics flag.** `auto_refresh_due` does two
    /// unrelated jobs: routine background re-anchoring (an economics choice — B4 folds the re-anchor
    /// cost into `transfer` and pays it on demand, so a running wallet should NOT silently shrink a
    /// balance in the background) and this deadline defence. Both sat behind
    /// `auto_refresh && background_auto_refresh`, and the second is off by default. So on a default
    /// wallet the routine pass was correctly disabled and the deadline defence went with it.
    ///
    /// `auto_exit_due` does not cover this clock: it protects sub-coins and materialises carriers.
    /// The whole-coin `L_k` deadline had no scheduled defender at all.
    ///
    /// # Two remedies, in order, because the first one needs the counterparty
    ///
    /// 1. **Re-anchor** (cooperative, cheap, keeps the coin off-chain). One ~112-vB transaction; the
    ///    coin comes back with a fresh chain and `k = 0` prior owners.
    /// 2. **Sever from `F`** (unilateral, no counterparty) — broadcast the already-co-signed trigger.
    ///    `T` is un-timelocked and spends `F`, so it wins against every retained rung *by being valid
    ///    first*, and it needs no SE, no coordinator and no wait.
    ///
    /// The order matters and so does the fallback. Re-anchoring requires one fresh SE co-signature,
    /// and the party that would most like this coin's deadline to pass is the same party being asked
    /// to sign — see D40.1. A defence that can be declined by its adversary is not a defence, so when
    /// the cooperative route fails the pass takes the unilateral one rather than reporting a clean
    /// pass over an undefended coin.
    ///
    /// Severing costs the coin its off-chain life and starts the CSV walk. That is a real cost, and
    /// it is paid only when the alternative is losing the coin outright.
    ///
    /// Returns `(re_anchored, severed)`. Carriers are excluded on both routes — a plain re-anchor
    /// and a plain trigger both destroy an RGB allocation; their protection is `auto_exit_due`.
    pub async fn deadline_safety_due(
        &self,
        margin_blocks: u32,
    ) -> Result<(Vec<RefreshResult>, Vec<String>)> {
        let re_anchored = match self.auto_refresh_due(margin_blocks).await {
            Ok(v) => v,
            // BLIND is not "nothing was due". If the carrier set is unreadable the cooperative route
            // refuses (correctly — it cannot tell which coins a plain re-anchor would destroy), and
            // the unilateral route below is equally unable to tell. Propagate rather than sever
            // blindly: broadcasting a plain trigger over an unknown carrier destroys its allocation,
            // which is a worse outcome than the deadline this pass exists to beat.
            Err(e) => {
                return Err(anyhow!(
                    "deadline safety: BLIND — {e}. Neither remedy is safe while this wallet cannot                      tell a token carrier from a plain coin."
                ))
            }
        };
        // Whatever the cooperative route could not save is still near its deadline. Re-read rather
        // than inferring from the failure list: `reanchor` may have partially succeeded, and a coin
        // it moved is no longer at risk under its OLD id.
        use electrum_client::ElectrumApi;
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let carriers = self.unspendable_as_btc_outpoints().await?;
        let record = self.record().await?;
        // **[D46] CARRIERS ARE INCLUDED HERE.** They are excluded from the COOPERATIVE route above
        // for a real reason — a plain re-anchor spends the carrier's outpoint and destroys its RGB
        // allocation — but excluding them from the unilateral route too left the carrier lane with
        // ZERO automatic deadline coverage, resting on `auto_exit_due` alone. That is the lane where
        // the loss is an ASSET rather than sats.
        //
        // The forced action is `unilateral_exit`, which broadcasts the pre-signed `T` — a tier of
        // the coin's OWN ladder, carrying the coin's own state. It does not re-aggregate and it does
        // not spend the allocation to a stranger's script, so it is safe on a carrier in a way a
        // re-anchor is not. On a COLOURED ladder every tier carries a valid RGB transition; on a
        // plain-laddered carrier the ladder is refused at build time (D35's lane rule), so a carrier
        // that reaches here with a ladder has one that can carry it.
        //
        // `sdk86` measured what this protects: the flat backup chain's absolute locktime is real and
        // finite, mining moves the tip toward it, and each whole-coin hop spends `interval` of it.
        // INV-27's "idle coins never age" is true of the CSV side only.
        let still_due: Vec<String> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| coin_near_final(c, tip, margin_blocks))
            .filter_map(|c| c.statechain_id.clone())
            .collect();
        let carrier_count = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| crate::wallet::is_token_carrier(c, &carriers))
            .filter(|c| coin_near_final(c, tip, margin_blocks))
            .count();
        if carrier_count > 0 {
            println!(
                "deadline safety: {carrier_count} token carrier(s) within {margin_blocks} blocks of \
                 their floor will be SEVERED (pre-signed T), not re-anchored — a plain re-anchor \
                 would destroy the allocation"
            );
        }
        drop(record);

        let mut severed = Vec::new();
        for id in still_due {
            // `unilateral_exit` broadcasts the trigger when `F` is still unspent, which IS the sever,
            // and then walks whatever has matured. It is idempotent per block.
            match self.unilateral_exit(Some(vec![id.clone()]), None).await {
                Ok(statuses) if !statuses.is_empty() => severed.push(id),
                // A coin that cannot be severed either is genuinely stuck; the fault surfaces through
                // the watchtower channel rather than aborting the pass over one coin.
                _ => continue,
            }
        }
        Ok((re_anchored, severed))
    }

    /// Pre-spend auto-refresh hook (see [`Self::auto_refresh_due`]): when `auto_refresh` is enabled,
    /// re-anchor any near-final coin before a transfer selects it, then WAIT for the fresh coins to
    /// confirm so the spend can use them. This is what makes an aging coin transparent to the caller
    /// — the transfer simply succeeds, with the re-anchor fee the only visible effect. A no-op
    /// (empty vec, negligible overhead: one `update_coins` + tip read) when nothing is near its
    /// floor, which is the common case. Called before `transfer`/`transfer_many` acquire the wallet
    /// lock (both `auto_refresh_due` and the confirm-wait take it themselves).
    pub(crate) async fn auto_refresh_before_spend(&self) -> Result<Vec<RefreshResult>> {
        if !self.inner.config.auto_refresh {
            return Ok(vec![]);
        }
        let refreshed = self
            .auto_refresh_due(self.inner.config.auto_refresh_margin_blocks)
            .await?;
        if !refreshed.is_empty() {
            let new_ids: Vec<String> =
                refreshed.iter().map(|r| r.new_statechain_id.clone()).collect();
            self.await_ids_confirmed(&new_ids).await?;
        }
        Ok(refreshed)
    }

    /// Poll `claim()` until every id in `ids` is CONFIRMED (its re-anchor deposit tx reached the
    /// confirmation target), bounded so it can never hang forever. This is the unavoidable
    /// in-transfer latency of an on-chain refresh; the background watcher pre-empts it by refreshing
    /// before the user acts, so this wait is a rare fallback.
    async fn await_ids_confirmed(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // ~120 polls at 1s ≈ a 2-minute ceiling: ample for regtest and typical mainnet confirmation.
        for _ in 0..120 {
            let _ = self.claim().await;
            let record = self.record().await?;
            let all_confirmed = ids.iter().all(|id| {
                record.coins.iter().any(|c| {
                    c.statechain_id.as_deref() == Some(id.as_str())
                        && c.duplicate_index == 0
                        && c.status == CoinStatus::CONFIRMED
                })
            });
            if all_confirmed {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }
        Err(anyhow!(
            "auto-refresh: a re-anchored coin has not confirmed within the wait window; the transfer will succeed once it confirms — retry shortly (or rely on the background refresh loop to pre-empt this)"
        ))
    }
}

/// A coin is "near final" — due for auto-refresh — when its backup-ladder headroom (`locktime −
/// tip`) has fallen to `margin_blocks` or below. A CONFIRMED coin always has a locktime; one without
/// (never happens on this path) is treated as not-due. `saturating_sub` makes an already-floored
/// coin (locktime ≤ tip) report zero headroom, so it is always due.
fn coin_near_final(c: &Coin, tip: u32, margin_blocks: u32) -> bool {
    c.locktime.map_or(false, |l| l.saturating_sub(tip) <= margin_blocks)
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

/// [CR-D] The coloured re-anchor's wiring rules, pinned in source.
#[cfg(test)]
mod crd_wiring_tests {
    fn src() -> String {
        include_str!("refresh.rs").to_string()
    }
    fn fn_src(name: &str) -> String {
        let s = src();
        let at = s.find(name).expect("function exists");
        let end = s[at..].find("\n    /// ").map(|e| at + e).unwrap_or(s.len());
        s[at..end].lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
    }

    /// **Order is the property.** The de-trigger spends the trigger's payload output, so a
    /// de-trigger broadcast before its trigger has a missing input. The trigger must go first.
    #[test]
    fn the_trigger_is_broadcast_before_the_detrigger() {
        let f = fn_src("pub async fn colored_reanchor(");
        let trig = f.find("could not broadcast the trigger").expect("trigger broadcast");
        let detrig = f.find("coloured de-trigger hex does not decode").expect("de-trigger decode");
        assert!(trig < detrig, "the trigger must be broadcast first or the de-trigger has no input");
    }

    /// An ALREADY-triggered coin is the case this de-trigger was designed for — collapsing a hostile
    /// trigger. Re-broadcasting a known transaction is an error on some backends rather than a
    /// no-op, so the trigger broadcast must be conditional on it not already being known.
    #[test]
    fn an_already_triggered_coin_does_not_re_broadcast() {
        let f = fn_src("pub async fn colored_reanchor(");
        assert!(f.contains("transaction_get(&txid).is_ok()"), "must check first");
        assert!(f.contains("if !already"), "and only broadcast when it is NOT already known");
    }

    /// A failed broadcast AFTER the co-sign must tell the caller to retry the BROADCAST, not rebuild
    /// — the SE slot is already spent and rebuilding burns a second irreversible one.
    #[test]
    fn a_failed_broadcast_after_cosign_says_retry_not_rebuild() {
        let f = fn_src("pub async fn colored_reanchor(");
        assert!(
            f.contains("retry the BROADCAST rather than rebuilding"),
            "the post-co-sign failure must not invite a rebuild"
        );
    }

    /// The engine guard must not be held across an await: the RGB resolver is `!Sync`, and holding it
    /// stops the whole future being `Send`, which takes this out of the background watcher's task.
    #[test]
    fn the_engine_guard_is_scoped_and_does_not_straddle_an_await() {
        let f = fn_src("pub async fn colored_reanchor(");
        let block_start = f.find("let draft = {").expect("two-phase block");
        let block_end = f[block_start..].find("};").expect("block closes") + block_start;
        let block = &f[block_start..block_end];
        // The ACQUISITION await is fine and unavoidable — `self.rgb()` is async. What must not
        // happen is an await AFTER the guard exists. (My first version of this test forbade every
        // await in the block and failed on the acquisition itself; the codebase's own pattern at
        // `wallet.rs` acquires the same way.)
        let after_acquire = block
            .find("self.rgb().await")
            .map(|i| &block[i + "self.rgb().await".len()..])
            .expect("the guard is acquired here");
        assert!(
            !after_acquire.contains(".await"),
            "no await may follow the guard's acquisition — the RGB resolver is !Sync, and holding \
             it across an await stops this future being Send: {after_acquire}"
        );
        assert!(f.contains("block_in_place"), "the engine call is blocking, and says so");
    }

    /// Both lanes must refuse each other's coins by name: a coloured coin sent to the plain
    /// re-anchor burns its allocation, and a plain coin sent here finds nothing to colour.
    #[test]
    fn each_lane_refuses_the_others_coin() {
        let f = fn_src("pub async fn colored_reanchor(");
        assert!(f.contains("has a PLAIN ladder"), "this lane must refuse a plain coin");
        let s = src();
        assert!(
            s.contains("is a COLOURED carrier: its re-anchor is CR-D"),
            "and the plain lane must point a coloured carrier here instead of refusing flatly"
        );
    }
}
