//! Sending: exact-amount transfers with automatic coin selection and off-chain split.
//!
//! Mercury transfers move whole statechain coins (like Spark leaves). `transfer` makes arbitrary
//! amounts frictionless:
//! 1. If a subset of confirmed coins sums to the amount exactly → native key-handover transfer of
//!    each coin (works with any Mercury wallet as receiver, fully async).
//! 2. Otherwise → the SDK **splits one coin off-chain** (SE-co-signed, un-broadcast, single-use
//!    sub-coins) to mint the exact remainder, then transfers the pieces. The split sub-coins are
//!    SDK-native coins; the sender keeps the change sub-coin.

use anyhow::{anyhow, Result};
use bitcoin::psbt::Psbt;
use mercurylib::transaction::{
    create_and_commit_nonces, create_signature, get_partial_sig_request_for_colored_tx,
    get_unsigned_split_psbt, new_backup_transaction,
};
use mercurylib::wallet::{Coin, CoinStatus};
use std::str::FromStr;

use crate::select::{self, Candidate, Plan};

/// How the PIECE child of an in-ladder split is latched for a non-exact Lightning swap
/// (LIGHTNING.md §2b). Plain in-ladder payments use [`InLadderLatch::None`].
pub enum InLadderLatch<'a> {
    /// Not a Lightning swap — convey the piece outright.
    None,
    /// Non-exact PAY: latch the piece to a known merchant-invoice payment hash. The SSP censuses +
    /// pays; settlement reveals the preimage that unlocks the piece.
    External(&'a str),
    /// Non-exact RECEIVE: the SE mints the preimage; the returned hash goes into the SSP's HODL
    /// invoice and `settle_receive` retrieves the preimage from the SE to release + claim.
    ClassicMinted,
}

/// What [`UtexoWallet::recover_in_ladder_splits`] did with one interrupted split.
#[derive(Clone, Debug)]
pub enum InLadderSplitOutcome {
    /// The co-signed material survived the crash and the split was rebuilt from it: our own change
    /// child is on disk and exitable again. `unconveyed_pieces` are the recipients' children — real,
    /// complete coins of this wallet that were never handed over. Recovery NEVER conveys them by
    /// itself; call [`UtexoWallet::convey_recovered_piece`] to complete the payment.
    Replayed {
        change_statechain_id: Option<String>,
        unconveyed_pieces: Vec<String>,
    },
    /// The crash landed before the parent was terminalized: nothing was lost and nothing was
    /// consumed — the payment can simply be made again.
    Retryable,
    /// The irreducible window: the SE consumed the parent's budget but this process never recorded
    /// the `SP` co-signature, so it can never be produced again. The parent's value is recoverable
    /// ONLY by a unilateral exit of its own backup.
    CooperativePathLost,
}

/// One entry of the in-ladder split recovery report.
#[derive(Clone, Debug)]
pub struct InLadderSplitRecovery {
    pub op_id: String,
    /// `"in_ladder_split"` or `"child_in_ladder_split"`.
    pub lane: String,
    /// The coin the split terminalized (the root parent, or the child that was re-split).
    pub terminalized_statechain_id: String,
    pub outcome: InLadderSplitOutcome,
}

/// Which split route a `transfer_many` parent takes. Chosen by the parent's SHAPE, exactly as
/// `transfer` chooses between `in_ladder_pay` / `child_in_ladder_pay` / `split_coin`: a parent that
/// carries a TES-R ladder must never take the plain split ([B1] — a retained, un-timelocked trigger
/// over the parent's funding `F` would void it and destroy every recipient's piece).
enum ManyRoute {
    /// Laddered ROOT coin: one split state `SP` over `X_m.out[0]`.
    InLadderRoot,
    /// Received in-ladder CHILD: one split state `CSP` at the child's own level.
    InLadderChild,
    /// [CATS spine batch] A SPINE TIP: the next batch, `SP_{i+1}` over the tip's own `SP_i.out[K]`.
    SpineBatch,
    /// Un-laddered coin (a plain split sub-coin): the N+1-output plain split.
    PlainSplit,
}

/// One `TransferResult` per recipient for an in-ladder `transfer_many`: each piece was conveyed
/// directly to its recipient's mailbox inside the split (never through the key-handover loop), so
/// the piece's statechain id IS the coin the recipient adopts at claim.
fn inladder_many_results(recipients: &[(String, u64)], piece_sids: &[String]) -> Vec<TransferResult> {
    recipients
        .iter()
        .zip(piece_sids)
        .map(|((recipient, amount), sid)| TransferResult {
            receiver_address: recipient.clone(),
            total_sats: *amount,
            coins: vec![TransferredCoin { statechain_id: sid.clone(), amount_sats: *amount }],
            used_split: true,
        })
        .collect()
}
use crate::types::{SdkError, TransferResult, TransferredCoin};
use crate::wallet::{coin_outpoint, UtexoWallet};

/// True if this coin's utxo currently carries an RGB token allocation. Such coins must never be
/// selected for a plain-BTC spend — doing so destroys the allocation (review H2).
fn is_token_carrier(c: &Coin, carriers: &std::collections::HashSet<String>) -> bool {
    coin_outpoint(c).map_or(false, |o| carriers.contains(&o))
}

/// [B2] The coins a payment may spend — ONE definition, used by `quote_transfer` AND by `transfer`.
/// A shared floor is not enough if the two sides look at different wallets: `fundable` is computed
/// over this set and the plan is executed over this set, so they must be the same set.
///
/// Confirmed, non-duplicate, not an RGB carrier (spending one as plain BTC destroys the allocation),
/// and not STUCK. A coin whose value is at or below the fee of its own re-anchor cannot pay for its
/// own renewal; it is reported for rescue-by-combine rather than silently planned into a payment.
/// Returns `(spendable coins, stuck statechain ids)`.
fn payment_coins(
    coins: &[Coin],
    carriers: &std::collections::HashSet<String>,
    refresh_fee: u64,
) -> (Vec<Coin>, Vec<String>) {
    let live: Vec<&Coin> = coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .filter(|c| !is_token_carrier(c, carriers))
        .collect();
    let amt = |c: &&Coin| c.amount.unwrap_or_default() as u64;
    let stuck = live
        .iter()
        .filter(|c| amt(c) <= refresh_fee)
        .filter_map(|c| c.statechain_id.clone())
        .collect();
    let usable = live.iter().filter(|c| amt(c) > refresh_fee).map(|c| (*c).clone()).collect();
    (usable, stuck)
}

impl UtexoWallet {
    /// [B2] Resolve how a coin is laddered — the ONE resolution that fixes both the split route and
    /// the split floor. `transfer` dispatches on it and `quote_transfer` quotes on it, so neither can
    /// pick a route the other did not.
    ///
    /// [B3] FAIL CLOSED. A bundle read that FAILS propagates. It must never be read as "no ladder":
    /// `Unladdered` is both the cheaper cost model (the ~300-sat split reserve instead of the ~2 536-sat
    /// in-ladder cost) and the LOWER floor (554 instead of 1 310 at 2 sat/vB), so a swallowed DB error
    /// would quote an unfundable payment as fundable and route a laddered coin at un-laddered prices —
    /// exactly the silent-degradation shape.
    pub(crate) async fn parent_shape(&self, statechain_id: &str) -> Result<ParentShape> {
        // [CATS/V4] THE SPINE TIP, FIRST — and this arm is why the tip needed a record of its own.
        //
        // A tip has no `tesr-` row and no `ctesr-` row, so before this arm existed it fell through
        // every probe below to `Ok(ParentShape::Unladdered)` — a POSITIVE answer, arrived at by
        // three consecutive absences, and wrong in three ways at once: the cheaper cost model, the
        // LOWER floor, and a route to `split_coin`, which is the [B1]-unsafe plain split of a coin
        // that IS laddered. Exactly the silent-degradation shape [B3] made this function
        // fail-closed for, arriving through a door [B3] did not cover: not a swallowed error, an
        // unasked question.
        //
        // Probed FIRST because it is the narrowest and most specific key. The tip's sid is a fresh
        // child slot the sender owns; it never carries a `tesr-` row, and it carries a `ctesr-` row
        // only after it has been handed over — at which point it is no longer this wallet's tip.
        if let Some(tip) = mercuryrustlib::tesr::load_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        {
            return Ok(ParentShape::SpineTip {
                fee_rate: tip.parent.fee_rate,
                // The next batch's `SP_{i+1}` spends the tip's own funding outpoint `SP_i.out[K]`,
                // NOT the cap's output — the cap is the tier being replaced, not the one being
                // extended.
                split_source_value: tip.sp_out_value,
            });
        }
        if let Some(cb) = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        {
            return Ok(ParentShape::Child {
                fee_rate: cb.parent.fee_rate,
                split_source_value: cb.child_extension.out_value,
            });
        }
        if let Some(bundle) = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        {
            let out_value = bundle.current().extension.out_value;
            return Ok(ParentShape::Root { fee_rate: bundle.fee_rate, split_source_value: out_value });
        }
        Ok(ParentShape::Unladdered)
    }

    /// [B2] **THE payment planner.** `quote_transfer` and `transfer` both call this and nothing else,
    /// so `fundable` and the executor's verdict are one computation over one coin set, not two that
    /// happen to agree today.
    ///
    /// `select::plan_with_floor` is shape-blind — it sees only amounts — so it is ADVISORY here. It
    /// is run at the smallest floor any candidate imposes (so it never refuses a split some coin
    /// could actually make), and the coin it names is then judged by [`split_preflight_pure`] at THAT
    /// coin's own floor, which BINDS. A coin the executor would refuse is marked un-splittable — it
    /// can still be handed over whole — and the plan is re-run; refusing the whole payment because
    /// the first candidate did not fit would be a different disagreement, not a fix. Each pass
    /// removes one candidate, so the loop terminates.
    pub(crate) async fn plan_payment(
        &self,
        coins: &[Coin],
        amount_sats: u64,
    ) -> Result<PaymentPlan> {
        let backup_rate = backup_fee_rate(&self.inner.cc).await?;
        let network = self.inner.config.network.to_string();

        // One shape — and therefore one floor — per candidate, resolved ONCE. A failed read
        // propagates [B3]: it must not become "un-laddered", the cheapest and lowest-floored answer.
        let mut shapes: Vec<ParentShape> = Vec::with_capacity(coins.len());
        for c in coins {
            let sid = c
                .statechain_id
                .clone()
                .ok_or_else(|| anyhow!("coin without statechain id"))?;
            shapes.push(self.parent_shape(&sid).await?);
        }
        let planning_floor = shapes
            .iter()
            // The SMALLEST floor either leg of any candidate imposes — this is advisory, and the
            // named coin is re-judged per-leg by `split_preflight_pure`, which BINDS.
            .map(|s| split_output_floors(backup_rate, *s).planning())
            .min()
            // No candidates: the laddered floor stands in, since `claim()` ladders every fresh root
            // coin unconditionally and the laddered case is therefore the default, not the exception.
            .unwrap_or_else(|| planning_split_floor(backup_rate, &network));

        let mut candidates: Vec<Candidate> = coins
            .iter()
            .enumerate()
            .map(|(index, c)| Candidate {
                index,
                amount_sats: c.amount.unwrap_or_default() as u64,
                splittable: true,
            })
            .collect();

        let mut last_refusal: Option<SplitChoice> = None;
        loop {
            let plan = select::plan_with_floor(&candidates, amount_sats, planning_floor);
            let (split, piece_sats) = match &plan {
                Plan::WithSplit { split, split_amount, .. } => (*split, *split_amount),
                // Exact / Insufficient: no split is proposed. If candidates were refused on the way
                // here, THAT refusal is the reason, not a bare "insufficient".
                _ => return Ok(PaymentPlan { plan, split: last_refusal }),
            };
            let statechain_id = coins[split]
                .statechain_id
                .clone()
                .ok_or_else(|| anyhow!("coin without statechain id"))?;
            let preflight = split_preflight_pure(
                backup_rate,
                shapes[split],
                coins[split].amount.unwrap_or_default() as u64,
                piece_sats,
            );
            let choice = SplitChoice { statechain_id, piece_sats, preflight };
            if choice.preflight.admission.is_ok() {
                return Ok(PaymentPlan { plan, split: Some(choice) });
            }
            // `Candidate.index` is its position, so this is the coin the planner named.
            candidates[split].splittable = false;
            last_refusal = Some(choice);
        }
    }

    /// Send `amount_sats` to a statechain address. Exact amounts always work: the SDK either finds
    /// an exact subset of coins or mints one via an off-chain split. The receiver claims
    /// asynchronously (their SDK background watcher, or any Mercury wallet's receive flow for the
    /// exact-subset path).
    pub async fn transfer(&self, receiver_address: &str, amount_sats: u64) -> Result<TransferResult> {
        // Auto-refresh (embedded state transition): re-anchor any near-final coin BEFORE selecting
        // coins to spend, so an aging coin never fails a handover or hands the receiver a coin past
        // its exit-race deadline. Transparent to the caller — the re-anchor fee is the only visible
        // effect. No-op (negligible cost) when disabled or nothing is near its floor. Runs before the
        // wallet lock since it (and its confirm-wait) take the lock themselves.
        let _ = self.auto_refresh_before_spend().await?;
        let _guard = self.inner.wallet_lock.lock().await;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;
        let carriers = self.unspendable_as_btc_outpoints().await?;
        // Received in-ladder split CHILDREN are FIRST-CLASS: the receiver co-owns `A_child` (the
        // handover completed at claim) and the child is left non-terminal, so it can be re-transferred
        // off-chain via `child_retransfer` — Spark-parity multi-hop with zero on-chain footprint.
        //
        // A `child_claim_sids` read used to sit here to EXCLUDE children from the split candidate
        // set, back when a child could not itself be split. Commit C made child-level in-ladder
        // splits real and [B2] moved candidate selection into the shared `payment_coins` /
        // `plan_payment` pair, so the exclusion is gone and the read was dead — one DB round-trip
        // per transfer whose result nothing consulted, under a comment asserting the opposite of
        // what the code fifteen lines down now does.

        // [B2] The SAME coin set and the SAME planner the quote uses (`payment_coins` +
        // `plan_payment`), so `fundable: true` followed by a refusal is no longer expressible: the
        // quote's verdict and this executor's verdict are the same call on the same inputs.
        //
        // Children participate as splittable candidates too (child-level in-ladder split, Commit C):
        // a non-exact payment that selects a child routes to `child_in_ladder_pay` below.
        let backup_rate = backup_fee_rate(&self.inner.cc).await?;
        let refresh_fee = (BACKUP_TX_VBYTES as f64 * backup_rate).ceil() as u64;
        let (spendable, _stuck) = payment_coins(&record.coins, &carriers, refresh_fee);
        let planned = self.plan_payment(&spendable, amount_sats).await?;

        // An in-ladder split payment (laddered coin) is conveyed directly to the recipient inside
        // the split, not handed over in the loop below; track its piece for the returned result.
        let mut inladder_piece: Option<(String, u64)> = None;
        let (mut to_send, used_split): (Vec<String>, bool) = match &planned.plan {
            Plan::Insufficient { available } => {
                // A candidate the planner named but the executor refuses is a MORE precise answer
                // than "insufficient" — report the actual refusal when there was one.
                if let Some(choice) = &planned.split {
                    if let Err(why) = &choice.preflight.admission {
                        return Err(anyhow!(
                            "cannot send {amount_sats} sat: no coin can mint the piece. Closest was \
                             {} on its {} route (per-leg floor {}): {why}",
                            choice.statechain_id,
                            choice.preflight.shape.route(),
                            choice.preflight.floors.describe()
                        ));
                    }
                }
                return Err(SdkError::InsufficientBalance {
                    requested_sats: amount_sats,
                    available_sats: *available,
                }
                .into());
            }
            Plan::Exact(indices) => (
                indices
                    .iter()
                    .filter_map(|&i| spendable[i].statechain_id.clone())
                    .collect(),
                false,
            ),
            Plan::WithSplit { whole, .. } => {
                let mut ids: Vec<String> = whole
                    .iter()
                    .filter_map(|&i| spendable[i].statechain_id.clone())
                    .collect();
                let choice = planned
                    .split
                    .as_ref()
                    .ok_or_else(|| anyhow!("planner proposed a split without naming a coin"))?;
                // `plan_payment` only returns `WithSplit` with an ADMISSIBLE preflight — the same
                // verdict `quote_transfer` reported as `fundable`.
                if let Err(why) = &choice.preflight.admission {
                    return Err(anyhow!(
                        "coin {} cannot mint a {}-sat piece on its {} route (per-leg floor {}): {why}",
                        choice.statechain_id,
                        choice.piece_sats,
                        choice.preflight.shape.route(),
                        choice.preflight.floors.describe()
                    ));
                }
                let split_coin_id = choice.statechain_id.clone();
                let split_amount = choice.piece_sats;
                let shape = choice.preflight.shape;
                drop(spendable);
                drop(record);

                // A laddered (TES-R) coin cannot be split as plain BTC — a prior owner's no-timelock
                // trigger could void the split [B1]. `ParentShape` therefore selects the executor as
                // well as the floor: a received CHILD splits at its own level, a root coin splits
                // in-ladder off its trigger, and only an un-laddered coin takes the plain split.
                match shape {
                    ParentShape::Child { .. } => {
                        let (piece_id, _change_id) = self
                            .child_in_ladder_pay(&split_coin_id, receiver_address, split_amount)
                            .await?;
                        inladder_piece = Some((piece_id, split_amount));
                        (ids, true)
                    }
                    ParentShape::Root { .. } => {
                        let (piece_id, _change_id, _batch) = self
                            .in_ladder_pay(
                                &split_coin_id,
                                receiver_address,
                                split_amount,
                                InLadderLatch::None,
                            )
                            .await?;
                        inladder_piece = Some((piece_id, split_amount));
                        // `ids` = the whole coins (still handed over below); the piece is already conveyed.
                        (ids, true)
                    }
                    ParentShape::Unladdered => {
                        let (piece_id, _change_id) =
                            self.split_coin(&split_coin_id, split_amount).await?;
                        ids.push(piece_id);
                        (ids, true)
                    }
                    // [CATS spine batch] A SPINE TIP takes the SPINE BATCH: `SP_{i+1}` over the tip's
                    // own funding outpoint `SP_i.out[K]`. It is deliberately its own arm and not
                    // folded into either neighbour — the two it could plausibly be folded into are
                    // the two that would lose the money (`split_coin` is the [B1]-unsafe plain
                    // split; `in_ladder_pay` would build over `X_m.out[0]`, an outpoint this coin's
                    // ladder gave up a batch ago).
                    //
                    // This is the arm that keeps a wallet spendable: after change 2 every partial
                    // payment leaves the sender holding a tip, so without it the SECOND payment out
                    // of a coin had no route at all.
                    ParentShape::SpineTip { .. } => {
                        let (piece_id, _change_id) = self
                            .spine_batch_pay(&split_coin_id, receiver_address, split_amount)
                            .await?;
                        inladder_piece = Some((piece_id, split_amount));
                        (ids, true)
                    }
                }
            }
        };

        // Hand each coin over (async key handover through the SE message relay).
        let mut coins = Vec::new();
        let record = self.record().await?;
        for id in to_send.drain(..) {
            let amount = record
                .coins
                .iter()
                .filter(|c| c.statechain_id.as_deref() == Some(id.as_str()))
                .filter_map(|c| c.amount)
                .next_back()
                .unwrap_or_default() as u64;
            // A received in-ladder CHILD takes its own onward route. It has no `tesr-` ladder and no
            // flat signed-once backup chain, so `transfer_sender::execute` would mis-handle it; `child_retransfer`
            // co-signs a fresh lower-CSV state over `ext_child.out[0]` paying the new recipient and
            // discloses the replaced state for the receiver's census.
            if let Some(cb) = mercuryrustlib::tesr::load_child(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &id,
            )
            .await?
            {
                let mut child_coin = record
                    .coins
                    .iter()
                    .find(|c| c.statechain_id.as_deref() == Some(id.as_str()) && c.duplicate_index == 0)
                    .cloned()
                    .ok_or_else(|| anyhow!("child coin {id} not found in the wallet"))?;
                mercuryrustlib::tesr::child_retransfer(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &mut child_coin,
                    &cb,
                    receiver_address,
                )
                .await?;
                self.set_coin_status(&id, CoinStatus::WITHDRAWN).await?;
                coins.push(TransferredCoin {
                    statechain_id: id,
                    amount_sats: amount,
                });
                continue;
            }
            // [CATS change 2] …and a SPINE TIP is REFUSED here, by name, rather than falling through
            // to the flat lane below.
            //
            // This arm exists because the producer made it reachable. A tip's funding `SP.out[K]` is
            // un-broadcast, exactly like a `ctesr-` child's — and `transfer_sender`'s classifier
            // knows that, so it LICENSES the flat conveyance (`PermanentLicence::FundingNotOnChain`)
            // instead of refusing it. That licence is right for what it was written for (it stops a
            // laddered coin being conveyed flat by accident) and wrong as a route: a flat conveyance
            // hands the receiver a signed-once backup chain over an outpoint that does not exist on
            // chain and never will, i.e. a coin with no working exit, with no error on either side.
            // The `ctesr-` arm above avoids that by having its own conveyance (`child_retransfer`).
            // The tip has none yet — handing one over is a key handover plus a `spinetip-`
            // conveyance, which is not built — so the honest answer is a refusal, not a route.
            //
            // The coin is untouched by this: it is still unilaterally exitable, and its cap already
            // pays this wallet's own key.
            if mercuryrustlib::tesr::load_spine_tip(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &id,
            )
            .await?
            .is_some()
            {
                return Err(anyhow!(
                    "coin {id} is a SPINE TIP (the change leg of an earlier in-ladder payment): \
                     handing it over whole is a spine-tip conveyance, whose builder is not landed. \
                     Refusing rather than conveying it on the flat lane, which would give the \
                     recipient a backup chain over an un-broadcast funding output — a coin with no \
                     exit. It can still be exited unilaterally by this wallet."
                ));
            }
            mercuryrustlib::transfer_sender::execute(
                &self.inner.cc,
                receiver_address,
                &self.inner.config.wallet_name,
                &id,
                None,
                false,
                None,
            )
            .await?;
            coins.push(TransferredCoin {
                statechain_id: id,
                amount_sats: amount,
            });
        }

        // The in-ladder split piece was conveyed directly to the recipient (not through the handover
        // loop); include it in the result so the caller sees the full amount sent.
        if let Some((piece_id, piece_amount)) = inladder_piece {
            coins.push(TransferredCoin {
                statechain_id: piece_id,
                amount_sats: piece_amount,
            });
        }

        Ok(TransferResult {
            receiver_address: receiver_address.to_string(),
            total_sats: coins.iter().map(|c| c.amount_sats).sum(),
            coins,
            used_split,
        })
    }

    /// Preview the all-in cost of sending `amount_sats` (B4 economics): the transfer's own
    /// split-reserve fee PLUS any on-chain renewal (re-anchor) this send triggers because a coin it
    /// uses is due for refresh — so the app shows the user ONE fee, like a payment, instead of a
    /// balance quietly shrinking in the background. `renewal_fee_sats` is 0 until a coin the send
    /// would use is at the renewal boundary, so the fee only rises when renewal is actually due.
    /// Stuck coins (value ≤ their renewal fee) are reported separately and excluded from `fundable`.
    /// Best-effort estimate; `transfer` applies the real charge.
    pub async fn quote_transfer(&self, amount_sats: u64) -> Result<crate::types::TransferQuote> {
        use electrum_client::ElectrumApi;
        let record = self.record().await?;
        // [B2] Both reads PROPAGATE. An empty carrier set would quote an RGB carrier as spendable
        // plain BTC (a coin `transfer` then refuses), and a defaulted 1.0 sat/vB rate would compute a
        // LOWER floor than the executor's — both are the quote disagreeing with the executor because
        // it could not read something, which is the bug this round exists to close.
        let carriers = self.unspendable_as_btc_outpoints().await?;
        let tip = self.inner.cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
        let margin = self.inner.config.auto_refresh_margin_blocks;
        let rate = backup_fee_rate(&self.inner.cc).await?;
        let refresh_fee = (BACKUP_TX_VBYTES as f64 * rate).ceil() as u64;

        // [B2] The SAME coin set `transfer` will plan over. A coin at/below its own renewal fee
        // cannot self-refresh and is reported for rescue-by-combine instead.
        let (usable, stuck_coins) = payment_coins(&record.coins, &carriers, refresh_fee);
        let amt = |c: &Coin| c.amount.unwrap_or_default() as u64;
        let usable_total: u64 = usable.iter().map(amt).sum();

        // Renewal is due if any usable coin is within the auto-refresh margin of its ladder floor.
        let renewal_due = usable
            .iter()
            .any(|c| c.locktime.map_or(false, |l| l.saturating_sub(tip) <= margin));
        let renewal_fee_sats = if renewal_due { refresh_fee } else { 0 };

        // [B2] The SAME planner `transfer` runs, over the SAME coins. `fundable` is therefore not an
        // estimate of what the executor would do — it IS what the executor will do. Round 1 raised
        // the floor here only, which left the executor planning lower; one planner from one source is
        // the fix, not a higher number on one side.
        let planned = self.plan_payment(&usable, amount_sats).await?;
        let (network_fee_sats, split_admissible, split_note) = match (&planned.plan, &planned.split) {
            (Plan::Exact(_), _) => (0, true, "paid from exact coins — no split needed".to_string()),
            (Plan::WithSplit { .. }, Some(choice)) => match &choice.preflight.admission {
                Ok(change) => (
                    choice.preflight.fee_sats,
                    true,
                    format!(
                        "includes the real {} cost {} sat (piece {} + change {change}, per-leg floor {})",
                        choice.preflight.shape.route(),
                        choice.preflight.fee_sats,
                        choice.piece_sats,
                    choice.preflight.floors.describe()
                    ),
                ),
                Err(why) => (
                    choice.preflight.fee_sats,
                    false,
                    format!(
                        "coin {} cannot mint a {}-sat piece on its {} route (per-leg floor {}): {why}",
                        choice.statechain_id,
                        choice.piece_sats,
                        choice.preflight.shape.route(),
                        choice.preflight.floors.describe()
                    ),
                ),
            },
            (Plan::WithSplit { .. }, None) => {
                (0, false, "planner proposed a split without naming a coin".to_string())
            }
            // No plan. If a candidate WAS named and refused, that refusal is the honest reason.
            (Plan::Insufficient { available }, Some(choice)) => (
                0,
                false,
                match &choice.preflight.admission {
                    Err(why) => format!(
                        "no coin can mint this amount as a viable split piece (available {available}); \
                         closest was {} on its {} route (per-leg floor {}): {why}",
                        choice.statechain_id,
                        choice.preflight.shape.route(),
                        choice.preflight.floors.describe()
                    ),
                    Ok(_) => format!(
                        "no coin can mint this amount as a viable split piece (available {available})"
                    ),
                },
            ),
            (Plan::Insufficient { available }, None) => (
                0,
                false,
                format!("no coin can mint this amount as a viable split piece (available {available})"),
            ),
        };

        let total_fee_sats = network_fee_sats + renewal_fee_sats;
        // `fundable` now means what the caller reads it as: the executor will accept this payment.
        let fundable =
            usable_total >= amount_sats.saturating_add(total_fee_sats) && split_admissible;
        let note = if !fundable {
            format!(
                "not fundable from non-stuck coins: usable {usable_total} vs need {} — {split_note}{}",
                amount_sats.saturating_add(total_fee_sats),
                if stuck_coins.is_empty() { String::new() } else { format!(" ({} stuck coin(s) — combine to rescue)", stuck_coins.len()) }
            )
        } else if renewal_due {
            format!("{split_note}; includes a renewal (re-anchor) fee — a coin this send uses is due for refresh")
        } else {
            format!("{split_note}; no renewal due for this send")
        };

        Ok(crate::types::TransferQuote {
            amount_sats,
            network_fee_sats,
            renewal_fee_sats,
            total_fee_sats,
            fundable,
            stuck_coins,
            note,
        })
    }

    /// Send sats to MANY recipients in one off-chain split (Spark's multi-receiver transfer): one
    /// SE-co-signed tx carves one piece per recipient (its exact amount) plus this wallet's
    /// change; each piece is handed over. Returns one `TransferResult` per recipient.
    ///
    /// Like `transfer`, this DISPATCHES ON THE PARENT'S SHAPE — a plain split of a laddered parent
    /// is unsafe ([B1], see `split_coin`), so a laddered parent takes the multi-child IN-LADDER
    /// route instead (`SP` descends from the trigger rather than racing it for `F`):
    ///   * a laddered ROOT coin  → [`Self::in_ladder_pay_many`] (one `SP` over `X_m.out[0]`);
    ///   * a received CHILD      → [`Self::child_in_ladder_pay_many`] (one `CSP` at the child's level);
    ///   * an un-laddered coin   → the plain N+1-output split (the un-laddered route; no trigger exists
    ///     over its funding outpoint, so nothing can race the split).
    /// Because `claim()` ladders every fresh confirmed root coin unconditionally, the in-ladder route
    /// is the DEFAULT — the plain split now serves only un-laddered sub-coins.
    pub async fn transfer_many(
        &self,
        recipients: &[(String, u64)],
    ) -> Result<Vec<TransferResult>> {
        if recipients.is_empty() {
            return Err(anyhow!("no recipients"));
        }
        let total: u64 = recipients.iter().map(|(_, a)| *a).sum();

        // Auto-refresh near-final coins before the parent is selected (see `transfer`).
        let _ = self.auto_refresh_before_spend().await?;
        let _guard = self.inner.wallet_lock.lock().await;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;
        let carriers = self.unspendable_as_btc_outpoints().await?;

        // Every piece and the change must clear the backup-fee floor (dust + each sub-coin's own
        // backup fee) so no output is a stranded coin. Reject up-front — before any parent is made
        // terminal — so a doomed batch never pins a carrier's spend budget. This is the floor that
        // holds on EVERY route; the in-ladder routes raise it per-parent below (`min_child_value`).
        let backup_rate = backup_fee_rate(&self.inner.cc).await?;
        let min_output = split_output_floors(backup_rate, ParentShape::Unladdered).piece;
        if let Some((_, amt)) = recipients.iter().find(|(_, a)| *a < min_output) {
            return Err(anyhow!(
                "recipient amount {amt} is below the minimum viable piece {min_output} (dust floor + backup fee) — it could not fund its own backup"
            ));
        }

        // Parent selection is SHAPE-AWARE: a coin's real capacity depends on its route (an in-ladder
        // split spends `X_m.out[0]`, which is the coin's value net of its already-committed tier fees,
        // and each child then funds its own two tiers), so a candidate cannot be judged on
        // `coin.amount` alone. Ladder state lives in the wallet db, so this must be async: collect +
        // sort first (smallest workable parent wins, as before), then probe each candidate's shape.
        let mut candidates: Vec<(u64, String)> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| !is_token_carrier(c, &carriers))
            .filter_map(|c| {
                c.statechain_id
                    .clone()
                    .map(|id| (c.amount.unwrap_or_default() as u64, id))
            })
            .collect();
        candidates.sort_by_key(|(amount, _)| *amount);
        drop(record);

        // N recipient pieces + one change output share the split.
        let n_out = recipients.len() + 1;
        let mut chosen: Option<(String, ManyRoute)> = None;
        let mut rejected: Vec<String> = Vec::new();
        for (parent_sats, id) in candidates {
            // [B2] ONE shape resolution decides the route AND the floors here too — the same
            // `parent_shape` / `split_output_floors` pair `transfer` and `quote_transfer` use, so the
            // batch lane cannot drift from the single-recipient lane. An in-ladder child gets its OWN
            // extension + state tier from `establish_child`, each burning `committed_fee +
            // P2A_VALUE`, and its final state output must still clear dust — so the in-ladder floor is
            // strictly above the backup-fee floor and BOTH bind.
            //
            // [V5] And the recipients are PIECES while the leftover is the CHANGE, so the two terms
            // below take the two floors. They are equal today; writing one name for both is how the
            // change leg's cheaper shape would silently become the payees' floor.
            let shape = self.parent_shape(&id).await?;
            let floors = split_output_floors(backup_rate, shape);
            match shape {
                ParentShape::Unladdered => {
                    let need = total + split_fee_reserve(parent_sats) + floors.change;
                    if parent_sats > need {
                        chosen = Some((id, ManyRoute::PlainSplit));
                        break;
                    }
                    rejected.push(format!(
                        "un-laddered {id} ({parent_sats} sat): too small — needs more than {need} sat \
                         (total {total} + fee reserve + a {}-sat change)",
                        floors.change
                    ));
                }
                // [CATS spine batch] A SPINE TIP is a candidate on the same terms as the two older
                // in-ladder shapes — `spine_batch_split` is N-ary, so one batch pays N recipients
                // and keeps the change as the next tip, exactly like `in_ladder_pay_many`. Sharing
                // the arm is what keeps the three capacities computed the same way; only the ROUTE
                // differs, and it is selected from the same `shape`.
                ParentShape::Child { .. } | ParentShape::Root { .. } | ParentShape::SpineTip { .. } => {
                    let cap = shape.split_total(n_out);
                    let fits = recipients.iter().all(|(_, a)| *a >= floors.piece)
                        && cap.is_some_and(|c| c >= total + floors.change);
                    if fits {
                        let route = match shape {
                            ParentShape::Child { .. } => ManyRoute::InLadderChild,
                            ParentShape::SpineTip { .. } => ManyRoute::SpineBatch,
                            _ => ManyRoute::InLadderRoot,
                        };
                        chosen = Some((id, route));
                        break;
                    }
                    rejected.push(format!(
                        "{} {id} ({parent_sats} sat): in-ladder capacity {}, per-leg floor {}",
                        shape.route(),
                        cap.map(|c| c.to_string())
                            .unwrap_or_else(|| "unavailable (committed fee no longer fits)".to_string()),
                        floors.describe()
                    ));
                }
            }
        }
        let (carrier_id, route) = chosen.ok_or_else(|| {
            anyhow!(
                "no confirmed coin can fund {total} sats to {} recipients + fee + non-dust change ({})",
                recipients.len(),
                if rejected.is_empty() {
                    "no candidate coins".to_string()
                } else {
                    rejected.join("; ")
                }
            )
        })?;

        // A laddered parent MUST NOT take the plain split [B1]; route it in-ladder instead.
        match route {
            ManyRoute::InLadderRoot => {
                let (piece_sids, _change) =
                    self.in_ladder_pay_many(&carrier_id, recipients).await?;
                return Ok(inladder_many_results(recipients, &piece_sids));
            }
            ManyRoute::InLadderChild => {
                let (piece_sids, _change) =
                    self.child_in_ladder_pay_many(&carrier_id, recipients).await?;
                return Ok(inladder_many_results(recipients, &piece_sids));
            }
            ManyRoute::SpineBatch => {
                let (piece_sids, _change) =
                    self.spine_batch_pay_many(&carrier_id, recipients).await?;
                return Ok(inladder_many_results(recipients, &piece_sids));
            }
            ManyRoute::PlainSplit => {}
        }

        let record = self.record().await?;
        let carrier = record
            .coins
            .iter()
            .find(|c| {
                c.statechain_id.as_deref() == Some(carrier_id.as_str())
                    && c.status == CoinStatus::CONFIRMED
                    && c.duplicate_index == 0
            })
            .cloned()
            .ok_or_else(|| anyhow!("selected parent {carrier_id} not found"))?;
        drop(record);
        let parent_sats = carrier.amount.unwrap_or_default() as u64;
        let fee_reserve = split_fee_reserve(parent_sats);
        let change_sats = parent_sats - total - fee_reserve;

        // One fresh slot per recipient piece + one change slot; build the N+1 plain split. All
        // N+1 slots are DERIVED from the carrier (one free SE voucher batch, one auth nonce) —
        // never the paid pool.
        let mut slot_tokens = self.take_derived_tokens(&carrier_id, recipients.len() + 1).await?;
        let mut outputs: Vec<(String, u64)> = Vec::with_capacity(recipients.len() + 1);
        let mut piece_addrs: Vec<String> = Vec::with_capacity(recipients.len());
        for (_, amount) in recipients {
            let tk = slot_tokens.remove(0);
            let addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &tk,
                u32::try_from(*amount)?,
            )
            .await?;
            outputs.push((addr.clone(), *amount));
            piece_addrs.push(addr);
        }
        let change_tk = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &change_tk,
            u32::try_from(change_sats)?,
        )
        .await?;
        outputs.push((change_addr.clone(), change_sats));

        // Terminal-guard the carrier (one split), then co-sign the un-broadcast split.
        mercuryrustlib::lightning_latch::set_spend_budget(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
            1,
        )
        .await?;
        let parent_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await
        .map(|v| v.len() as u32)
        .unwrap_or(0);
        let mut carrier_coin = carrier;
        let signed = self
            .sign_split_tx(&mut carrier_coin, &outputs, parent_backups + 1)
            .await?;
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&signed)?)?;
        let txid = tx.txid().to_string();

        // Register all sub-coins (plain split has no OP_RETURN, so vout i = i).
        let reg: Vec<(String, u32, u64)> = outputs
            .iter()
            .enumerate()
            .map(|(i, (addr, sats))| (addr.clone(), i as u32, *sats))
            .collect();
        let ids = self
            .register_split_subcoins_n(&carrier_id, &signed, &txid, &reg)
            .await?;

        // Hand each recipient its piece.
        let mut results = Vec::with_capacity(recipients.len());
        for (i, (recipient, amount)) in recipients.iter().enumerate() {
            let piece_id = ids[i].clone();
            mercuryrustlib::transfer_sender::execute(
                &self.inner.cc,
                recipient,
                &self.inner.config.wallet_name,
                &piece_id,
                None,
                false,
                None,
            )
            .await?;
            results.push(TransferResult {
                receiver_address: recipient.clone(),
                total_sats: *amount,
                coins: vec![TransferredCoin { statechain_id: piece_id, amount_sats: *amount }],
                used_split: true,
            });
        }
        Ok(results)
    }

    /// Ensure this wallet holds a CONFIRMED coin of exactly `sats`, minting one via an
    /// off-chain split when needed. Returns its statechain id. (The amount-maker behind
    /// single-coin flows: Lightning swaps, latch transfers.)
    pub async fn ensure_exact_coin(&self, sats: u64) -> Result<String> {
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;
        let carriers = self.unspendable_as_btc_outpoints().await?;
        if let Some(c) = record.coins.iter().find(|c| {
            c.status == CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.amount.unwrap_or_default() as u64 == sats
                && !is_token_carrier(c, &carriers)
        }) {
            return Ok(c.statechain_id.clone().unwrap_or_default());
        }
        // Split the smallest non-token-carrier coin that can cover the piece + fee reserve.
        // [HF-3 / B1] ...and that is UN-LADDERED. `split_coin` refuses a laddered parent (a prior
        // owner's retained no-timelock trigger could void the split and destroy the payee's sub-coin),
        // so picking the smallest coin blindly would hard-fail whenever that coin happens to carry a
        // ladder — even with a perfectly splittable coin sitting in the wallet. Ladder state lives in
        // the wallet db, so this filter must be async: collect + sort first, then pick the smallest
        // splittable one.
        let mut candidates: Vec<(u64, String)> = record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .filter(|c| !is_token_carrier(c, &carriers))
            .filter(|c| {
                let a = c.amount.unwrap_or_default() as u64;
                a > sats + split_fee_reserve(a)
            })
            .filter_map(|c| c.statechain_id.clone().map(|id| (c.amount.unwrap_or_default() as u64, id)))
            .collect();
        candidates.sort_by_key(|(amount, _)| *amount);
        drop(record);
        let mut chosen: Option<String> = None;
        for (_, id) in candidates {
            if mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, &id)
                .await?
                .is_none()
            {
                chosen = Some(id);
                break;
            }
        }
        let parent = chosen.ok_or_else(|| {
            anyhow!("no splittable coin large enough to mint {sats} sats (coins carrying a TES-R exit ladder cannot be split [B1] — use an exact amount or re-anchor)")
        })?;
        let (piece, _change) = self.split_coin(&parent, sats).await?;
        Ok(piece)
    }

    /// Split a confirmed coin into (`piece_sats`, remainder) **off-chain**: one SE-co-signed,
    /// un-broadcast transaction whose outputs are two fresh single-use statechain coins owned by
    /// this wallet. Returns (piece statechain_id, change statechain_id). Broadcasting the split tx
    /// later is the unilateral exit for the sub-coins.
    pub async fn split_coin(&self, statechain_id: &str, piece_sats: u64) -> Result<(String, String)> {
        let record = self.record().await?;
        let parent = record
            .coins
            .iter()
            .find(|c| {
                c.statechain_id.as_deref() == Some(statechain_id)
                    && c.status == CoinStatus::CONFIRMED
                    && c.duplicate_index == 0
            })
            .cloned()
            .ok_or_else(|| anyhow!("no confirmed coin with statechain id {statechain_id}"))?;
        // A plain-BTC split must never consume a token carrier (review H2): its RGB allocation
        // would be destroyed. Token moves go through the colored-split path instead.
        let carriers = self.unspendable_as_btc_outpoints().await?;
        if is_token_carrier(&parent, &carriers) {
            return Err(anyhow!(
                "coin {statechain_id} carries an RGB token allocation; splitting it as plain BTC would destroy the token — use a token transfer or pick a different coin"
            ));
        }
        // [B1] NEVER split a coin that carries a TES-R ladder. The trigger `T` has NO timelock
        // (`TRIGGER_SEQUENCE = 0xFFFF_FFFD`) and is fully co-signed at `establish`, so every prior owner
        // of a Model-A-conveyed coin retains a broadcastable `T` that spends `F` — and the split tx
        // spends the SAME `F`. A prior owner can therefore `unilateral_exit` the parent at any time,
        // consume `F`, and permanently kill this split, voiding the sub-coin its receiver paid for,
        // while their ladder pays them the full parent value. The race is rigged: `T` is v3/TRUC with a
        // P2A anchor (fee-bumpable by anyone, forever) vs a v2 split tx with a frozen fee and no RBF
        // headroom. The receiver cannot detect the exposure: the ladder is not conveyed on the
        // un-laddered backup-chain route and the SE has never seen it, so their `terminal_parents` check
        // returns true and means nothing. NOTE the spend-budget does NOT protect here — it bounds FUTURE
        // co-signs, and `T` was co-signed long before `set_spend_budget` (this is why `transfer.rs`'s "the
        // branch cannot be double-spent even by a malicious sender" claim does not hold for laddered parents).
        // The real fix is the IN-LADDER split (PROTOCOL.md §5.4): a split as a STATE tier spending
        // `X_m.out[0]` is a DESCENDANT of `T`, not a rival for `F`, so a retained trigger has nothing to
        // race. Until that ships, refuse — a hard error beats silently voiding the receiver's coin.
        if mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, statechain_id)
            .await?
            .is_some()
        {
            return Err(anyhow!(
                "coin {statechain_id} has a TES-R exit ladder and cannot be split: a prior owner may hold a no-timelock trigger over its funding UTXO and could void the split, voiding the receiver's sub-coin [B1]. Use an exact-amount transfer, pick an un-laddered coin, or re-anchor this coin first."
            ));
        }
        let parent_sats = parent.amount.unwrap_or_default() as u64;
        // Admission guard (fee-reserve fit + backup-fee floor on both outputs) — rejects BEFORE
        // touching the parent. The floor is the dust limit PLUS each sub-coin's own backup fee at
        // the rate create_tx1 will use: a piece in [330, 330+backup_fee) would be a valid split
        // output whose backup is FeeTooLow, and admitting it here (then making the parent terminal)
        // would strand the parent to unilateral-exit-only. Guarding up-front keeps the parent
        // spendable on refusal.
        // [B2] ONE floor, from `split_output_floors` — the same call the quote's
        // `split_preflight_pure` makes for an `Unladdered` parent (the `tesr::load` above proved this
        // coin is un-laddered). Un-laddered sub-coins have identical shape, so `binding()` is the
        // honest reading of two equal legs, not a shortcut past a distinction.
        let min_output =
            split_output_floors(backup_fee_rate(&self.inner.cc).await?, ParentShape::Unladdered)
                .binding();
        let (change_sats, _fee_reserve) =
            split_amounts_floored(parent_sats, piece_sats, min_output)?;

        // Two fresh statechain slots owned by this wallet (SE handshake only — no on-chain tx).
        // Normal coins: sub-coin security is Mercury's decrementing-locktime scheme, with the
        // split tx as the shared exit branch below the parent's deposit backup. The slots are
        // DERIVED (they re-house this parent's value, adding no on-chain onboarding surface), so
        // their tokens are free SE-minted vouchers against the parent — never the paid pool.
        let mut slot_tokens = self.take_derived_tokens(statechain_id, 2).await?;
        let token_a = slot_tokens.remove(0);
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(piece_sats)?,
        )
        .await?;
        let token_b = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_b,
            u32::try_from(change_sats)?,
        )
        .await?;

        // Build + blind-MuSig2 co-sign the un-broadcast split tx (plain BTC: no coloring step).
        // The split IS the child's exit branch and is now locktime-FREE (INV-4 / review H5), so it
        // is unconditionally broadcastable and always matures below the parent's deposit-anchored
        // backup — winning the exit race regardless of tip. `qt_backup_tx` no longer sets the split
        // locktime (it did, tip-relative, which could invert the race); kept only for signature
        // shape / withdrawal reuse.
        let parent_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await
        .map(|v| v.len() as u32)
        .unwrap_or(0);
        // Make the parent TERMINAL at the SE before co-signing the split: exactly one more
        // co-signature is allowed (the split itself), so no later withdraw/transfer/backup of the
        // parent can be signed.
        //
        // [HF-5 / B1] SCOPE — this does NOT mean "the branch cannot be double-spent even by a malicious
        // sender" (the claim that stood here). That holds only under the pre-TES-R premise that EVERY spend
        // of the parent's funding `F` needs a FRESH SE co-signature. A budget bounds FUTURE co-signs; it
        // cannot RETRACT one already issued. A laddered parent's trigger `T` was co-signed back at `establish`
        // — long before this call — carries no timelock, and spends `F` directly, so a retained `T`
        // double-spends the branch regardless of any budget set here. That is B1. Splitting a laddered
        // coin is therefore refused up-front in `split_coin`; the real fix is the in-ladder split
        // (PROTOCOL.md §5.4), where the split is a state tier DESCENDING from `T` rather than a rival for
        // `F`. Keep this comment honest: the budget is a co-sign bound, not a spend bound.
        mercuryrustlib::lightning_latch::set_spend_budget(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
            1,
        )
        .await?;
        let mut parent = parent;
        let signed = self
            .sign_split_tx(
                &mut parent,
                &[(piece_addr.clone(), piece_sats), (change_addr.clone(), change_sats)],
                parent_backups + 1,
            )
            .await?;
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&signed)?)?;
        let txid = tx.txid().to_string();

        // Register both sub-coins (coin records + backups + shared branch).
        let (piece_id, change_id) = self
            .register_split_subcoins(
                statechain_id,
                &signed,
                &txid,
                &[
                    (piece_addr.clone(), 0, piece_sats),
                    (change_addr.clone(), 1, change_sats),
                ],
            )
            .await?;
        Ok((piece_id, change_id))
    }

    /// **In-ladder split payment of a laddered (TES-R) coin** (the B1-safe replacement for the refused
    /// `split_coin` path). The split IS the payment: `SP` is a STATE tier spending `X_m.out[0]` (a
    /// DESCENDANT of the trigger, never a rival for `F`), with two children — the PIECE, whose headless
    /// ladder pays the recipient (Model A), and the CHANGE, paying this wallet back. The piece child
    /// bundle is conveyed to `recipient_address` via the mailbox (`convey_child_bundle`); the recipient's
    /// claim() adopts it with `verify_child_bundle`. The change child bundle is persisted locally as an
    /// exitable claim. Returns `(piece_child_sid, change_child_sid, latch_batch_id)`.
    ///
    /// Value is conserved by the split builder: `piece + change == tier_out_total(X_m.out[0], 2)`, so
    /// `change` is derived (not free) and `piece` must leave room for a viable change output.
    ///
    /// A non-`None` `latch` makes this a **non-exact Lightning swap** (LIGHTNING.md §2b): the PIECE
    /// child is registered under a latch bound to the invoice and conveyed batch-locked, so the paying
    /// party can identify + census it (`verify_conveyed_child`) before releasing the other leg. The
    /// piece then sits at the same operator-trust bar as the exact-lane `S'` (bounded to the piece; the
    /// NON-EXACT payment out of a RECEIVED child (a child-level in-ladder split). The child's state is
    /// replaced by a split state paying two grandchildren — the PIECE to `recipient_address` and the
    /// CHANGE back to us — and the child becomes an intermediate segment in each grandchild's bundle.
    /// Returns `(piece_sid, change_sid)`.
    pub async fn child_in_ladder_pay(
        &self,
        child_statechain_id: &str,
        recipient_address: &str,
        piece_sats: u64,
    ) -> Result<(String, String)> {
        let network = self.inner.config.network.to_string();
        let cb = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            child_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("coin {child_statechain_id} is not a received split child"))?;

        // [B2] ONE floor and ONE admission rule, both shared with the quote's `split_preflight_pure`: a
        // grandchild also funds its own extension + state tier before it can clear dust, and the
        // child is terminalized BEFORE those are built.
        let shape = ParentShape::Child {
            fee_rate: cb.parent.fee_rate,
            split_source_value: cb.child_extension.out_value,
        };
        // piece + change == the child's split total (ext_child.out[0] − committed fee for 2).
        let total = shape
            .split_total(2)
            .ok_or_else(|| anyhow!("committed fee too high to split this child into two"))?;
        let floors = split_output_floors(backup_fee_rate(&self.inner.cc).await?, shape);
        let change_sats = inladder_amounts_floored(total, piece_sats, floors)?;

        let mut slot_tokens = self.take_derived_tokens(child_statechain_id, 2).await?;
        let piece_gc = self.create_child_slot(&slot_tokens.remove(0), piece_sats).await?;
        let change_gc = self.create_child_slot(&slot_tokens.remove(0), change_sats).await?;
        let payee = mercurylib::tesr::payee_address(recipient_address, &network)?;
        let self_change_backup =
            mercurylib::transaction::get_user_backup_address(&change_gc, network.clone())?;

        let mut child_coin = self
            .record()
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(child_statechain_id) && c.duplicate_index == 0)
            .cloned()
            .ok_or_else(|| anyhow!("child coin {child_statechain_id} not found"))?;

        let mut grandchildren = vec![
            (piece_gc.clone(), payee, piece_sats),
            (change_gc.clone(), self_change_backup, change_sats),
        ];
        let bundles = mercuryrustlib::tesr::child_in_ladder_split(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut child_coin,
            &cb,
            &mut grandchildren,
        )
        .await?;

        let piece_sid = piece_gc.statechain_id.clone().unwrap_or_default();
        let change_sid = change_gc.statechain_id.clone().unwrap_or_default();
        // Convey the piece grandchild (with the standard handover) and keep the change locally.
        mercuryrustlib::tesr::convey_child_bundle(
            &self.inner.cc,
            recipient_address,
            &grandchildren[0].0,
            &bundles[0],
            None,
        )
        .await?;
        mercuryrustlib::tesr::persist_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &bundles[1],
        )
        .await?;

        // Book: the spent child is gone, the piece left, the change is a fresh confirmed claim.
        {
            let mut record = self.record().await?;
            for coin in record.coins.iter_mut() {
                match coin.statechain_id.as_deref() {
                    Some(sid) if sid == child_statechain_id => coin.status = CoinStatus::WITHDRAWN,
                    Some(sid) if sid == piece_sid => coin.status = CoinStatus::WITHDRAWN,
                    Some(sid) if sid == change_sid => {
                        coin.status = CoinStatus::CONFIRMED;
                        coin.amount = Some(change_sats as u32);
                    }
                    _ => {}
                }
            }
            self.save_record(&record).await?;
        }

        // [P0-3] The split's write-ahead journal record stays OPEN until this point: the co-signed
        // material only becomes recoverable-by-restart once the bundles are on disk and conveyed.
        mercuryrustlib::tesr::journal_commit(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mercuryrustlib::tesr::split_op_id(&bundles[0]),
        )
        .await?;
        Ok((piece_sid, change_sid))
    }

    /// MULTI-RECIPIENT payment out of a RECEIVED CHILD — the `transfer_many` route for a child parent.
    /// One split state `CSP` over `ext_child.out[0]` carves N recipient grandchildren (exact amounts)
    /// plus this wallet's change; each recipient's bundle is conveyed to its mailbox with the standard
    /// key handover. Returns `(piece_sids in recipient order, change_sid)`.
    ///
    /// This is the N-way generalisation of [`Self::child_in_ladder_pay`] and exists for the same
    /// reason: a plain split of a laddered parent is [B1]-unsafe, so `transfer_many` must never take
    /// it. `child_in_ladder_split` already accepts N children and enforces value conservation
    /// (`Σ children == tier_out_total(ext_child.out[0], N+1)`), so the change is DERIVED, not free.
    pub async fn child_in_ladder_pay_many(
        &self,
        child_statechain_id: &str,
        recipients: &[(String, u64)],
    ) -> Result<(Vec<String>, String)> {
        let network = self.inner.config.network.to_string();
        let n = recipients.len();
        if n == 0 {
            return Err(anyhow!("no recipients"));
        }
        let cb = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            child_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("coin {child_statechain_id} is not a received split child"))?;

        // Σpieces + change == the child's split total (ext_child.out[0] − committed fee for N+1).
        let shape = ParentShape::Child {
            fee_rate: cb.parent.fee_rate,
            split_source_value: cb.child_extension.out_value,
        };
        let total = shape
            .split_total(n + 1)
            .ok_or_else(|| anyhow!("committed fee too high to split this child into {} outputs", n + 1))?;
        let pieces_total: u64 = recipients.iter().map(|(_, a)| *a).sum();
        if pieces_total >= total {
            return Err(anyhow!(
                "payments totalling {pieces_total} sat leave no change: splitting this child {n} ways can pay at most {} sat",
                total.saturating_sub(1)
            ));
        }
        let change_sats = total - pieces_total;
        // [B2] The same floor, from the same source, as the single-recipient child split (see the
        // guard in `child_in_ladder_pay`), applied to EVERY output: refuse up-front, before the child
        // is terminalized.
        // [V5] Per-leg: the recipients are PIECES, the leftover is the CHANGE.
        let floors = split_output_floors(backup_fee_rate(&self.inner.cc).await?, shape);
        if let Some((_, amt)) = recipients.iter().find(|(_, a)| *a < floors.piece) {
            return Err(anyhow!(
                "recipient amount {amt} is below the in-ladder piece minimum {} sat (each grandchild funds its own extension + state tier, then must clear the {DUST_LIMIT}-sat dust floor)",
                floors.piece
            ));
        }
        if change_sats < floors.change {
            return Err(anyhow!(
                "change {change_sats} is below the in-ladder change minimum {} sat ({}) — lower the payment total or use a larger coin",
                floors.change,
                floors.change_note()
            ));
        }

        let mut slot_tokens = self.take_derived_tokens(child_statechain_id, n + 1).await?;
        let mut grandchildren: Vec<(Coin, String, u64)> = Vec::with_capacity(n + 1);
        for (address, amount) in recipients {
            let slot = self.create_child_slot(&slot_tokens.remove(0), *amount).await?;
            grandchildren.push((slot, mercurylib::tesr::payee_address(address, &network)?, *amount));
        }
        let change_gc = self.create_child_slot(&slot_tokens.remove(0), change_sats).await?;
        let self_change_backup =
            mercurylib::transaction::get_user_backup_address(&change_gc, network.clone())?;
        grandchildren.push((change_gc.clone(), self_change_backup, change_sats));

        let mut child_coin = self
            .record()
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(child_statechain_id) && c.duplicate_index == 0)
            .cloned()
            .ok_or_else(|| anyhow!("child coin {child_statechain_id} not found"))?;

        let bundles = mercuryrustlib::tesr::child_in_ladder_split(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut child_coin,
            &cb,
            &mut grandchildren,
        )
        .await?;

        // Persist the change claim FIRST, then convey. Ordering matters more here than in the
        // single-recipient case: `CSP` and every tier are already co-signed and the child is terminal,
        // so those signatures can never be regenerated — and a conveyance that fails on recipient k
        // would abort the call with the change bundle unwritten, destroying the only record of our own
        // remaining value. Persisting first makes the change recoverable from any mid-batch failure.
        mercuryrustlib::tesr::persist_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &bundles[n],
        )
        .await?;
        // Convey each recipient's grandchild (standard handover).
        for (j, (address, _)) in recipients.iter().enumerate() {
            mercuryrustlib::tesr::convey_child_bundle(
                &self.inner.cc,
                address,
                &grandchildren[j].0,
                &bundles[j],
                None,
            )
            .await?;
        }

        let piece_sids: Vec<String> = grandchildren[..n]
            .iter()
            .map(|(c, _, _)| c.statechain_id.clone().unwrap_or_default())
            .collect();
        let change_sid = change_gc.statechain_id.clone().unwrap_or_default();
        // Book: the spent child is gone, every piece left, the change is a fresh confirmed claim.
        {
            let mut record = self.record().await?;
            for coin in record.coins.iter_mut() {
                match coin.statechain_id.as_deref() {
                    Some(sid) if sid == child_statechain_id => coin.status = CoinStatus::WITHDRAWN,
                    Some(sid) if piece_sids.iter().any(|p| p == sid) => {
                        coin.status = CoinStatus::WITHDRAWN
                    }
                    Some(sid) if sid == change_sid => {
                        coin.status = CoinStatus::CONFIRMED;
                        coin.amount = Some(u32::try_from(change_sats)?);
                    }
                    _ => {}
                }
            }
            self.save_record(&record).await?;
        }
        // History: the grandchildren are funded by `CSP.out[j]`, and `CSP` is the state of the segment
        // `child_in_ladder_split` just appended (the spent child as an ancestor). Best-effort by
        // construction — the payment itself is already committed and conveyed at this point.
        if let Some(seg) = bundles[n].ancestors.last() {
            let csp_txid = signed_tier_txid(&seg.state.signed_tx)?;
            self.record_conveyed_pieces(&csp_txid, recipients).await?;
        }
        // [P0-3] Close the split's write-ahead journal: the bundles are on disk and conveyed, so a
        // restart has nothing left to replay.
        mercuryrustlib::tesr::journal_commit(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mercuryrustlib::tesr::split_op_id(&bundles[0]),
        )
        .await?;
        Ok((piece_sids, change_sid))
    }

    /// change is self-owned and trustless; no double-recovery, both share `X_m.out[0]`). The 3rd tuple
    /// element is the latch `(batch_id, payment_hash)` (`None` for a plain in-ladder payment):
    ///   * [`InLadderLatch::External`] — PAY: latch to a known merchant-invoice hash.
    ///   * [`InLadderLatch::ClassicMinted`] — RECEIVE: the SE mints the preimage; the returned hash goes
    ///     into the SSP's HODL invoice and `settle_receive` retrieves the preimage from the SE.
    pub async fn in_ladder_pay(
        &self,
        parent_statechain_id: &str,
        recipient_address: &str,
        piece_sats: u64,
        latch: InLadderLatch<'_>,
    ) -> Result<(String, String, Option<(String, String)>)> {
        let network = self.inner.config.network.to_string();
        let bundle = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            parent_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("coin {parent_statechain_id} has no TES-R ladder to split in-ladder"))?;

        // piece + change == the split total (X_m.out[0] − committed fee for 2 children).
        let x_m = bundle.current().extension.clone();
        let shape =
            ParentShape::Root { fee_rate: bundle.fee_rate, split_source_value: x_m.out_value };
        let total = shape
            .split_total(2)
            .ok_or_else(|| anyhow!("committed fee too high to split this coin into two"))?;
        // [B2] Admission guard — ONE floor (`split_output_floor`) and ONE rule
        // (`inladder_amounts_floored`), both shared with the quote's `split_preflight_pure`. Two floors
        // apply and the LARGER binds:
        //  * the backup-fee floor (dust + the sub-coin's own backup fee), as for a plain un-laddered
        //    sub-coin; and
        //  * `min_child_value` — an in-ladder child gets its OWN two-tier ladder (extension + state)
        //    from `establish_child`, each tier burning `committed_fee + P2A_VALUE`, and its final
        //    state output must still clear dust.
        // The second floor is the load-bearing one here: `establish_child` runs AFTER the parent's
        // spend budget is consumed and `SP` is co-signed, so admitting a child below it terminalizes
        // the parent and THEN fails with FeeTooHigh, stranding the parent to unilateral-exit-only.
        // Refusing up-front keeps the parent fully spendable (same discipline as `split_coin`).
        let floors = split_output_floors(backup_fee_rate(&self.inner.cc).await?, shape);
        let change_sats = inladder_amounts_floored(total, piece_sats, floors)?;

        // Two fresh SE-registered child slots (DERIVED — free vouchers against the parent's value).
        let mut slot_tokens = self.take_derived_tokens(parent_statechain_id, 2).await?;
        let piece_child = self.create_child_slot(&slot_tokens.remove(0), piece_sats).await?;
        let change_child = self.create_child_slot(&slot_tokens.remove(0), change_sats).await?;

        // Model A payees: piece -> recipient's exit key; change -> this change slot's own key.
        let payee = mercurylib::tesr::payee_address(recipient_address, &network)?;
        let self_change_backup =
            mercurylib::transaction::get_user_backup_address(&change_child, network.clone())?;

        let mut parent = self
            .record()
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(parent_statechain_id) && c.duplicate_index == 0)
            .cloned()
            .ok_or_else(|| anyhow!("parent coin {parent_statechain_id} not found"))?;

        let mut children = vec![
            (piece_child.clone(), payee, piece_sats),
            (change_child.clone(), self_change_backup, change_sats),
        ];
        // [CATS change 2] The change leg is LAST and is built as a one-cap SPINE TIP — one rung, not
        // two, which is exactly the shape `split_output_floors` just admitted it at.
        let split = mercuryrustlib::tesr::in_ladder_split(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut parent,
            &bundle,
            &mut children,
            mercuryrustlib::tesr::ChangeLeg::LastIsTip,
        )
        .await?;
        let bundles = &split.pieces;
        let piece_bundle = &bundles[0];
        let change_tip = split
            .tip
            .as_ref()
            .ok_or_else(|| anyhow!("the in-ladder split produced no change leg"))?;

        // [non-exact LN latch, LIGHTNING.md §2b] If a payment hash is given, register the external-hash
        // latch on the PIECE child (so the SSP finds it via the batch and censuses it pre-pay) BEFORE
        // conveying, and book the piece IN_TRANSFER so `create_external_hash_latch`'s CONFIRMED|IN_TRANSFER
        // status check passes and the piece stays a valid latched-pending coin (not WITHDRAWN) the SSP
        // adopts once it pays. Latch first, then convey: convey's `get_new_x1` runs `insert_new_transfer`,
        // which marks the child mailbox row a lightning latch only if the latch row already exists.
        let piece_sid = piece_child.statechain_id.clone().unwrap_or_default();
        let latch: Option<(String, String)> = match latch {
            InLadderLatch::None => None,
            InLadderLatch::External(hash) => {
                self.set_coin_status(&piece_sid, CoinStatus::IN_TRANSFER).await?;
                let batch_id = mercuryrustlib::lightning_latch::create_external_hash_latch(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &piece_sid,
                    hash,
                )
                .await?;
                Some((batch_id, hash.to_string()))
            }
            InLadderLatch::ClassicMinted => {
                // The classic SE latch (`create_pre_image`) sanity-checks `coin.locktime`; a fresh
                // un-broadcast child slot has none, and the child exits via CSV (not an absolute
                // locktime), so stamp a placeholder (inherited from the parent) purely to pass it.
                let placeholder_lock = parent.locktime.or(Some(0));
                {
                    let mut record = self.record().await?;
                    for coin in record.coins.iter_mut() {
                        if coin.statechain_id.as_deref() == Some(&piece_sid) {
                            coin.status = CoinStatus::IN_TRANSFER;
                            coin.locktime = placeholder_lock;
                        }
                    }
                    self.save_record(&record).await?;
                }
                let pre = mercuryrustlib::lightning_latch::create_pre_image(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &piece_sid,
                )
                .await?;
                Some((pre.batch_id, pre.hash))
            }
        };
        let latch_batch: Option<String> = latch.as_ref().map(|(b, _)| b.clone());

        // [LN carve-out] A LATCHED piece is deliberately left unclaimed until a Lightning preimage
        // lands — which is exactly the situation the temporary pending-transfer lock does NOT cover
        // (it expires with the batch window, and the receiver cannot complete the handover until the
        // latch releases). So for the latched lane, and only there, keep the child TERMINAL: the SE
        // will co-sign nothing further over it, closing the post-expiry rival window permanently.
        // Plain in-ladder payments rely on the pending lock + the receiver's prompt handover instead.
        // Placement matters: `set_spend_budget` authenticates with the child's auth key
        // (`fresh_auth`), so it must run while WE still own that key — i.e. before the receiver's
        // key update, and therefore before conveyance.
        if latch_batch.is_some() {
            mercuryrustlib::lightning_latch::set_spend_budget(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &piece_sid,
                0,
            )
            .await?;
        }

        // Convey the piece child to the recipient's mailbox (auth = the piece slot we own).
        mercuryrustlib::tesr::convey_child_bundle(
            &self.inner.cc,
            recipient_address,
            &children[0].0,
            piece_bundle,
            latch_batch.clone(),
        )
        .await?;

        // Persist the change as an exitable self-claim; book both slots + the spent parent.
        //
        // [CATS change 2] `persist_spine_tip`, not `persist_child`: the change is a one-cap tip and a
        // `ctesr-` row would route it to leaf handling. That call runs `SpineTipBundle::validate()`
        // as its precondition, so the record is checked against its own signed cap before it becomes
        // this wallet's source of truth for the change.
        mercuryrustlib::tesr::persist_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            change_tip,
        )
        .await?;
        let sp_txid = signed_tier_txid(&change_tip.parent.current().state.signed_tx)?;
        // A latched piece stays IN_TRANSFER (conveyed but pending payment; the SSP adopts on pay); a
        // plain conveyed piece is WITHDRAWN (given away outright).
        let piece_status = if latch_batch.is_some() {
            CoinStatus::IN_TRANSFER
        } else {
            CoinStatus::WITHDRAWN
        };
        self.book_inladder_split_coins(
            parent_statechain_id,
            &sp_txid,
            &piece_sid,
            change_child.statechain_id.as_deref().unwrap_or_default(),
            change_sats,
            piece_status,
        )
        .await?;
        // [P0-3] Close the split's write-ahead journal: the bundles are on disk and conveyed, so a
        // restart has nothing left to replay.
        mercuryrustlib::tesr::journal_commit(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mercuryrustlib::tesr::split_op_id(&bundles[0]),
        )
        .await?;

        Ok((piece_sid, change_child.statechain_id.clone().unwrap_or_default(), latch))
    }

    /// MULTI-RECIPIENT in-ladder split payment — the `transfer_many` route for a laddered ROOT coin,
    /// and the [B1] fix for it. ONE split state `SP` over `X_m.out[0]` carves N recipient children
    /// (exact amounts) plus this wallet's change; each recipient's child bundle is conveyed to its
    /// mailbox with the standard key handover, so every piece is a first-class coin the recipient
    /// adopts at claim. Returns `(piece_sids in recipient order, change_sid)`.
    ///
    /// Why this and not the plain N+1 split: `SP` spends `X_m.out[0]`, i.e. it DESCENDS from the
    /// trigger `T` instead of racing it for the funding outpoint `F`. A prior owner's retained,
    /// un-timelocked `T` therefore cannot consume `F` and void the split, which is exactly what would
    /// destroy every recipient's piece on the plain lane (see the [B1] refusal in [`Self::split_coin`]).
    ///
    /// Value is conserved by the split builder: `Σ pieces + change == tier_out_total(X_m.out[0], N+1)`,
    /// so the change is derived — the caller states the payments, not the change.
    pub async fn in_ladder_pay_many(
        &self,
        parent_statechain_id: &str,
        recipients: &[(String, u64)],
    ) -> Result<(Vec<String>, String)> {
        let network = self.inner.config.network.to_string();
        let n = recipients.len();
        if n == 0 {
            return Err(anyhow!("no recipients"));
        }
        let bundle = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            parent_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("coin {parent_statechain_id} has no TES-R ladder to split in-ladder"))?;

        // Σpieces + change == the split total (X_m.out[0] − the committed fee for N+1 outputs; each
        // extra payload output costs P2TR_OUT_VBYTES, so the fee scales with the recipient count).
        let x_m = bundle.current().extension.clone();
        let shape =
            ParentShape::Root { fee_rate: bundle.fee_rate, split_source_value: x_m.out_value };
        let total = shape
            .split_total(n + 1)
            .ok_or_else(|| anyhow!("committed fee too high to split this coin into {} outputs", n + 1))?;
        let pieces_total: u64 = recipients.iter().map(|(_, a)| *a).sum();
        if pieces_total >= total {
            return Err(anyhow!(
                "payments totalling {pieces_total} sat leave no change: an in-ladder split of this coin into {} outputs can pay at most {} sat",
                n + 1,
                total.saturating_sub(1)
            ));
        }
        let change_sats = total - pieces_total;
        // [B2] The same floor, from the same source, as the single-recipient guard in
        // `in_ladder_pay`, applied to EVERY output. Refusing up-front is load-bearing:
        // `establish_child` runs AFTER the parent's spend budget is consumed and `SP` is co-signed,
        // so an output admitted below the floor terminalizes the parent and THEN fails, stranding it
        // to unilateral-exit-only.
        // [V5] Per-leg: the recipients are PIECES, the leftover is the CHANGE.
        let floors = split_output_floors(backup_fee_rate(&self.inner.cc).await?, shape);
        if let Some((_, amt)) = recipients.iter().find(|(_, a)| *a < floors.piece) {
            return Err(anyhow!(
                "recipient amount {amt} is below the in-ladder piece minimum {} sat (each child funds its own extension + state tier at {} sat/vB, then must clear the {DUST_LIMIT}-sat dust floor)",
                floors.piece,
                bundle.fee_rate
            ));
        }
        if change_sats < floors.change {
            return Err(anyhow!(
                "change {change_sats} is below the in-ladder change minimum {} sat ({}) — lower the payment total or use a larger coin",
                floors.change,
                floors.change_note()
            ));
        }

        // N+1 fresh SE-registered child slots (DERIVED — one free voucher batch against the parent).
        let mut slot_tokens = self.take_derived_tokens(parent_statechain_id, n + 1).await?;
        let mut children: Vec<(Coin, String, u64)> = Vec::with_capacity(n + 1);
        for (address, amount) in recipients {
            let slot = self.create_child_slot(&slot_tokens.remove(0), *amount).await?;
            // Model A payee: the recipient's exit key (from their statechain address).
            children.push((slot, mercurylib::tesr::payee_address(address, &network)?, *amount));
        }
        let change_child = self.create_child_slot(&slot_tokens.remove(0), change_sats).await?;
        let self_change_backup =
            mercurylib::transaction::get_user_backup_address(&change_child, network.clone())?;
        children.push((change_child.clone(), self_change_backup, change_sats));

        let mut parent = self
            .record()
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(parent_statechain_id) && c.duplicate_index == 0)
            .cloned()
            .ok_or_else(|| anyhow!("parent coin {parent_statechain_id} not found"))?;

        // [CATS change 2] The change leg is LAST (pushed after every recipient above) and becomes the
        // one-cap spine tip; the N recipients' pieces are unchanged two-tier children.
        let split = mercuryrustlib::tesr::in_ladder_split(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut parent,
            &bundle,
            &mut children,
            mercuryrustlib::tesr::ChangeLeg::LastIsTip,
        )
        .await?;
        let bundles = &split.pieces;
        let change_tip = split
            .tip
            .as_ref()
            .ok_or_else(|| anyhow!("the in-ladder split produced no change leg"))?;

        // Persist the change claim FIRST, then convey. Ordering matters more here than in the
        // single-recipient case: `SP` and every tier are already co-signed and the parent is terminal,
        // so those signatures can never be regenerated — and a conveyance that fails on recipient k
        // would abort the call with the change record unwritten, destroying the only record of our own
        // remaining value. Persisting first makes the change recoverable from any mid-batch failure.
        mercuryrustlib::tesr::persist_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            change_tip,
        )
        .await?;
        // Convey each recipient's child (auth = the slot we still own).
        // No LN latch on this lane: a batch payment is never a Lightning swap leg, so every piece
        // relies on the pending-transfer lock + the receiver's prompt handover, exactly like the
        // single-recipient plain `in_ladder_pay` (see its [F1] note).
        for (j, (address, _)) in recipients.iter().enumerate() {
            mercuryrustlib::tesr::convey_child_bundle(
                &self.inner.cc,
                address,
                &children[j].0,
                &bundles[j],
                None,
            )
            .await?;
        }

        let piece_sids: Vec<String> = children[..n]
            .iter()
            .map(|(c, _, _)| c.statechain_id.clone().unwrap_or_default())
            .collect();
        let change_sid = change_child.statechain_id.clone().unwrap_or_default();
        let sp_txid = signed_tier_txid(&change_tip.parent.current().state.signed_tx)?;
        self.book_inladder_split_coins_n(
            parent_statechain_id,
            &sp_txid,
            &piece_sids,
            CoinStatus::WITHDRAWN,
            &change_sid,
            change_sats,
            n as u32,
        )
        .await?;
        self.record_conveyed_pieces(&sp_txid, recipients).await?;
        // [P0-3] Close the split's write-ahead journal: the bundles are on disk and conveyed, so a
        // restart has nothing left to replay.
        mercuryrustlib::tesr::journal_commit(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mercuryrustlib::tesr::split_op_id(&bundles[0]),
        )
        .await?;
        Ok((piece_sids, change_sid))
    }

    /// **[CATS spine batch] THE SPINE BATCH — a further partial payment out of a coin whose change is
    /// already a one-cap tip.** Returns `(piece_sid, next_tip_sid)`.
    ///
    /// This is what makes payment 2 out of a coin possible at all. After change 2 the change leg of
    /// every in-ladder payment is a [`mercuryrustlib::tesr::SpineTipBundle`], which has no `tesr-`
    /// row (so `in_ladder_pay` cannot load it) and no `ctesr-` row (so `child_in_ladder_pay` cannot
    /// either). The batch spends the tip's own funding outpoint `SP_i.out[K]` — see
    /// [`mercuryrustlib::tesr::spine_batch_split`] for why `SP_{i+1}` sits at `SPINE_CSV` while the
    /// new cap sits at `state_csv(0)`.
    ///
    /// Deliberately WITHOUT an `InLadderLatch` parameter, unlike [`Self::in_ladder_pay`]. A Lightning
    /// latch over a spine batch is a design question this change does not answer (the SSP censuses a
    /// piece pre-pay, and a spine ancestor changes what that census reads), and a parameter that
    /// silently accepted `External(hash)` would look like support for it. The LN lanes select their
    /// own carrier and still route to `in_ladder_pay`.
    pub async fn spine_batch_pay(
        &self,
        tip_statechain_id: &str,
        recipient_address: &str,
        piece_sats: u64,
    ) -> Result<(String, String)> {
        let (piece_sids, change_sid) = self
            .spine_batch_pay_inner(
                tip_statechain_id,
                &[(recipient_address.to_string(), piece_sats)],
                None,
            )
            .await?;
        Ok((piece_sids.into_iter().next().unwrap_or_default(), change_sid))
    }

    /// MULTI-RECIPIENT spine batch — the `transfer_many` route for a coin that is already a tip, and
    /// the sibling of [`Self::in_ladder_pay_many`]. ONE `SP_{i+1}` carves N recipient pieces (exact
    /// amounts) plus this wallet's change, which becomes the NEXT tip.
    pub async fn spine_batch_pay_many(
        &self,
        tip_statechain_id: &str,
        recipients: &[(String, u64)],
    ) -> Result<(Vec<String>, String)> {
        self.spine_batch_pay_inner(tip_statechain_id, recipients, Some(recipients)).await
    }

    /// The one body both spine-batch entry points share, so the single- and multi-recipient lanes
    /// cannot drift the way `in_ladder_pay` and `in_ladder_pay_many` have (two copies of the floor
    /// check, two copies of the booking, one of them with a history row and one without).
    ///
    /// `history` is `Some(recipients)` when the caller wants a `get_transfers()` row per conveyed
    /// piece and `None` when it does not — the only real difference between the two lanes.
    async fn spine_batch_pay_inner(
        &self,
        tip_statechain_id: &str,
        recipients: &[(String, u64)],
        history: Option<&[(String, u64)]>,
    ) -> Result<(Vec<String>, String)> {
        let network = self.inner.config.network.to_string();
        let n = recipients.len();
        if n == 0 {
            return Err(anyhow!("no recipients"));
        }
        let tip = mercuryrustlib::tesr::load_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            tip_statechain_id,
        )
        .await?
        .ok_or_else(|| {
            anyhow!("coin {tip_statechain_id} has no spine-tip record to build the next batch over")
        })?;

        // [B2] ONE shape, ONE pair of floors, ONE admission rule — the same three the quote's
        // `split_preflight_pure` uses, so `fundable: true` followed by a refusal stays inexpressible
        // on this lane too. `split_source_value` is the tip's `sp_out_value`: the batch spends
        // `SP_i.out[K]`, not the cap's output.
        let shape = ParentShape::SpineTip {
            fee_rate: tip.parent.fee_rate,
            split_source_value: tip.sp_out_value,
        };
        let total = shape.split_total(n + 1).ok_or_else(|| {
            anyhow!("committed fee too high to carve a spine batch of {} outputs", n + 1)
        })?;
        let pieces_total: u64 = recipients.iter().map(|(_, a)| *a).sum();
        let floors = split_output_floors(backup_fee_rate(&self.inner.cc).await?, shape);
        // Refusing up-front is load-bearing here for the same reason as on the two older lanes, and
        // the stakes are the same: `establish_child_journalled` runs AFTER the tip's spend budget is
        // consumed and `SP_{i+1}` is co-signed, so a leg admitted below its floor terminalizes the
        // tip and THEN fails — leaving the batch's own change unbuildable.
        let change_sats = if n == 1 {
            inladder_amounts_floored(total, pieces_total, floors)?
        } else {
            if pieces_total >= total {
                return Err(anyhow!(
                    "payments totalling {pieces_total} sat leave no change: a spine batch of this \
                     tip into {} outputs can pay at most {} sat",
                    n + 1,
                    total.saturating_sub(1)
                ));
            }
            let change = total - pieces_total;
            if let Some((_, amt)) = recipients.iter().find(|(_, a)| *a < floors.piece) {
                return Err(anyhow!(
                    "recipient amount {amt} is below the spine-batch piece minimum {} sat (each \
                     piece funds its own extension + state tier at {} sat/vB, then must clear the \
                     {DUST_LIMIT}-sat dust floor)",
                    floors.piece,
                    tip.parent.fee_rate
                ));
            }
            if change < floors.change {
                return Err(anyhow!(
                    "change {change} is below the spine-batch change minimum {} sat ({}) — lower \
                     the payment total or use a larger coin",
                    floors.change,
                    floors.change_note()
                ));
            }
            change
        };

        // N+1 fresh SE-registered slots, DERIVED against the TIP (it is the coin being spent, and
        // the coin whose lifetime allowance the vouchers are drawn from).
        let mut slot_tokens = self.take_derived_tokens(tip_statechain_id, n + 1).await?;
        let mut children: Vec<(Coin, String, u64)> = Vec::with_capacity(n + 1);
        for (address, amount) in recipients {
            let slot = self.create_child_slot(&slot_tokens.remove(0), *amount).await?;
            children.push((slot, mercurylib::tesr::payee_address(address, &network)?, *amount));
        }
        let change_child = self.create_child_slot(&slot_tokens.remove(0), change_sats).await?;
        let self_change_backup =
            mercurylib::transaction::get_user_backup_address(&change_child, network.clone())?;
        children.push((change_child.clone(), self_change_backup, change_sats));

        let mut tip_coin = self
            .record()
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(tip_statechain_id) && c.duplicate_index == 0)
            .cloned()
            .ok_or_else(|| anyhow!("spine tip coin {tip_statechain_id} not found"))?;

        // The change leg is LAST (pushed after every recipient) and becomes the NEXT one-cap tip.
        let batch = mercuryrustlib::tesr::spine_batch_split(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut tip_coin,
            &tip,
            &mut children,
            mercuryrustlib::tesr::ChangeLeg::LastIsTip,
        )
        .await?;
        let bundles = &batch.pieces;
        let next_tip = batch
            .tip
            .as_ref()
            .ok_or_else(|| anyhow!("the spine batch produced no change leg"))?;

        // Persist the change FIRST, then convey — the ordering `in_ladder_pay_many` argues for, and
        // for the same reason: `SP_{i+1}` and every tier are already co-signed and the tip is
        // terminal, so a conveyance that fails on recipient k would otherwise abort with the change
        // record unwritten, destroying the only record of this wallet's remaining value.
        mercuryrustlib::tesr::persist_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            next_tip,
        )
        .await?;
        for (j, (address, _)) in recipients.iter().enumerate() {
            mercuryrustlib::tesr::convey_child_bundle(
                &self.inner.cc,
                address,
                &children[j].0,
                &bundles[j],
                None,
            )
            .await?;
        }

        let piece_sids: Vec<String> = children[..n]
            .iter()
            .map(|(c, _, _)| c.statechain_id.clone().unwrap_or_default())
            .collect();
        let change_sid = change_child.statechain_id.clone().unwrap_or_default();
        // `SP_{i+1}` is the state of the segment the batch just appended — the spine level whose
        // outputs fund every leg, and `funding_tier()` is the ONE accessor that resolves it (the
        // last ancestor, not the root parent's `SP`). Hashed from the SIGNED transaction rather than
        // read off the declared field, so the outpoint this wallet books its change at is the one
        // the payees' bundles actually name.
        let sp_txid = signed_tier_txid(&next_tip.funding_tier().signed_tx)?;
        self.book_inladder_split_coins_n(
            tip_statechain_id,
            &sp_txid,
            &piece_sids,
            CoinStatus::WITHDRAWN,
            &change_sid,
            change_sats,
            next_tip.sp_vout,
        )
        .await?;
        if let Some(rows) = history {
            self.record_conveyed_pieces(&sp_txid, rows).await?;
        }
        // [P0-3] Close the batch's write-ahead journal: the bundles are on disk and conveyed.
        mercuryrustlib::tesr::journal_commit(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mercuryrustlib::tesr::split_op_id(&bundles[0]),
        )
        .await?;
        Ok((piece_sids, change_sid))
    }

    /// **[P0-3] THE RECOVERY READER for interrupted in-ladder splits.**
    ///
    /// Run it at startup (and after any crash): every in-ladder split that stopped after the parent
    /// was terminalized at the SE is replayed from its write-ahead journal, so the co-signed material
    /// — which the SE will never re-issue — comes back instead of being lost with the process.
    ///
    /// What it does NOT do: re-send a payment. A replayed piece is reported in
    /// [`InLadderSplitOutcome::Replayed::unconveyed_pieces`] and conveyed only by an explicit
    /// [`Self::convey_recovered_piece`], for the same reason the coloured lane's reader never sets
    /// `handed_over` — a crashed process is not evidence that the user still wants the payment made.
    ///
    /// Idempotent: a record it closes is not offered again.
    pub async fn recover_in_ladder_splits(&self) -> Result<Vec<InLadderSplitRecovery>> {
        use mercuryrustlib::tesr::SplitStage;
        let cc = &self.inner.cc;
        let wallet = self.inner.config.wallet_name.clone();
        let network = self.inner.config.network.to_string();
        let mut report = Vec::new();

        for mut rec in mercuryrustlib::tesr::journal_open_splits(cc, &wallet).await? {
            // Nothing irreversible had happened when a `Planned` record was written — unless the SE
            // consumed the budget in the window before we could record the co-signature.
            if rec.stage == SplitStage::Planned {
                let retryable = mercuryrustlib::tesr::split_is_retryable(cc, &rec).await?;
                let stage = if retryable { SplitStage::Committed } else { SplitStage::Stranded };
                mercuryrustlib::tesr::journal_close(cc, &wallet, &rec.op_id, stage).await?;
                report.push(InLadderSplitRecovery {
                    op_id: rec.op_id.clone(),
                    lane: rec.lane.clone(),
                    terminalized_statechain_id: rec.terminalized_statechain_id.clone(),
                    outcome: if retryable {
                        InLadderSplitOutcome::Retryable
                    } else {
                        InLadderSplitOutcome::CooperativePathLost
                    },
                });
                continue;
            }

            // `Signed` / `Established`: the material exists and must be REPLAYED, never restarted.
            // Supply every journalled child's coin so an unfinished ladder can be completed; a child
            // whose coin is missing makes `resume_in_ladder_split` fail closed rather than return a
            // bundle set that silently omits it.
            let coins = self.record().await?.coins;
            let mut child_coins: Vec<(String, Coin)> = rec
                .children
                .iter()
                .filter_map(|c| {
                    coins
                        .iter()
                        .find(|k| {
                            k.statechain_id.as_deref() == Some(c.statechain_id.as_str())
                                && k.duplicate_index == 0
                        })
                        .cloned()
                        .map(|k| (c.statechain_id.clone(), k))
                })
                .collect();
            let legs =
                mercuryrustlib::tesr::resume_in_ladder_split(cc, &wallet, &mut rec, &mut child_coins)
                    .await?;

            // Which leg is OURS?
            //
            // [CATS change 2] A SPINE TIP is ours by CONSTRUCTION — the journalled role says so, and
            // it is the one fact about the leg that was written before the parent was terminalized.
            // A PIECE is judged as before: ours iff its journalled Model-A payee is the exit address
            // this wallet derives from that child's own key. That derivation stays, because the two
            // lanes that still carve a two-tier change (and any older record) run through it.
            //
            // `legs` is index-stable against `rec.children`, which is what makes reading them
            // together safe: a leg list that quietly dropped the tip would shift every index after it
            // and persist a payee's bundle under the change's statechain id.
            let mut change: Option<(String, u64, u32)> = None;
            let mut unconveyed_pieces: Vec<String> = Vec::new();
            for (j, jc) in rec.children.iter().enumerate() {
                match &legs[j] {
                    mercuryrustlib::tesr::SplitLeg::Tip(tip) => {
                        mercuryrustlib::tesr::persist_spine_tip(cc, &wallet, tip).await?;
                        change = Some((jc.statechain_id.clone(), jc.value, jc.sp_vout));
                    }
                    mercuryrustlib::tesr::SplitLeg::Piece(cb) => {
                        let ours = child_coins
                            .iter()
                            .find(|(id, _)| *id == jc.statechain_id)
                            .map(|(_, coin)| {
                                mercurylib::transaction::get_user_backup_address(
                                    coin,
                                    network.clone(),
                                )
                                .map(|a| a == jc.owner_exit_address)
                                .unwrap_or(false)
                            })
                            .unwrap_or(false);
                        if ours {
                            mercuryrustlib::tesr::persist_child(cc, &wallet, cb).await?;
                            change = Some((jc.statechain_id.clone(), jc.value, jc.sp_vout));
                        } else {
                            unconveyed_pieces.push(jc.statechain_id.clone());
                        }
                    }
                }
            }

            // Book what the split actually did: the parent is terminal (gone), the change is a fresh
            // confirmed claim funded by `SP.out[j]`. The pieces are deliberately left untouched —
            // they were never conveyed.
            self.book_inladder_split_coins_opt(
                &rec.terminalized_statechain_id,
                &rec.sp_txid,
                &[],
                CoinStatus::WITHDRAWN,
                change.clone(),
            )
            .await?;
            mercuryrustlib::tesr::journal_commit(cc, &wallet, &rec.op_id).await?;
            report.push(InLadderSplitRecovery {
                op_id: rec.op_id.clone(),
                lane: rec.lane.clone(),
                terminalized_statechain_id: rec.terminalized_statechain_id.clone(),
                outcome: InLadderSplitOutcome::Replayed {
                    change_statechain_id: change.map(|(id, _, _)| id),
                    unconveyed_pieces,
                },
            });
        }
        Ok(report)
    }

    /// Convey a piece whose in-ladder split was interrupted and then recovered by
    /// [`Self::recover_in_ladder_splits`] — the explicit "yes, still send it" step.
    ///
    /// Rebuilds the piece's bundle from the journal (the material is already co-signed; nothing new
    /// is signed here) and hands it over exactly as the original call would have.
    pub async fn convey_recovered_piece(
        &self,
        op_id: &str,
        piece_statechain_id: &str,
        recipient_address: &str,
    ) -> Result<()> {
        let cc = &self.inner.cc;
        let wallet = self.inner.config.wallet_name.clone();
        let rec = mercuryrustlib::tesr::journal_find(cc, &wallet, op_id)
            .await?
            .ok_or_else(|| anyhow!("no in-ladder split journal record {op_id}"))?;
        let idx = rec
            .children
            .iter()
            .position(|c| c.statechain_id == piece_statechain_id)
            .ok_or_else(|| anyhow!("split {op_id} carved no child {piece_statechain_id}"))?;
        // [CATS change 2] Rebuild THAT leg, as a piece. A root-lane record now normally holds a spine
        // tip too, so `bundles()` (all-pieces) would refuse the whole record; and naming the tip's
        // index here must be an error, not a conveyance of the sender's own change to a third party.
        let piece_bundle = rec.piece_bundle(idx)?;
        let piece_coin = self
            .record()
            .await?
            .coins
            .iter()
            .find(|c| {
                c.statechain_id.as_deref() == Some(piece_statechain_id) && c.duplicate_index == 0
            })
            .cloned()
            .ok_or_else(|| anyhow!("piece coin {piece_statechain_id} not found"))?;
        mercuryrustlib::tesr::convey_child_bundle(
            cc,
            recipient_address,
            &piece_coin,
            &piece_bundle,
            None,
        )
        .await?;
        self.set_coin_status(piece_statechain_id, CoinStatus::WITHDRAWN).await?;
        Ok(())
    }

    /// Set a single coin's status by statechain_id in the local wallet db.
    pub(crate) async fn set_coin_status(&self, statechain_id: &str, status: CoinStatus) -> Result<()> {
        let mut record = self.record().await?;
        for coin in record.coins.iter_mut() {
            if coin.statechain_id.as_deref() == Some(statechain_id) {
                coin.status = status.clone();
            }
        }
        self.save_record(&record).await?;
        Ok(())
    }

    /// Create one SE-registered child slot funded by a derived token, returning its `Coin` (with
    /// statechain_id + auth). The slot's aggregate is what `SP.out[j]` pays in the in-ladder split.
    pub(crate) async fn create_child_slot(&self, token_id: &str, amount_sats: u64) -> Result<Coin> {
        let addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            token_id,
            u32::try_from(amount_sats)?,
        )
        .await?;
        self.record()
            .await?
            .coins
            .iter()
            .find(|c| c.aggregated_address.as_deref() == Some(&addr))
            .cloned()
            .ok_or_else(|| anyhow!("child slot coin not found for {addr}"))
    }

    /// Book the coin records after an in-ladder split payment: the parent is spent (WITHDRAWN), the
    /// piece slot is sent to the recipient (WITHDRAWN — its value left this wallet), and the change slot
    /// becomes a CONFIRMED exitable claim funded by the un-broadcast `SP.out[1]`.
    pub(crate) async fn book_inladder_split_coins(
        &self,
        parent_statechain_id: &str,
        sp_txid: &str,
        piece_child_sid: &str,
        change_child_sid: &str,
        change_sats: u64,
        piece_status: CoinStatus,
    ) -> Result<()> {
        let pieces = [piece_child_sid.to_string()];
        self.book_inladder_split_coins_n(
            parent_statechain_id,
            sp_txid,
            &pieces,
            piece_status,
            change_child_sid,
            change_sats,
            1,
        )
        .await
    }

    /// N-piece variant of [`Self::book_inladder_split_coins`] (multi-recipient in-ladder split): the
    /// change slot is funded by `SP.out[change_vout]`, which is the LAST payload output — `SP.out[j]`
    /// is `children[j]` and the change is appended after the N pieces, so `change_vout == N`.
    async fn book_inladder_split_coins_n(
        &self,
        parent_statechain_id: &str,
        sp_txid: &str,
        piece_child_sids: &[String],
        piece_status: CoinStatus,
        change_child_sid: &str,
        change_sats: u64,
        change_vout: u32,
    ) -> Result<()> {
        self.book_inladder_split_coins_opt(
            parent_statechain_id,
            sp_txid,
            piece_child_sids,
            piece_status,
            Some((change_child_sid.to_string(), change_sats, change_vout)),
        )
        .await
    }

    /// [CTES-R] The general form: N pieces and **optionally** a change child.
    ///
    /// A coloured split that hands over a carrier's WHOLE allocation (the ordinary case in the
    /// middle of a multi-carrier payment, and the only shape a fully-spent carrier can take) carves
    /// no change child at all — every sat of `X_m`'s payload goes to the pieces. Passing a change
    /// slot there would mean carving a child that holds an EMPTY RGB assignment purely to satisfy
    /// this booking call, which is both wasteful (it must still clear `colored_child_floor`) and a
    /// shape nothing else in CTES-R produces. So the change is an `Option` rather than a required
    /// argument, and the parent is still marked WITHDRAWN either way — that part is about the
    /// parent being terminalized, not about where the change went.
    pub(crate) async fn book_inladder_split_coins_opt(
        &self,
        parent_statechain_id: &str,
        sp_txid: &str,
        piece_child_sids: &[String],
        piece_status: CoinStatus,
        change: Option<(String, u64, u32)>,
    ) -> Result<()> {
        let mut record = self.record().await?;
        for coin in record.coins.iter_mut() {
            match coin.statechain_id.as_deref() {
                Some(sid) if sid == parent_statechain_id && coin.duplicate_index == 0 => {
                    coin.status = CoinStatus::WITHDRAWN;
                }
                Some(sid) if piece_child_sids.iter().any(|p| p == sid) => {
                    // Plain conveyance: value given to the recipient (WITHDRAWN). Latched LN piece:
                    // IN_TRANSFER (conveyed but pending payment; the SSP adopts it once it pays).
                    coin.status = piece_status.clone();
                }
                Some(sid)
                    if change.as_ref().is_some_and(|(c, _, _)| c == sid) =>
                {
                    let (_, change_sats, change_vout) = change.clone().expect("matched above");
                    coin.utxo_txid = Some(sp_txid.to_string());
                    coin.utxo_vout = Some(change_vout);
                    coin.amount = Some(u32::try_from(change_sats)?);
                    coin.status = CoinStatus::CONFIRMED;
                }
                _ => {}
            }
        }
        self.save_record(&record).await?;
        Ok(())
    }

    /// Record one `"Transfer"` history row per conveyed in-ladder piece, keyed on the piece's funding
    /// outpoint `SP.out[j]`. An in-ladder split conveys its pieces directly through the mailbox and
    /// never calls `transfer_sender::execute` — which is what writes the history row for a whole-coin
    /// handover — so without this an off-chain in-ladder payment would leave no trace in
    /// `get_transfers()`. Never called for a LATCHED piece: that value has not left the wallet until
    /// the Lightning preimage lands.
    async fn record_conveyed_pieces(
        &self,
        split_txid: &str,
        pieces: &[(String, u64)],
    ) -> Result<()> {
        let mut record = self.record().await?;
        for (vout, (_, amount)) in pieces.iter().enumerate() {
            record.activities.push(mercuryrustlib::utils::create_activity(
                &format!("{split_txid}:{vout}"),
                u32::try_from(*amount)?,
                "Transfer",
            ));
        }
        self.save_record(&record).await?;
        Ok(())
    }

    /// Register the outputs of a signed (un-broadcast) split tx as wallet coins: patch the
    /// freshly-initialised coin records onto the outputs, mark the parent spent, give each
    /// sub-coin its own first backup tx + locktime, and persist the shared exit branch under
    /// "branch-<id>". `outputs` is `[(aggregated_address, vout, sats), ...]`; returns the
    /// statechain ids in the same order (currently piece, change).
    pub(crate) async fn register_split_subcoins(
        &self,
        parent_statechain_id: &str,
        signed_split_tx_hex: &str,
        split_txid: &str,
        outputs: &[(String, u32, u64)],
    ) -> Result<(String, String)> {
        let ids = self
            .register_split_subcoins_n(parent_statechain_id, signed_split_tx_hex, split_txid, outputs)
            .await?;
        Ok((ids[0].clone(), ids[1].clone()))
    }

    /// N-output variant of [`Self::register_split_subcoins`]: returns the statechain ids of every
    /// registered sub-coin, in the same order as `outputs`. Used by batch token transfers where a
    /// single colored split funds many recipient pieces + one change.
    pub(crate) async fn register_split_subcoins_n(
        &self,
        parent_statechain_id: &str,
        signed_split_tx_hex: &str,
        split_txid: &str,
        outputs: &[(String, u32, u64)],
    ) -> Result<Vec<String>> {
        let mut record = self.record().await?;
        let mut ids: Vec<String> = vec![String::new(); outputs.len()];
        for coin in record.coins.iter_mut() {
            let addr = coin.aggregated_address.clone().unwrap_or_default();
            if coin.status == CoinStatus::INITIALISED {
                if let Some((i, (_, vout, sats))) =
                    outputs.iter().enumerate().find(|(_, (a, _, _))| *a == addr)
                {
                    coin.utxo_txid = Some(split_txid.to_string());
                    coin.utxo_vout = Some(*vout);
                    coin.amount = Some(u32::try_from(*sats)?);
                    coin.status = CoinStatus::CONFIRMED;
                    ids[i] = coin.statechain_id.clone().unwrap_or_default();
                    continue;
                }
            }
            if coin.statechain_id.as_deref() == Some(parent_statechain_id)
                && coin.duplicate_index == 0
            {
                // Parent is terminally spent by the split.
                coin.status = CoinStatus::WITHDRAWN;
            }
        }
        if ids.iter().any(|i| i.is_empty()) {
            return Err(anyhow!("split sub-coin registration failed"));
        }

        // Each sub-coin gets its own first backup tx (exit leaf) + locktime.
        let network = self.inner.config.network.to_string();
        let mut sub_backups: Vec<(String, mercurylib::wallet::BackupTx)> = Vec::new();
        for coin in record.coins.iter_mut() {
            let id = coin.statechain_id.clone().unwrap_or_default();
            if ids.contains(&id) && coin.status == CoinStatus::CONFIRMED {
                let bkp =
                    mercuryrustlib::deposit::create_tx1(&self.inner.cc, coin, &network, 1).await?;
                coin.locktime = Some(mercurylib::utils::get_blockheight(&bkp)?);
                sub_backups.push((id, bkp));
            }
        }
        self.save_record(&record).await?;

        // The exit branch is stored root-first: every un-broadcast tx from an ON-CHAIN outpoint
        // down to this split. When the parent is itself an off-chain sub-coin it already carries a
        // branch (its own chain from the on-chain root); inherit that and append this split as the
        // final hop. Otherwise the branch root's input would be the parent's un-broadcast funding
        // tx, which the receiver cannot resolve on-chain (validate_branch would fail resolving it).
        let mut branch_txs: Vec<mercurylib::wallet::BackupTx> =
            mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("branch-{parent_statechain_id}"),
            )
            .await
            .unwrap_or_default();
        branch_txs.push(mercurylib::wallet::BackupTx {
            tx_n: (branch_txs.len() + 1) as u32,
            tx: signed_split_tx_hex.to_string(),
            client_public_nonce: String::new(),
            server_public_nonce: String::new(),
            client_public_key: String::new(),
            server_public_key: String::new(),
            blinding_factor: String::new(),
            rgb_consignment: None,
            rgb_blinding: None,
        });
        for (id, bkp) in &sub_backups {
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                id,
                &vec![bkp.clone()],
            )
            .await?;
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("branch-{id}"),
                &branch_txs,
            )
            .await?;
        }

        // Record the structural ancestor chain (stored under "parents-<id>", one id per row) so a
        // future transfer of the sub-coin can prove to its receiver that every ancestor is
        // terminal at the SE. ancestors = this split's parent plus that parent's own ancestors.
        let mut ancestors: Vec<String> = vec![parent_statechain_id.to_string()];
        if let Ok(inherited) = mercuryrustlib::sqlite_manager::get_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &format!("parents-{parent_statechain_id}"),
        )
        .await
        {
            ancestors.extend(inherited.iter().map(|b| b.tx.clone()));
        }
        let parent_rows: Vec<mercurylib::wallet::BackupTx> = ancestors
            .iter()
            .enumerate()
            .map(|(i, id)| mercurylib::wallet::BackupTx {
                tx_n: (i + 1) as u32,
                tx: id.clone(),
                client_public_nonce: String::new(),
                server_public_nonce: String::new(),
                client_public_key: String::new(),
                server_public_key: String::new(),
                blinding_factor: String::new(),
                rgb_consignment: None,
                rgb_blinding: None,
            })
            .collect();
        for id in &ids {
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("parents-{id}"),
                &parent_rows,
            )
            .await?;
        }

        Ok(ids)
    }

    /// Register the sub-coins of a COMBINE (N input carriers → M outputs, e.g. piece + change).
    /// Mirrors [`Self::register_split_subcoins_n`] but the branch is a DAG: each output's exit
    /// branch is the UNION of all N inputs' sub-branches (root-first) plus the combine tx, and its
    /// ancestor list is the union of every input carrier id + that input's inherited ancestors — so
    /// the receiver's per-structural-input terminal check (Σ branch inputs) is satisfied with exactly
    /// one named terminal ancestor per combined input.
    ///
    /// Only DISJOINT input sub-branches are supported (no two inputs share an ancestor tx); a shared
    /// ancestor (a re-merging DAG) is rejected — rare, and it would make the ancestor-count bookkeeping
    /// ambiguous. Flat carriers (on-chain funding, empty branch) are the trivially-disjoint common case.
    pub(crate) async fn register_combine_subcoins(
        &self,
        parent_ids: &[String],
        signed_combine_tx_hex: &str,
        combine_txid: &str,
        outputs: &[(String, u32, u64)],
    ) -> Result<Vec<String>> {
        let mut record = self.record().await?;
        let mut ids: Vec<String> = vec![String::new(); outputs.len()];
        for coin in record.coins.iter_mut() {
            let addr = coin.aggregated_address.clone().unwrap_or_default();
            if coin.status == CoinStatus::INITIALISED {
                if let Some((i, (_, vout, sats))) =
                    outputs.iter().enumerate().find(|(_, (a, _, _))| *a == addr)
                {
                    coin.utxo_txid = Some(combine_txid.to_string());
                    coin.utxo_vout = Some(*vout);
                    coin.amount = Some(u32::try_from(*sats)?);
                    coin.status = CoinStatus::CONFIRMED;
                    ids[i] = coin.statechain_id.clone().unwrap_or_default();
                    continue;
                }
            }
            // Every combined input carrier is terminally spent by the combine.
            if coin.duplicate_index == 0
                && coin
                    .statechain_id
                    .as_deref()
                    .map_or(false, |sid| parent_ids.iter().any(|p| p == sid))
            {
                coin.status = CoinStatus::WITHDRAWN;
            }
        }
        if ids.iter().any(|i| i.is_empty()) {
            return Err(anyhow!("combine sub-coin registration failed"));
        }

        // Fresh first backup (exit leaf) per output sub-coin.
        let network = self.inner.config.network.to_string();
        let mut sub_backups: Vec<(String, mercurylib::wallet::BackupTx)> = Vec::new();
        for coin in record.coins.iter_mut() {
            let id = coin.statechain_id.clone().unwrap_or_default();
            if ids.contains(&id) && coin.status == CoinStatus::CONFIRMED {
                let bkp =
                    mercuryrustlib::deposit::create_tx1(&self.inner.cc, coin, &network, 1).await?;
                coin.locktime = Some(mercurylib::utils::get_blockheight(&bkp)?);
                sub_backups.push((id, bkp));
            }
        }
        self.save_record(&record).await?;

        // Merge the input sub-branches (root-first) — rejecting any shared ancestor tx — then append
        // the combine as the final hop. For flat carriers every sub-branch is empty, so the merged
        // branch is just [combine].
        let mut merged_branch: Vec<mercurylib::wallet::BackupTx> = Vec::new();
        let mut seen_txids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for pid in parent_ids {
            let sub = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("branch-{pid}"),
            )
            .await
            .unwrap_or_default();
            for b in sub {
                let txid = bitcoin::consensus::encode::deserialize::<bitcoin::Transaction>(
                    &hex::decode(&b.tx)?,
                )?
                .txid()
                .to_string();
                if !seen_txids.insert(txid) {
                    return Err(anyhow!(
                        "combine of carriers sharing a common ancestor is not supported — pick independent carriers"
                    ));
                }
                merged_branch.push(b);
            }
        }
        // Re-number and append the combine tx last (the receiver validates by txid lookup, but keep
        // tx_n contiguous and root-first for clarity).
        for (i, b) in merged_branch.iter_mut().enumerate() {
            b.tx_n = (i + 1) as u32;
        }
        merged_branch.push(mercurylib::wallet::BackupTx {
            tx_n: (merged_branch.len() + 1) as u32,
            tx: signed_combine_tx_hex.to_string(),
            client_public_nonce: String::new(),
            server_public_nonce: String::new(),
            client_public_key: String::new(),
            server_public_key: String::new(),
            blinding_factor: String::new(),
            rgb_consignment: None,
            rgb_blinding: None,
        });
        for (id, bkp) in &sub_backups {
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                id,
                &vec![bkp.clone()],
            )
            .await?;
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("branch-{id}"),
                &merged_branch,
            )
            .await?;
        }

        // Ancestor list = every input carrier id + that input's own inherited ancestors. Because the
        // sub-branches are disjoint, these are all distinct, so the count equals the receiver's
        // required-terminal-ancestor count (Σ inputs across the merged branch).
        let mut ancestors: Vec<String> = Vec::new();
        for pid in parent_ids {
            ancestors.push(pid.clone());
            if let Ok(inherited) = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("parents-{pid}"),
            )
            .await
            {
                ancestors.extend(inherited.iter().map(|b| b.tx.clone()));
            }
        }
        let parent_rows: Vec<mercurylib::wallet::BackupTx> = ancestors
            .iter()
            .enumerate()
            .map(|(i, id)| mercurylib::wallet::BackupTx {
                tx_n: (i + 1) as u32,
                tx: id.clone(),
                client_public_nonce: String::new(),
                server_public_nonce: String::new(),
                client_public_key: String::new(),
                server_public_key: String::new(),
                blinding_factor: String::new(),
                rgb_consignment: None,
                rgb_blinding: None,
            })
            .collect();
        for id in &ids {
            mercuryrustlib::sqlite_manager::insert_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("parents-{id}"),
                &parent_rows,
            )
            .await?;
        }

        Ok(ids)
    }

    /// Blind-MuSig2 co-sign a multi-output spend of `coin` (the plain-BTC split; the RGB-colored
    /// variant lives in `mercuryrustlib::rgb::create_colored_split_tx`). `qt_backup_tx` positions
    /// the split's locktime in the decrement ladder (backup count + 1 beats the parent's backup).
    async fn sign_split_tx(
        &self,
        coin: &mut Coin,
        outputs: &[(String, u64)],
        qt_backup_tx: u32,
    ) -> Result<String> {
        let cc = &self.inner.cc;
        let network = self.inner.config.network.to_string();
        let server_info = mercuryrustlib::utils::info_config(cc).await?;

        let coin_nonce = create_and_commit_nonces(coin)?;
        coin.secret_nonce = Some(coin_nonce.secret_nonce.clone());
        coin.public_nonce = Some(coin_nonce.public_nonce.clone());
        coin.blinding_factor = Some(coin_nonce.blinding_factor.clone());
        let server_public_nonce = mercuryrustlib::transaction::sign_first(
            cc,
            &coin_nonce.sign_first_request_payload,
        )
        .await?;
        coin.server_public_nonce = Some(server_public_nonce);

        let block_height = {
            use electrum_client::ElectrumApi;
            cc.electrum_client.block_headers_subscribe_raw()?.height as u32
        };

        let unsigned_psbt_b64 = get_unsigned_split_psbt(
            coin,
            block_height,
            server_info.initlock,
            server_info.interval,
            qt_backup_tx,
            outputs.to_vec(),
            network.clone(),
            false,
        )?;
        let psbt = Psbt::from_str(&unsigned_psbt_b64)
            .map_err(|e| anyhow!("could not parse split psbt: {e}"))?;
        let tx_hex = hex::encode(bitcoin::consensus::encode::serialize(&psbt.unsigned_tx));

        let partial_sig_request =
            get_partial_sig_request_for_colored_tx(coin, tx_hex.clone(), network)?;
        let server_partial_sig = mercuryrustlib::transaction::sign_second(
            cc,
            &partial_sig_request.partial_signature_request_payload,
        )
        .await?;
        let signature = create_signature(
            partial_sig_request.msg,
            partial_sig_request.client_partial_sig,
            hex::encode(server_partial_sig.serialize()),
            partial_sig_request.encoded_session,
            partial_sig_request.output_pubkey,
        )?;
        Ok(new_backup_transaction(tx_hex, signature)?)
    }
}

/// txid of a signed (un-broadcast) tier tx, from its hex. The in-ladder split's children are funded
/// by `SP.out[j]`, an outpoint that exists only inside the bundle — so the booking code derives the
/// txid from the signed tx rather than from any on-chain lookup.
pub(crate) fn signed_tier_txid(signed_tx_hex: &str) -> Result<String> {
    let tx: bitcoin::Transaction =
        bitcoin::consensus::encode::deserialize(&hex::decode(signed_tx_hex)?)?;
    Ok(tx.txid().to_string())
}

/// **[P0-4] The real sat cost of paying a non-exact amount out of a LADDERED coin.**
///
/// The commonly-quoted figure counted only the `SP` tier and omitted the two children the split
/// creates — each of which gets its OWN extension and state rung, every rung burning
/// `committed_fee + P2A_VALUE`. Measured as loss of exitable value across the tree
/// (`docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md` §1.1):
///
/// ```text
///   SP split tier (2 payload outputs)   576   committed_fee_for_outputs(2, r) + P2A
///   piece child   — extension + state   980   2 × (committed_fee(r) + P2A)
///   change child  — extension + state   980   2 × (committed_fee(r) + P2A)
///                                     -----
///                                      2 536  at r = 2.0 sat/vB
/// ```
///
/// This is the GROSS figure: it deliberately does not take the −490 credit the economics ledger
/// applies for the parent's superseded state rung. That rung's sats were burned when the parent's
/// ladder was built, not by this payment, and crediting them would let the quote come in UNDER what
/// the payer's tree actually gives up. The old quote — `clamp(parent/100, 300, 2000)` — returned 300
/// sat here, a 6.8× under-quote on a 10 000-sat parent.
pub(crate) fn in_ladder_split_cost(fee_rate_sats_per_vb: f64) -> u64 {
    let rung = mercurylib::tesr::committed_fee(fee_rate_sats_per_vb) + mercurylib::tesr::P2A_VALUE;
    let sp_tier = mercurylib::tesr::committed_fee_for_outputs(2, fee_rate_sats_per_vb)
        + mercurylib::tesr::P2A_VALUE;
    sp_tier + 4 * rung
}

/// Miner-fee margin left in a split tx for its (exit-only) broadcast.
pub(crate) fn split_fee_reserve(parent_sats: u64) -> u64 {
    // ~200 vB at a couple sat/vB, floored so tiny test coins still split.
    (parent_sats / 100).clamp(300, 2_000)
}

/// Dust floor for every split output (audit [9]): a P2TR output below 330 sats is
/// non-standard/unrelayable, so a split tx containing one is unbroadcastable and — once the
/// parent is consumed — strands both sub-coins with no on-chain exit. Shared with the planner
/// (`select::plan`) and the invalidation model tests.
pub(crate) const DUST_LIMIT: u64 = 330;

/// Measured vsize of a sub-coin's own backup tx (1-in-1-out P2TR keyspend). The backup sweeps
/// `sub_coin_sats − ceil(BACKUP_TX_VBYTES · fee_rate)`, which must itself clear the dust floor.
pub(crate) const BACKUP_TX_VBYTES: u64 = 112;

/// The minimum VIABLE value for a split sub-coin output at backup feerate `fee_rate_sats_per_byte`:
/// the P2TR dust floor PLUS the fee the sub-coin's own backup tx must pay. A split output below
/// this is a valid tx output but a coin that can never be exited — its backup would sweep below
/// dust (`create_tx1` → `MercuryError::FeeTooLow`, lib/src/transaction.rs). Admitting it and then
/// consuming the parent (spend budget → terminal) strands the parent to unilateral-exit-only.
/// `fee_rate_sats_per_byte` MUST be the rate `create_tx1` uses = `min(SE quote, max_fee_rate)`.
pub(crate) fn min_split_output(fee_rate_sats_per_byte: f64) -> u64 {
    DUST_LIMIT + (BACKUP_TX_VBYTES as f64 * fee_rate_sats_per_byte).ceil() as u64
}

/// The backup feerate `create_tx1` will use for this wallet's sub-coins: `min(SE quote, max)`.
pub(crate) async fn backup_fee_rate(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<f64> {
    let info = mercuryrustlib::utils::info_config(cc).await?;
    Ok(info.fee_rate_sats_per_byte.min(cc.max_fee_rate))
}

// ================================================================================================
// [B2] ONE FLOOR, ONE SOURCE.
//
// `quote_transfer` reports `fundable`; `transfer` executes. Round 1 (P0-4) raised the floor in the
// quote and left the executor planning at the bare `min_split_output`, which did not close the bug —
// it only changed which side was wrong. A quote that can disagree with the executor IS the bug.
//
// Everything the two sides must agree on now has exactly one derivation, below:
//   * the ROUTE            -> `UtexoWallet::parent_shape`   (load_child, then load; errors PROPAGATE)
//   * the per-output FLOOR -> `split_output_floor`
//   * the split TOTAL      -> `ParentShape::split_total`
//   * ADMISSIBILITY        -> `split_amounts_floored` / `inladder_amounts_floored`
//   * all four at once     -> `split_preflight_pure`
//   * plan + verdict       -> `UtexoWallet::plan_payment`
// `quote_transfer` and `transfer` both call `plan_payment` and nothing else, so
// `fundable: true` followed by a refusal is no longer expressible.
// ================================================================================================

/// How a parent is laddered. This ONE resolution decides both the split ROUTE and the split FLOOR,
/// so the quote and the executor cannot answer the same question differently.
///
/// Resolved only by [`UtexoWallet::parent_shape`], which PROPAGATES a failed bundle read. A failed
/// read must never fall through to `Unladdered`: that is simultaneously the cheaper cost model and
/// the LOWER floor, i.e. the silent-degradation shape that quotes an unfundable payment as fundable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ParentShape {
    /// No TES-R ladder: the plain off-chain split (`split_coin`), whose sub-coins carry only their
    /// own backup tx.
    Unladdered,
    /// A received in-ladder split CHILD: it re-splits at its OWN level (`child_in_ladder_pay`),
    /// carving `CSP` out of `ext_child.out[0]`.
    Child { fee_rate: f64, split_source_value: u64 },
    /// A root TES-R coin: `in_ladder_pay` carves `SP` out of `X_m.out[0]`.
    Root { fee_rate: f64, split_source_value: u64 },
    /// **[CATS/V4] A SPINE TIP** — the sender's own change leg from a CATS batch, funded by the
    /// un-broadcast `SP_i.out[K]` and capped by ONE tier.
    ///
    /// It exists as a variant for one reason: without it a tip falls through `parent_shape` to
    /// `Unladdered`, which is simultaneously the cheaper cost model, the LOWER floor, and the route
    /// to `split_coin` — a plain split of a laddered coin, which [B1] makes unsafe. That is three
    /// wrong answers from one missing arm, and every one of them fails open.
    ///
    /// [CATS spine batch] Splitting a tip is the NEXT spine batch (`SP_{i+1}` over `SP_i.out[K]`,
    /// `mercuryrustlib::tesr::spine_batch_split`), and it routes to `spine_batch_pay`. Note
    /// `split_source_value` is the tip's `sp_out_value` — the value of `SP_i.out[K]`, the outpoint
    /// the next batch SPENDS — and not the cap's output value: the cap is the tier being replaced,
    /// not the one being extended, so pricing off it would under-fund every leg by a committed fee.
    SpineTip { fee_rate: f64, split_source_value: u64 },
}

impl ParentShape {
    /// The committed fee rate of the ladder this parent's children inherit, or `None` when there is
    /// no ladder and therefore no `min_child_value` floor.
    pub(crate) fn ladder_fee_rate(self) -> Option<f64> {
        match self {
            ParentShape::Unladdered => None,
            ParentShape::Child { fee_rate, .. }
            | ParentShape::Root { fee_rate, .. }
            | ParentShape::SpineTip { fee_rate, .. } => Some(fee_rate),
        }
    }

    /// The value an in-ladder split of this parent can carve into `n_payload` outputs — the tier
    /// source value net of the split tier's own committed fee and P2A anchor. `None` for an
    /// un-laddered parent (whose capacity is `parent_sats` minus the split fee reserve instead) or
    /// when the committed fee no longer fits.
    pub(crate) fn split_total(self, n_payload: usize) -> Option<u64> {
        match self {
            ParentShape::Unladdered => None,
            ParentShape::Child { fee_rate, split_source_value }
            | ParentShape::Root { fee_rate, split_source_value }
            | ParentShape::SpineTip { fee_rate, split_source_value } => {
                mercurylib::tesr::tier_out_total(split_source_value, n_payload, fee_rate)
            }
        }
    }

    /// The executor this shape dispatches to — named in quotes and refusals so a disagreement is
    /// legible instead of mysterious.
    pub(crate) fn route(self) -> &'static str {
        match self {
            ParentShape::Unladdered => "plain off-chain split",
            ParentShape::Child { .. } => "child in-ladder split",
            ParentShape::Root { .. } => "in-ladder split",
            ParentShape::SpineTip { .. } => "spine batch",
        }
    }
}

/// **[V5] The per-LEG floors of one in-ladder split.** A split has two kinds of output and — once
/// CATS change 2 lands — two different SHAPES of output, so it has two floors. Returning one number
/// for both is the [V5] hazard in a sentence: the two legs then move together, and lowering the
/// change leg to its true one-rung cost lowers the payee's piece with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SplitFloors {
    /// Every PAYEE piece must clear this. A piece is always a two-tier child (V1's correction is
    /// explicit that the conveyed leaf stays two-tier), so this floor never falls.
    pub piece: u64,
    /// The sender's CHANGE leg must clear this — 490 sat below `piece` on the PLAIN ROOT lane, where
    /// the change is a one-cap spine tip, and equal to it on the two lanes whose builders still
    /// carve a two-tier change. Which is which is [`mercuryrustlib::tesr::change_leg_role`]'s answer
    /// for that lane, never a number chosen here.
    pub change: u64,
    /// **The lane these two numbers were derived for**, carried WITH them rather than passed
    /// alongside. `None` for the un-laddered split, which has no in-ladder builder at all.
    ///
    /// It is here so the refusal text explaining `change` cannot describe a different lane's shape
    /// than the number it is explaining. Deriving that text from the numbers instead (`change <
    /// piece` ⟹ "one-cap tip") reads correctly today and lies during a fee spike: once
    /// `min_split_output` exceeds `min_child_value` both legs clamp to it, the two floors become
    /// equal, and the shape has not changed at all.
    pub lane: Option<mercuryrustlib::tesr::SplitLane>,
}

impl SplitFloors {
    /// The floor that binds BOTH legs — the larger. Used where one number is genuinely right: the
    /// plain un-laddered split (whose two sub-coins have identical shape) and any single-number
    /// report.
    pub(crate) fn binding(self) -> u64 {
        self.piece.max(self.change)
    }

    /// The smallest floor either leg imposes. Used by the shape-BLIND planner, which must never
    /// refuse a split some coin could actually make; the named coin is then judged per-leg.
    pub(crate) fn planning(self) -> u64 {
        self.piece.min(self.change)
    }

    /// One legible string for a quote or refusal: a single number while the legs agree, and both
    /// numbers, named, as soon as they do not.
    pub(crate) fn describe(self) -> String {
        if self.piece == self.change {
            self.piece.to_string()
        } else {
            format!("piece {}, change {}", self.piece, self.change)
        }
    }

    /// Why the CHANGE leg's floor is what it is, in the words of the lane it came from.
    pub(crate) fn change_note(self) -> &'static str {
        match self.lane {
            Some(lane) => change_leg_shape_note(lane),
            // Un-laddered: both sub-coins carry only their own backup tx, so there is no ladder
            // shape to describe and the floor is the backup-fee floor for both.
            None => "an un-laddered sub-coin funds only its own backup transaction",
        }
    }
}

/// **THE** per-leg floors for a split of this parent. Every admission guard in this file and the
/// quote derive their floors here and nowhere else.
///
/// Two floors apply to each leg and the LARGER binds:
///  * `min_split_output(backup_fee_rate)` — the dust limit plus the fee the sub-coin's OWN backup tx
///    must pay; below it the sub-coin exists but can never be exited.
///  * the LADDER floor for that leg's SHAPE — a payee's piece gets its own extension + state rungs
///    from `establish_child` (`min_child_value`), while the sender's change gets whatever
///    [`mercuryrustlib::tesr::change_leg_role`] says THAT LANE's builder actually gives it: one rung
///    (`min_spine_tip_value`) on the plain root lane, two on the other two. Only applies when the
///    parent carries a ladder.
///
/// The leg's shape is deliberately NOT a parameter. It is read from the one function that describes
/// what the builders emit, so the floor a payment is admitted at and the ladder that is then built
/// cannot be two different shapes — which is the failure this split exists to prevent, and it lands
/// *after* `set_spend_budget` has terminalized the parent. The LANE is a parameter, because the
/// three builders no longer agree and the question is meaningless without naming one.
pub(crate) fn split_output_floors(backup_fee_rate: f64, shape: ParentShape) -> SplitFloors {
    let backup_floor = min_split_output(backup_fee_rate);
    let laddered = |rate: f64, lane: mercuryrustlib::tesr::SplitLane| SplitFloors {
        piece: backup_floor
            .max(mercuryrustlib::tesr::SplitLegRole::Piece.min_value(rate, DUST_LIMIT)),
        change: backup_floor
            .max(mercuryrustlib::tesr::change_leg_role(lane).min_value(rate, DUST_LIMIT)),
        lane: Some(lane),
    };
    // Exhaustive on the SHAPE, so a fourth parent shape is a compile error rather than a fourth
    // silent fall-through to the un-laddered (lower) floor.
    match shape {
        ParentShape::Unladdered => {
            SplitFloors { piece: backup_floor, change: backup_floor, lane: None }
        }
        ParentShape::Root { fee_rate, .. } => {
            laddered(fee_rate, mercuryrustlib::tesr::SplitLane::PlainRoot)
        }
        ParentShape::Child { fee_rate, .. } => {
            laddered(fee_rate, mercuryrustlib::tesr::SplitLane::PlainChild)
        }
        // [CATS spine batch] Splitting a tip is the NEXT spine batch, and that builder is landed — so
        // this number IS an admission decision now. It is priced on the spine-batch lane, whose
        // change leg `spine_batch_split` carves as a one-cap tip exactly as the root lane does; the
        // pieces are ordinary two-tier children on both. Never the un-laddered pair, which is the
        // cheaper and LOWER answer that [V4] exists to keep unreachable.
        ParentShape::SpineTip { fee_rate, .. } => {
            laddered(fee_rate, mercuryrustlib::tesr::SplitLane::SpineBatch)
        }
    }
}

/// The floor used to PLAN, before the planner has named a coin — necessarily shape-blind, so it
/// assumes the laddered case at the NETWORK's committed rate, which is the rate every ladder this
/// wallet builds is anchored at (`claim()` ladders every fresh root coin unconditionally, so the
/// laddered case is the default, not the exception).
///
/// This is a planning heuristic, never an admission decision: once the planner names a coin, the
/// exact floor is re-derived from THAT coin via [`split_output_floor`] inside
/// [`split_preflight_pure`], and the plan stands or falls on that. Both `quote_transfer` and
/// `transfer` call this function, so they plan over the same feasible set.
pub(crate) fn planning_split_floor(backup_fee_rate: f64, network: &str) -> u64 {
    let committed_rate = mercurylib::tesr::TesrParams::for_network(network).committed_fee_rate;
    split_output_floors(
        backup_fee_rate,
        ParentShape::Root { fee_rate: committed_rate, split_source_value: 0 },
    )
    .planning()
}

/// How the CHANGE leg's floor is justified, for a refusal message. The two legs no longer
/// necessarily have the same shape, so a message that describes one shape and applies it to both is
/// exactly the false text V5 exists to remove — and neither does one LANE's shape describe another's.
fn change_leg_shape_note(lane: mercuryrustlib::tesr::SplitLane) -> &'static str {
    match mercuryrustlib::tesr::change_leg_role(lane) {
        mercuryrustlib::tesr::SplitLegRole::Piece => {
            "the change is built as a two-tier child on this lane, so it funds an extension + a state rung too"
        }
        mercuryrustlib::tesr::SplitLegRole::SpineTip => {
            "the change is a one-cap spine tip, so it funds ONE rung, not two"
        }
    }
}

/// The IN-LADDER admission rule, in one place: `piece` and `change` are both carved out of `total`
/// (the tier's payload budget) and **each must clear its OWN leg's floor**. Returns the change on
/// success.
///
/// Shared by `in_ladder_pay`, `child_in_ladder_pay` and the quote's preflight, so a piece the quote
/// calls fundable is a piece the executor accepts, and vice versa.
///
/// [V5] Two floors, not one. The old signature took a single number and the message explained it as
/// "each child funds its own extension + state rungs" — which is true of a payee's piece and false
/// of a spine tip, and the arithmetic was false in the more dangerous direction: one floor means the
/// change leg's true one-rung cost can only be applied by lowering the PIECE's floor too, admitting
/// a piece that cannot fund its second rung. `establish_child` discovers that after the parent is
/// already terminal.
pub(crate) fn inladder_amounts_floored(
    total: u64,
    piece_sats: u64,
    floors: SplitFloors,
) -> Result<u64> {
    if piece_sats >= total {
        return Err(anyhow!(
            "payment {piece_sats} sat leaves no change: an in-ladder split of this coin can pay at most {} sat (total {total} minus a viable change output)",
            total.saturating_sub(1)
        ));
    }
    let change_sats = total - piece_sats;
    let piece_short = piece_sats < floors.piece;
    let change_short = change_sats < floors.change;
    if piece_short || change_short {
        let which = match (piece_short, change_short) {
            (true, true) => "both legs fall short",
            (true, false) => "the piece falls short",
            (false, true) => "the change falls short",
            (false, false) => unreachable!(),
        };
        return Err(anyhow!(
            "in-ladder split refused — {which}. The payee's piece ({piece_sats}) must be >= {} sat \
             (it funds its own extension + state rungs) and the change ({change_sats}) must be >= \
             {} sat ({}); both must then clear the {DUST_LIMIT}-sat dust floor. The split total is \
             {total}",
            floors.piece,
            floors.change,
            floors.change_note()
        ));
    }
    Ok(change_sats)
}

/// The whole quote/executor agreement in one pure function, with the
/// two I/O reads (the backup fee rate and the coin's shape) lifted out so it is directly testable.
/// Nothing else in this file re-derives a floor, a split total or an admission verdict.
pub(crate) fn split_preflight_pure(
    backup_rate: f64,
    shape: ParentShape,
    parent_sats: u64,
    piece_sats: u64,
) -> SplitPreflight {
    let floors = split_output_floors(backup_rate, shape);
    let (fee_sats, admission) = match shape {
        ParentShape::Unladdered => (
            split_fee_reserve(parent_sats),
            // The plain lane's two sub-coins have IDENTICAL shape (each carries only its own backup
            // tx), so one number is genuinely right here — and it is the binding one, never the
            // planning one.
            split_amounts_floored(parent_sats, piece_sats, floors.binding())
                .map(|(change, _)| change)
                .map_err(|e| e.to_string()),
        ),
        // [CATS spine batch] A SPINE TIP joins the two in-ladder shapes rather than being refused: its
        // split IS the next spine batch (`SP_{i+1}` over `SP_i.out[K]`, `spine_batch_split`), which
        // carves the same two payload outputs out of the same kind of tier and admits them against
        // the same two floors. The arm is shared rather than duplicated on purpose — the three
        // differ in which builder runs and in `split_source_value` (the tip's is `SP_i.out[K]`, not
        // a `X_m.out[0]`), both of which `ParentShape` already carries.
        ParentShape::Child { fee_rate, .. }
        | ParentShape::Root { fee_rate, .. }
        | ParentShape::SpineTip { fee_rate, .. } => {
            // The split carves TWO payload outputs (piece + change) out of the tier.
            let admission = match shape.split_total(2) {
                Some(total) => {
                    inladder_amounts_floored(total, piece_sats, floors).map_err(|e| e.to_string())
                }
                None => Err(format!(
                    "committed fee at {fee_rate} sat/vB no longer fits: this coin cannot be split in-ladder at all"
                )),
            };
            (in_ladder_split_cost(fee_rate), admission)
        }
    };
    SplitPreflight { shape, floors, fee_sats, admission }
}

/// The coin a payment plan wants to split, together with the BINDING verdict on splitting it.
#[derive(Clone, Debug)]
pub(crate) struct SplitChoice {
    pub statechain_id: String,
    pub piece_sats: u64,
    pub preflight: SplitPreflight,
}

/// A resolved payment plan: which coins go whole, and — when a split is needed — which coin is split
/// and whether that split is actually admissible.
#[derive(Clone, Debug)]
pub(crate) struct PaymentPlan {
    pub plan: Plan,
    /// The split coin and its verdict. `Some` with an `Err` admission when NO candidate could mint
    /// the piece: that refusal is the honest reason the payment cannot be made, and both the quote
    /// and the executor report it.
    pub split: Option<SplitChoice>,
}

/// Everything the quote and the executor must agree on about splitting ONE named coin.
#[derive(Clone, Debug)]
pub(crate) struct SplitPreflight {
    pub shape: ParentShape,
    /// The per-LEG floors [`split_output_floors`] gives for THIS coin.
    pub floors: SplitFloors,
    /// The real cost of this route (the in-ladder split's burnt tier fees, or the plain split's
    /// reserve) — what the quote must report.
    pub fee_sats: u64,
    /// `Ok(change_sats)` when the executor will accept this split; `Err(reason)` — the executor's
    /// own refusal text — when it will not.
    pub admission: std::result::Result<u64, String>,
}

/// The split executor's pure admission guard with an explicit per-leg floor: fee reserve + fit
/// + `min_output` on both sub-coins. Returns `(change_sats, fee_reserve)` when admissible.
pub(crate) fn split_amounts_floored(
    parent_sats: u64,
    piece_sats: u64,
    min_output: u64,
) -> Result<(u64, u64)> {
    // Reserve a miner-fee margin for the (only-on-exit) broadcast of the split tx.
    let fee_reserve = split_fee_reserve(parent_sats);
    if piece_sats + fee_reserve >= parent_sats {
        return Err(anyhow!(
            "piece {piece_sats} + fee reserve {fee_reserve} does not fit in coin of {parent_sats} sats"
        ));
    }
    let change_sats = parent_sats - piece_sats - fee_reserve;
    // Both sub-coin funding outputs must clear `min_output`: the dust floor (audit [9]) plus, when
    // the caller passes the backup-fee floor (`min_split_output`), enough to fund each sub-coin's
    // own backup so neither is stranded (audit: backup-fee floor / GRN-INV-1b).
    if piece_sats < min_output || change_sats < min_output {
        return Err(anyhow!(
            "split would create an unviable output (piece {piece_sats}, change {change_sats}, minimum {min_output}) — each sub-coin must clear the {DUST_LIMIT}-sat dust floor AND fund its own backup; the split tx or a sub-coin backup would be unbroadcastable"
        ));
    }
    Ok((change_sats, fee_reserve))
}

/// The split executor's pure admission guard at the bare dust floor (fee reserve + fit + 330 on
/// both outputs). This is the DUST-only check; callers on the live signing path use
/// [`split_amounts_floored`] with [`min_split_output`] so a sub-coin can also fund its own backup.
/// Called by the invalidation/granularity model tests as the executable dust-boundary spec.
pub(crate) fn split_amounts(parent_sats: u64, piece_sats: u64) -> Result<(u64, u64)> {
    split_amounts_floored(parent_sats, piece_sats, DUST_LIMIT)
}

#[cfg(test)]
mod split_math_tests {
    use super::*;

    // INV-10: fee reserve clamps to [300, 2000] at ~1%; change = parent - piece - reserve.
    #[test]
    fn fee_reserve_and_change() {
        assert_eq!(split_fee_reserve(10_000), 300); // 100 -> floored to 300
        assert_eq!(split_fee_reserve(100_000), 1_000); // 1%
        assert_eq!(split_fee_reserve(1_000_000), 2_000); // 10000 -> capped
        // change is consistent for a valid split
        let parent = 40_000u64;
        let piece = 15_000u64;
        let reserve = split_fee_reserve(parent);
        assert!(piece + reserve < parent);
        assert_eq!(parent - piece - reserve, 40_000 - 15_000 - 400);
    }

    // [P0-4] The in-ladder split's real cost, re-derived from the tier arithmetic rather than
    // asserted as a magic number — and measured against the reserve the quote used to return.
    #[test]
    fn in_ladder_cost_is_the_whole_split_not_just_the_sp_tier() {
        let r = 2.0;
        let rung = mercurylib::tesr::committed_fee(r) + mercurylib::tesr::P2A_VALUE;
        let sp = mercurylib::tesr::committed_fee_for_outputs(2, r) + mercurylib::tesr::P2A_VALUE;
        assert_eq!(rung, 490, "committed fee 250 + P2A 240");
        assert_eq!(sp, 576, "a 2-payload tier costs one extra P2TR output of fee");
        // SP + piece child (ext+state) + change child (ext+state).
        assert_eq!(in_ladder_split_cost(r), sp + 2 * (2 * rung));
        assert_eq!(in_ladder_split_cost(r), 2_536);

        // THE DEFECT: on a 10 000-sat parent the old quote returned the 300-sat floor.
        assert_eq!(split_fee_reserve(10_000), 300);
        assert!(
            in_ladder_split_cost(r) > 6 * split_fee_reserve(10_000),
            "the old quote under-stated the cost by more than 6x"
        );
        // It scales with the committed fee rate, so it cannot go stale against the schedule.
        assert!(in_ladder_split_cost(4.0) > in_ladder_split_cost(2.0));
    }

    /// The tier source value whose 2-output in-ladder split total is exactly `total`.
    fn source_value_for_total(total: u64, rate: f64) -> u64 {
        total + mercurylib::tesr::committed_fee_for_outputs(2, rate) + mercurylib::tesr::P2A_VALUE
    }

    // [B2] ONE FLOOR, ONE SOURCE — asserted as an identity, not as two numbers that happen to match.
    // Every floor in this file now comes from `split_output_floor`; nothing re-derives one.
    #[test]
    fn every_floor_comes_from_split_output_floors() {
        let backup_rate = 2.0;
        let ladder_rate = 2.0;

        // The un-laddered floor IS `min_split_output` — no ladder, no `min_child_value` term. Both
        // legs, because an un-laddered split's two sub-coins genuinely have the same shape.
        assert_eq!(
            split_output_floors(backup_rate, ParentShape::Unladdered),
            SplitFloors {
                piece: min_split_output(backup_rate),
                change: min_split_output(backup_rate),
                lane: None,
            }
        );
        assert_eq!(min_split_output(backup_rate), 554, "dust 330 + a 112-vB backup at 2 sat/vB");

        // A laddered parent raises the PIECE to `min_child_value`, on either lane — the payee's leaf
        // is two-tier everywhere and that is the floor that must never fall.
        let root = ParentShape::Root { fee_rate: ladder_rate, split_source_value: 0 };
        let child = ParentShape::Child { fee_rate: ladder_rate, split_source_value: 0 };
        assert_eq!(mercurylib::tesr::min_child_value(ladder_rate, DUST_LIMIT), 1_310);
        assert_eq!(
            split_output_floors(backup_rate, root).piece,
            1_310,
            "the larger floor binds"
        );
        assert_eq!(
            split_output_floors(backup_rate, child).piece,
            split_output_floors(backup_rate, root).piece,
            "a child re-split and a root split floor the PAYEE identically"
        );
        // [CATS change 2] …and their CHANGE legs no longer agree, because their builders no longer
        // agree. This used to assert the two whole `SplitFloors` equal; that stopped being true the
        // moment `in_ladder_split` started carving a one-cap tip and `child_in_ladder_split` did not.
        // Asserting the difference (rather than deleting the comparison) is what keeps a future
        // change to the CHILD lane's builder from landing without its floor.
        assert_eq!(
            split_output_floors(backup_rate, root).change,
            mercurylib::tesr::min_spine_tip_value(ladder_rate, DUST_LIMIT),
            "the root lane's change is a one-cap spine tip"
        );
        assert_eq!(
            split_output_floors(backup_rate, child).change,
            mercurylib::tesr::min_child_value(ladder_rate, DUST_LIMIT),
            "the child lane's change is still a two-tier piece"
        );
        assert!(
            split_output_floors(backup_rate, root).change
                < split_output_floors(backup_rate, child).change
        );

        // The B2 defect itself: `transfer` planned at 554 while the executor enforced 1 310. Both
        // sides now call `planning_split_floor`, which is `split_output_floor` at the network's
        // committed rate — strictly above the bare backup-fee floor.
        for network in ["regtest", "mainnet"] {
            let planning = planning_split_floor(backup_rate, network);
            let committed = mercurylib::tesr::TesrParams::for_network(network).committed_fee_rate;
            assert_eq!(
                planning,
                split_output_floors(
                    backup_rate,
                    ParentShape::Root { fee_rate: committed, split_source_value: 0 }
                )
                .planning(),
                "[{network}] the planning floor is not a second derivation"
            );
            assert!(
                planning > min_split_output(backup_rate),
                "[{network}] planning at the bare backup-fee floor is the bug (planning {planning})"
            );
        }
    }

    // [B2] THE BOUNDARY, BOTH WAYS. A parent sized so its in-ladder split total is exactly two
    // floors: the piece can be the floor and no more, and the change must also be the floor.
    //
    // `quote_transfer` fills `fundable` from `plan_payment`, and `transfer` executes the SAME
    // call before touching the parent, so "quote agrees with executor" is checked here on the shared
    // core (`split_preflight_pure`) against the exact expressions the executors run inline.
    #[test]
    fn quote_and_executor_agree_at_the_floor_both_ways() {
        let backup_rate = 2.0;
        let ladder_rate = 2.0;
        let floors = split_output_floors(
            backup_rate,
            ParentShape::Root { fee_rate: ladder_rate, split_source_value: 0 },
        );
        let floor = floors.piece;
        assert_eq!(floor, 1_310);

        // A parent whose split total is exactly `piece_floor + change_floor`: the boundary in both
        // directions at once.
        //
        // [V5] Derived from the TWO floors, not from `2 × floor`. Those are the same number today
        // and stop being the same number the instant `change_leg_role()` reports `SpineTip` — at
        // which point `2 × piece_floor` is no longer a boundary for the change leg at all, and this
        // test would have failed on an assertion that had merely gone stale rather than on a defect.
        // Measured, not assumed: flipping the role and re-running is how this line was written.
        let total = floors.piece + floors.change;
        let shape = ParentShape::Root {
            fee_rate: ladder_rate,
            split_source_value: source_value_for_total(total, ladder_rate),
        };
        assert_eq!(shape.split_total(2), Some(total), "the parent sits exactly on the boundary");

        // What `in_ladder_pay` does inline, re-run: shape -> split_total(2) -> split_output_floor ->
        // inladder_amounts_floored. What the quote does, via `plan_payment`: `split_preflight_pure`. They must return
        // the same verdict for every piece across the boundary, in BOTH directions.
        for piece in [floor - 2, floor - 1, floor, floor + 1, floor + 2] {
            let executor = {
                let t = shape.split_total(2).expect("splittable");
                let f = split_output_floors(backup_rate, shape);
                inladder_amounts_floored(t, piece, f).map_err(|e| e.to_string())
            };
            let quote = split_preflight_pure(backup_rate, shape, 0, piece).admission;
            assert_eq!(
                executor.is_ok(),
                quote.is_ok(),
                "piece {piece}: quote says fundable={}, executor says admissible={}",
                quote.is_ok(),
                executor.is_ok()
            );
            assert_eq!(executor.ok(), quote.ok(), "piece {piece}: change differs");
        }

        // ...and the verdicts are the RIGHT ones, or the agreement above is agreement on garbage.
        assert!(
            split_preflight_pure(backup_rate, shape, 0, floor).admission.is_ok(),
            "a piece exactly at the floor, with change exactly at the floor, is admissible"
        );
        assert_eq!(
            split_preflight_pure(backup_rate, shape, 0, floor).admission.unwrap(),
            floors.change,
            "the change is the other half of the boundary — at ITS floor, which is not necessarily \
             the piece's"
        );
        let one_under = split_preflight_pure(backup_rate, shape, 0, floor - 1).admission;
        assert!(one_under.is_err(), "one sat under the floor the PIECE is unviable");
        let one_over = split_preflight_pure(backup_rate, shape, 0, floor + 1).admission;
        assert!(one_over.is_err(), "one sat over, the CHANGE falls under the floor — the other way");

        // The pre-B2 executor floor (the bare backup-fee floor, 554) admitted both of those. That is
        // the exact gap `fundable: true` used to be reported through.
        let old_floor = min_split_output(backup_rate);
        assert!(old_floor < floor, "554 < 1310");
        assert!(
            inladder_amounts_floored(
                total,
                floor - 1,
                SplitFloors {
                    piece: old_floor,
                    change: old_floor,
                    lane: Some(mercuryrustlib::tesr::SplitLane::PlainRoot),
                }
            )
            .is_ok(),
            "the old, lower floor admitted the piece the real executor refuses"
        );

        // A CHILD parent obeys the same RULE — piece and change each against their own floor — but
        // [CATS change 2] it no longer sits on the same NUMBER: its change leg is still a two-tier
        // piece, so its boundary total is `piece + piece`, not `piece + tip`. Deriving the child's
        // boundary from the child's own floors is the point; re-using the root's would have tested
        // the root lane twice and called it coverage of two.
        let child_floors = split_output_floors(
            backup_rate,
            ParentShape::Child { fee_rate: ladder_rate, split_source_value: 0 },
        );
        let child_total = child_floors.piece + child_floors.change;
        let child = ParentShape::Child {
            fee_rate: ladder_rate,
            split_source_value: source_value_for_total(child_total, ladder_rate),
        };
        assert_eq!(child.split_total(2), Some(child_total), "the child sits on ITS own boundary");
        assert!(split_preflight_pure(backup_rate, child, 0, child_floors.piece).admission.is_ok());
        assert!(split_preflight_pure(backup_rate, child, 0, child_floors.piece - 1)
            .admission
            .is_err());
        assert!(split_preflight_pure(backup_rate, child, 0, child_floors.piece + 1)
            .admission
            .is_err());

        // And an UN-LADDERED parent is floored by `min_split_output` alone, from the same function.
        let parent = 10_000u64;
        let unladdered_floor = split_output_floors(backup_rate, ParentShape::Unladdered).binding();
        let pf = split_preflight_pure(backup_rate, ParentShape::Unladdered, parent, unladdered_floor);
        assert_eq!(pf.floors.binding(), unladdered_floor);
        assert!(pf.admission.is_ok(), "at its own floor an un-laddered piece is admissible");
        let plain_under =
            split_preflight_pure(backup_rate, ParentShape::Unladdered, parent, unladdered_floor - 1)
                .admission;
        assert!(plain_under.is_err(), "one sat under the un-laddered floor, refused");
        assert_eq!(pf.fee_sats, split_fee_reserve(parent), "the plain lane quotes its reserve");
        assert_eq!(
            split_preflight_pure(backup_rate, shape, 0, floor).fee_sats,
            in_ladder_split_cost(ladder_rate),
            "the in-ladder lane quotes the real in-ladder cost"
        );
    }

    // [B2] The planner is ADVISORY; the per-coin floor BINDS. `plan_payment` runs
    // `select::plan_with_floor` at the smallest floor any candidate imposes and then judges the coin
    // it named at THAT coin's floor, retrying with the coin marked un-splittable. Planning at a
    // single conservative floor instead would agree with the executor by refusing payments the
    // wallet can actually make — agreement bought with a capability regression, which is not a fix.
    #[test]
    fn the_planner_is_advisory_and_the_per_coin_floor_binds() {
        let backup_rate = 2.0;
        let laddered = ParentShape::Root {
            fee_rate: 2.0,
            split_source_value: source_value_for_total(50_000, 2.0),
        };
        let plain = ParentShape::Unladdered;
        let laddered_floor = split_output_floors(backup_rate, laddered).binding();
        let plain_floor = split_output_floors(backup_rate, plain).binding();
        assert_eq!((plain_floor, laddered_floor), (554, 1_310));

        // The floor `plan_payment` plans at, over a wallet holding one of each.
        let planning = [laddered_floor, plain_floor].into_iter().min().unwrap();
        assert_eq!(planning, plain_floor, "the smallest floor any candidate imposes");

        // A 600-sat piece: the un-laddered coin can mint it; the laddered coin cannot.
        let plain_600 = split_preflight_pure(backup_rate, plain, 10_000, 600).admission;
        assert!(plain_600.is_ok(), "an un-laddered coin mints a 600-sat piece: {plain_600:?}");
        let laddered_600 = split_preflight_pure(backup_rate, laddered, 0, 600).admission;
        assert!(laddered_600.is_err(), "600 is under the in-ladder floor of {laddered_floor}");

        // Planning at the laddered floor would refuse the payment outright; planning at the smallest
        // candidate floor proposes it, and the binding per-coin preflight then decides.
        let coins = vec![crate::select::Candidate { index: 0, amount_sats: 10_000, splittable: true }];
        assert!(matches!(
            crate::select::plan_with_floor(&coins, 600, planning),
            crate::select::Plan::WithSplit { .. }
        ));
        assert!(matches!(
            crate::select::plan_with_floor(&coins, 600, laddered_floor),
            crate::select::Plan::Insufficient { .. }
        ));

        // And the retry itself: once the coin the planner named is marked un-splittable, the planner
        // must stop proposing it — this is what makes `plan_payment`'s loop terminate.
        let exhausted =
            vec![crate::select::Candidate { index: 0, amount_sats: 10_000, splittable: false }];
        assert!(matches!(
            crate::select::plan_with_floor(&exhausted, 600, planning),
            crate::select::Plan::Insufficient { .. }
        ));
    }

    // [P0-4] The quote must plan with the floor the executor enforces. The planner used to be run
    // at the bare dust floor (330) via `select::plan`, while `in_ladder_pay` refuses below
    // `min_child_value` (1 310) — so a piece in between was quoted `fundable` and then refused.
    #[test]
    fn quote_floor_matches_the_executor_floor() {
        let backup_rate = 2.0;
        let committed_rate = 2.0;
        // [B2] re-derived through the ONE source rather than re-spelled here.
        let executor_floor = split_output_floors(
            backup_rate,
            ParentShape::Root { fee_rate: committed_rate, split_source_value: 0 },
        )
        .binding();
        assert_eq!(min_split_output(backup_rate), 554, "dust 330 + a 112-vB backup at 2 sat/vB");
        assert_eq!(mercurylib::tesr::min_child_value(committed_rate, DUST_LIMIT), 1_310);
        assert_eq!(executor_floor, 1_310, "the larger floor binds");

        // A 1 000-sat piece: admitted by the old dust-floor plan, refused by the executor.
        let coins = vec![crate::select::Candidate { index: 0, amount_sats: 10_000, splittable: true }];
        assert!(
            matches!(crate::select::plan(&coins, 1_000), crate::select::Plan::WithSplit { .. }),
            "the dust-floor planner proposes a piece the executor rejects"
        );
        assert!(
            matches!(
                crate::select::plan_with_floor(&coins, 1_000, executor_floor),
                crate::select::Plan::Insufficient { .. }
            ),
            "planning at the executor's floor must not propose a doomed split"
        );
        // Above the floor both agree.
        assert!(matches!(
            crate::select::plan_with_floor(&coins, 2_000, executor_floor),
            crate::select::Plan::WithSplit { .. }
        ));
    }

    // ============================================================================================
    // [CATS / V5] TWO LEGS, TWO FLOORS — and the piece's floor is the one that must never fall.
    // ============================================================================================

    /// **The split is real, not plumbing.** Asserted by exercising `inladder_amounts_floored` with
    /// the two floors DIFFERENT — which is what the wallet computes on the plain root lane now that
    /// `change_leg_role(PlainRoot)` reports `SpineTip` — and showing that a value legal for the
    /// change is illegal for the piece.
    ///
    /// A single-floor implementation cannot express this at all: it either refuses both legs at
    /// 1 310 (correct but 490 sat expensive) or admits both at 820, and admitting a PIECE at
    /// 820 mints a child that cannot fund its second rung. `establish_child` discovers that after
    /// `set_spend_budget` has already terminalized the parent, which is why the guard has to be here
    /// and why it has to be per-leg.
    #[test]
    fn the_two_legs_are_floored_independently() {
        let piece_floor = mercurylib::tesr::min_child_value(2.0, DUST_LIMIT); // 1 310
        let tip_floor = mercurylib::tesr::min_spine_tip_value(2.0, DUST_LIMIT); // 820
        assert_eq!((piece_floor, tip_floor), (1_310, 820));
        let floors = SplitFloors {
            piece: piece_floor,
            change: tip_floor,
            lane: Some(mercuryrustlib::tesr::SplitLane::PlainRoot),
        };

        // A split whose change lands between the two floors: legal as a TIP, illegal as a piece.
        let total = piece_floor + tip_floor;
        assert_eq!(inladder_amounts_floored(total, piece_floor, floors).unwrap(), tip_floor);
        // …and the same number on the PIECE side is refused, by name.
        let e = inladder_amounts_floored(total, tip_floor, floors)
            .expect_err("a piece at the tip's floor cannot fund its second rung");
        let msg = e.to_string();
        assert!(msg.contains("the piece falls short"), "must name the leg, got: {msg}");
        assert!(msg.contains("1310"), "must state the piece's own floor, got: {msg}");

        // The change leg falling short is named as the change, not as "both legs >= N".
        let e = inladder_amounts_floored(total, total - (tip_floor - 1), floors)
            .expect_err("a change under the tip floor is refused");
        assert!(e.to_string().contains("the change falls short"), "got: {e}");

        // And the old text — one floor, one shape, "each child funds its own extension + state" —
        // is gone. It was false for a tip in both halves: wrong count of rungs, wrong owner.
        assert!(!msg.contains("needs both piece"), "the single-floor phrasing must not survive");
    }

    /// **The change floor tracks what THAT LANE's BUILDER emits, and nothing else.**
    /// `split_output_floors` reads `change_leg_role(lane)`, the one function describing the shape
    /// `in_ladder_split` / `child_in_ladder_split` / `cosign_colored_in_ladder_split` actually build.
    ///
    /// Every lane is walked here, and the assertion for each is written against ITS OWN builder — a
    /// lane whose floor moved without its builder is the fail-open direction, and it strands the
    /// parent after `set_spend_budget` rather than merely refusing a payment.
    #[test]
    fn the_change_floor_is_derived_from_the_builders_not_declared() {
        let backup_rate = 2.0;
        for shape in [
            ParentShape::Root { fee_rate: 2.0, split_source_value: 0 },
            ParentShape::Child { fee_rate: 2.0, split_source_value: 0 },
        ] {
            let floors = split_output_floors(backup_rate, shape);
            assert_eq!(
                floors.piece,
                mercurylib::tesr::min_child_value(2.0, DUST_LIMIT),
                "a payee's piece is always a two-tier child — V1's correction is explicit that the \
                 conveyed leaf never becomes a spine"
            );
            let lane = floors.lane.expect("a laddered parent names its lane");
            match mercuryrustlib::tesr::change_leg_role(lane) {
                mercuryrustlib::tesr::SplitLegRole::Piece => {
                    assert_eq!(floors.change, floors.piece, "{lane:?}: a two-tier change");
                    assert_eq!(floors.describe(), "1310", "one number while the legs agree");
                    assert!(floors.change_note().contains("two-tier"));
                }
                mercuryrustlib::tesr::SplitLegRole::SpineTip => {
                    assert_eq!(
                        floors.change,
                        mercurylib::tesr::min_spine_tip_value(2.0, DUST_LIMIT)
                    );
                    assert!(floors.change < floors.piece);
                    assert_eq!(floors.describe(), "piece 1310, change 820");
                    assert!(floors.change_note().contains("ONE rung"));
                }
            }
            // `binding()` and `planning()` are the two honest single-number reductions, and they are
            // used in opposite places: `binding` where one number must cover both legs (the
            // un-laddered lane), `planning` where refusing too much is the bug (the shape-blind
            // planner).
            assert_eq!(floors.binding(), floors.piece.max(floors.change));
            assert_eq!(floors.planning(), floors.piece.min(floors.change));
        }
        // And the two lanes really do give different answers — otherwise the loop above would pass
        // with one arm never taken, which is what the old single-shape version of this test did.
        assert_ne!(
            split_output_floors(backup_rate, ParentShape::Root { fee_rate: 2.0, split_source_value: 0 })
                .change,
            split_output_floors(backup_rate, ParentShape::Child { fee_rate: 2.0, split_source_value: 0 })
                .change
        );
    }

    /// **[CATS change 2 / V5] THE PRODUCER FLIP LOWERED THE CHANGE LEG AND NOTHING ELSE.**
    ///
    /// The [V5] hazard is not "the change floor is wrong" — it is that lowering the change floor is
    /// only expressible, in a single-floor world, by lowering the PIECE's with it. A piece admitted
    /// at 820 cannot fund its second rung, and `establish_child` finds that out *after*
    /// `set_spend_budget` has terminalized the parent: the coin is stranded to unilateral-exit-only,
    /// the payee gets nothing, and the sender cannot retry. So this is asserted as a **non-movement**
    /// of the piece floor, across the whole fee-rate range and on every lane, rather than as one
    /// number at one rate — the shape of the regression is a floor that tracks the wrong leg, and
    /// that is invisible at a single point.
    #[test]
    fn the_producer_flip_lowered_only_the_change_leg() {
        let backup_rate = 2.0;
        for rate in [1.0f64, 2.0, 5.0, 25.0, 100.0] {
            let root = split_output_floors(
                backup_rate,
                ParentShape::Root { fee_rate: rate, split_source_value: 0 },
            );
            let child = split_output_floors(
                backup_rate,
                ParentShape::Child { fee_rate: rate, split_source_value: 0 },
            );
            // THE FLOOR THAT MUST NEVER FALL. A payee's piece is a two-tier child on every lane, so
            // its floor is `min_child_value` (or the backup-fee floor when that binds) — the exact
            // expression it had before change 2, on both lanes.
            let piece = min_split_output(backup_rate)
                .max(mercurylib::tesr::min_child_value(rate, DUST_LIMIT));
            assert_eq!(root.piece, piece, "[{rate} sat/vB] the ROOT lane's piece floor moved");
            assert_eq!(child.piece, piece, "[{rate} sat/vB] the CHILD lane's piece floor moved");
            // …and the change leg is the ONLY thing that moved, and only on the lane whose builder
            // moved with it.
            assert_eq!(
                root.change,
                min_split_output(backup_rate)
                    .max(mercurylib::tesr::min_spine_tip_value(rate, DUST_LIMIT)),
                "[{rate} sat/vB] the root lane's change is a one-cap tip"
            );
            assert_eq!(child.change, piece, "[{rate} sat/vB] the child lane's change is unmoved");
            assert!(root.change <= root.piece, "[{rate} sat/vB] the tip is never the DEARER leg");
            // The planner reduction must not hand the tip's cheaper number to a piece: it is used
            // where refusing too much is the bug, and the per-coin preflight then judges each leg.
            assert!(root.planning() <= root.piece && root.binding() == root.piece);
        }

        // The admission rule itself, at the rate that matters: a piece AT the change leg's floor is
        // still refused, by name. This is the single assertion that would have caught a flip landed
        // without its builder — and it is checked through the executor's own guard, not through the
        // numbers alone.
        let root = split_output_floors(
            backup_rate,
            ParentShape::Root { fee_rate: 2.0, split_source_value: 0 },
        );
        assert_eq!((root.piece, root.change), (1_310, 820));
        let e = inladder_amounts_floored(root.piece + root.change, root.change, root)
            .expect_err("a PIECE at the tip's floor cannot fund its second rung");
        assert!(e.to_string().contains("the piece falls short"), "got: {e}");
        // …while the CHANGE at that same number is admitted — which is the 490 sat change 2 buys.
        assert_eq!(
            inladder_amounts_floored(root.piece + root.change, root.piece, root).unwrap(),
            root.change
        );
        // The pre-flip wallet refused exactly that split. Stated as the capability gained, so a
        // future revert is visible as a loss rather than as a silent tightening.
        assert!(
            inladder_amounts_floored(
                root.piece + root.change,
                root.piece,
                SplitFloors { piece: root.piece, change: root.piece, lane: root.lane },
            )
            .is_err(),
            "before change 2 this split was refused for want of 490 sat of change"
        );
    }

    /// **[V4 + spine batch] A SPINE TIP is priced as laddered — never `Unladdered` — and it SPLITS.**
    ///
    /// The variant exists because falling through to `Unladdered` is three wrong answers from one
    /// missing arm: the cheaper cost model, the LOWER floor, and the route to `split_coin`, which is
    /// the [B1]-unsafe plain split of a coin that IS laddered. This test pins the pricing half in
    /// both directions, and — since the spine batch landed the spine-batch builder — pins that a tip is now
    /// ADMITTED rather than refused, which is the capability the whole change exists for.
    #[test]
    fn a_spine_tip_prices_as_laddered_and_now_splits_as_a_batch() {
        let backup_rate = 2.0;
        let tip = ParentShape::SpineTip { fee_rate: 2.0, split_source_value: 100_000 };
        let root = ParentShape::Root { fee_rate: 2.0, split_source_value: 0 };
        assert_eq!(tip.ladder_fee_rate(), Some(2.0), "a tip carries a ladder");
        let (tf, rf) = (split_output_floors(backup_rate, tip), split_output_floors(backup_rate, root));
        // The NUMBERS match the root lane — both build a two-tier piece and a one-cap change…
        assert_eq!((tf.piece, tf.change), (rf.piece, rf.change));
        assert_eq!((tf.piece, tf.change), (1_310, 820));
        // …but the LANE is its own, and that is deliberate: the two floors agreeing is a fact about
        // two builders, and a lane that read another lane's answer is exactly how the floor and the
        // ladder actually built come apart (hazard 12).
        assert_eq!(tf.lane, Some(mercuryrustlib::tesr::SplitLane::SpineBatch));
        assert_eq!(rf.lane, Some(mercuryrustlib::tesr::SplitLane::PlainRoot));
        assert_ne!(
            tf,
            split_output_floors(backup_rate, ParentShape::Unladdered),
            "the fall-through answer is the dangerous one and must be distinguishable"
        );
        assert_eq!(tip.route(), "spine batch");
        // Its capacity is arithmetic over its FUNDING outpoint (`SP_i.out[K]`), not over the cap.
        assert_eq!(tip.split_total(2), mercurylib::tesr::tier_out_total(100_000, 2, 2.0));

        // THE CAPABILITY. A tip is admitted on exactly the terms a root coin is: the same two
        // floors, the same rule, the same change. Before the spine batch this returned the "builder is not
        // landed" refusal, and every wallet that had made one partial payment was exit-only for the
        // rest of its balance.
        let total = tip.split_total(2).unwrap();
        let admitted = split_preflight_pure(backup_rate, tip, 0, 5_000)
            .admission
            .expect("a spine tip splits as the next batch");
        assert_eq!(admitted, total - 5_000, "the change is the rest of `SP_i.out[K]`");
        assert_eq!(
            split_preflight_pure(backup_rate, tip, 0, 5_000).admission,
            split_preflight_pure(
                backup_rate,
                ParentShape::Root { fee_rate: 2.0, split_source_value: 100_000 },
                0,
                5_000
            )
            .admission,
            "quote and executor must judge a tip exactly as they judge the coin it came from"
        );
        // And the floors still BIND on this lane — a piece at the change leg's floor is refused,
        // which is the [V5] hazard the two numbers exist to keep apart.
        let e = split_preflight_pure(backup_rate, tip, 0, tf.change)
            .admission
            .expect_err("a PIECE at the tip floor cannot fund its second rung");
        assert!(e.contains("the piece falls short"), "got: {e}");
    }
}
