//! E2E (SDK_E2E=82): **a conveyed child has NO EPOCH TO RUN OUT OF, and the exit it is measured by
//! is read from the SIGNED `nSequence` of every tier [B1].**
//!
//! THE DEFECT THIS FILE WAS BORN FOR — [P0-1], the exit-headroom gate — was a property of the flat
//! backup: a child's unilateral exit is a chain of relative timelocks, while the sender's flat
//! backup matured at an ABSOLUTE height (`H_deposit + lockheight_init`), spent the funding outpoint
//! `F` and voided the whole tree. For the last `WAIT(d)` blocks of every epoch a payee could be
//! handed a child that provably could not be materialised, so the receiver had to refuse a child
//! whose exit did not fit in the epoch that was left. That backup no longer exists: a coin's only
//! exit material is its TES-R ladder, `coin.locktime` is `None` for life, nothing on the coin ever
//! matures on its own, and the headroom gate is not consulted. What replaced the epoch as the
//! admission bound is the split-depth cap measured against `initlock` as a FIXED window — a
//! property of the child's SHAPE, not of the calendar.
//!
//! THE TEST, re-derived onto that rule:
//!   * **CONTROL.** Alice's fresh laddered coin pays Bob through the in-ladder split; the receiver's
//!     verifier ADMITS the piece (run over the real conveyed material, before Bob claims it) and
//!     Bob adopts it. The piece's signed exit chain is the live regtest schedule
//!     (`T 0 | X_m 12 | SP 0 | ext 12 | state 24`, five tiers, 53 blocks) — read from `nSequence`.
//!   * **[B1] THE GATE'S OWN INPUT, FORGED.** A `TesrTier` carries its relative timelock twice — as
//!     the serde field `csv` and inside the signed transaction's `nSequence`, the only copy Bitcoin
//!     enforces. The same piece with only its declared `csv` fields rewritten to `1` (no signature,
//!     txid or nSequence touched) is refused BY NAME: every timelock is bound to the signed copy and
//!     a bundle whose two copies disagree is rejected rather than believed on either.
//!   * **NO EPOCH.** A second coin is deposited and laddered, then the chain is mined `initlock + 60`
//!     blocks past it — well past the height at which the old flat backup would have matured and
//!     the old gate would have refused EVERY child of it. `estimate_exit_cost` reports
//!     `wait_blocks: 0` and no deadline before the mining, the payment is made AFTER it, the
//!     verifier admits the piece with no "exit-headroom shortfall", Bob adopts it, and the aged
//!     coin's `F` is still unspent: nothing matured, because nothing on the coin can.
//!
//! Run: SDK_E2E=82 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::is_outpoint_spent;

const ALICE: &str = "sdk82_alice";
const BOB: &str = "sdk82_bob";
const DEPOSIT: u64 = 100_000;
const PAY: u64 = 30_000;
/// Blocks mined PAST `initlock` before the aged payment. Under the old rule the flat backup matured
/// AT `initlock`; anything past it is a coin whose every child the old gate refused outright.
const PAST_EPOCH: u32 = 60;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

/// Deposit + ladder one coin for `w`, returning its statechain id and its funding outpoint.
async fn laddered_coin(
    w: &UtexoWallet,
    cc: &mercuryrustlib::client_config::ClientConfig,
    name: &str,
) -> Result<(String, String, u32)> {
    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        w.claim().await?;
        if w.get_balance().await?.available_sats >= DEPOSIT {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("{name} deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name)
        .await?
        .coins
        .into_iter()
        .find(|c| {
            c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.amount == Some(DEPOSIT as u32)
        })
        .ok_or(anyhow!("{name} has no confirmed coin"))?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("{name}'s coin has no statechain id"))?;
    assert!(
        mercuryrustlib::tesr::load(cc, name, &sid).await?.is_some(),
        "the coin must be laddered — this test is about the IN-LADDER split"
    );
    assert!(coin.locktime.is_none(), "a laddered coin has no absolute calendar: locktime must be None");
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("coin has no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("coin has no F vout"))?;
    Ok((sid, f_txid, f_vout))
}

fn tip(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

/// Mine the chain to `target` in chunks and wait until electrs has indexed it. A pending chunk is
/// waited out, never re-issued, so a slow index cannot double-mine the stretch.
async fn mine_to(cc: &mercuryrustlib::client_config::ClientConfig, core: &str, target: u32) -> Result<()> {
    let mut mined_to = tip(cc)?;
    while mined_to < target {
        let step = (target - mined_to).min(200);
        bitcoin_core::generatetoaddress(step, core)?;
        mined_to += step;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let mut waited = 0;
    while tip(cc)? < target {
        waited += 1;
        if waited > 900 {
            return Err(anyhow!("electrs did not catch up to {target}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(())
}

/// The recipient's piece of `sid`'s most recent split — the child that does NOT pay a key of
/// alice's own wallet — as (payee exit address, rebuilt piece bundle).
async fn recipient_piece(
    cc: &mercuryrustlib::client_config::ClientConfig,
    sid: &str,
) -> Result<(String, mercuryrustlib::tesr::ChildTesrBundle)> {
    let rec = mercuryrustlib::tesr::journal_records_for(cc, ALICE, sid)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("the split of {sid} left no journal record"))?;
    let alice_coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, ALICE).await?.coins;
    let piece_idx = rec
        .children
        .iter()
        .position(|jc| {
            !alice_coins.iter().any(|c| {
                c.statechain_id.as_deref() == Some(jc.statechain_id.as_str())
                    && mercurylib::transaction::get_user_backup_address(c, "regtest".to_string())
                        .map(|a| a == jc.owner_exit_address)
                        .unwrap_or(false)
            })
        })
        .ok_or_else(|| anyhow!("the split carved no recipient piece"))?;
    // [CATS change 2] Rebuild THAT leg as a piece. A root-lane record also holds the sender's spine
    // tip, which `bundles()` refuses wholesale — and `piece_bundle` refuses the tip's own index by
    // name, so this can only ever be the recipient's leaf.
    let payee = rec.children[piece_idx].owner_exit_address.clone();
    let bundle = rec.piece_bundle(piece_idx)?;
    Ok((payee, bundle))
}

/// Poll bob's claim until his balance is exactly `want`.
async fn claim_until(bob: &UtexoWallet, want: u64, what: &str) -> Result<()> {
    let mut waited = 0;
    loop {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats == want {
            return Ok(());
        }
        waited += 1;
        if waited > 30 {
            return Err(anyhow!("{what}: bob's balance did not reach {want}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = wallet(ALICE).await?;
    let bob = wallet(BOB).await?;
    let bob_address = bob.get_utexo_address().await?;

    // The exit a depth-1 child needs, derived from the live regtest schedule the coins are built
    // with: `T (no lock) | X_m E0 | SP 0 | ext_child E0 | state_child D0`, one confirmation per
    // tier. [CATS] `SP` is a SPINE tier at `SPINE_CSV`, not a state at `D0 − δ`.
    let required_wait: u32 = {
        let p = mercurylib::tesr::TesrParams::regtest();
        mercurylib::transfer::receiver::exit_wait_blocks(&[
            None,
            Some(p.ext_csv(0)),
            Some(mercuryrustlib::tesr::SPINE_CSV),
            Some(p.ext_csv(0)),
            Some(p.state_csv(0)),
        ])
    };
    assert_eq!(required_wait, 53, "regtest depth-1 exit: 48 blocks of CSV + 5 confirmations");

    // ============================================================================================
    // CONTROL: a fresh coin pays, the verifier admits the piece over its REAL conveyed material,
    // and bob adopts it. Without this the refusal below could be a verifier that refuses everything.
    // ============================================================================================
    let (control_sid, _, _) = laddered_coin(&alice, &cc, ALICE).await?;
    alice
        .in_ladder_pay(
            &control_sid,
            &bob_address,
            PAY,
            mercury_utexo_sdk::transfer::InLadderLatch::None,
        )
        .await?;
    let (control_payee, control_piece_bundle) = recipient_piece(&cc, &control_sid).await?;
    assert!(
        control_piece_bundle.parent_flat_backups.is_empty(),
        "a conveyed child carries NO flat backup beside its ladder (parent_flat_backups must be empty)"
    );
    let admitted = mercuryrustlib::tesr::verify_conveyed_child(&cc, &control_payee, &control_piece_bundle)
        .await
        .map_err(|e| anyhow!("the CONTROL child must be ADMITTED by the receiver's verifier: {e:#}"))?;
    // The census-bound value is the piece MINUS the child's own two tiers, not the nominal: the
    // child funds an extension and a state off the piece and each burns `committed_fee(rate) +
    // P2A_VALUE`. Derived from the conveyed bundle's rate so it stays exact if the constants move.
    let control_rate = control_piece_bundle.parent.fee_rate;
    let control_after_x = mercurylib::tesr::tier_out_value(PAY, control_rate)
        .ok_or_else(|| anyhow!("the {PAY} piece is too small for the child's extension tier"))?;
    let control_expected = mercurylib::tesr::tier_out_value(control_after_x, control_rate)
        .ok_or_else(|| anyhow!("the {PAY} piece is too small for the child's state tier"))?;
    assert!(control_expected < PAY, "each child tier burns a committed fee plus the anchor");
    assert_eq!(
        admitted, control_expected,
        "the verifier's census-bound exit value is the piece's EXIT-REACHABLE value: {PAY} minus \
         the child's own two tiers"
    );
    // [B1] The honest child's two copies of every timelock agree, and the requirement read off its
    // SIGNATURES is the live schedule's.
    let bound = mercuryrustlib::tesr::child_exit_chain_bound(&control_piece_bundle)
        .map_err(|e| anyhow!("an honest bundle's declared timelocks match its signatures: {e:#}"))?;
    let bound_csvs: Vec<Option<u16>> = bound.iter().map(|(_, csv)| *csv).collect();
    assert_eq!(bound.len(), 5, "T | X_m | SP | ext_child | state_child");
    assert_eq!(
        mercurylib::transfer::receiver::exit_wait_blocks(&bound_csvs),
        required_wait,
        "the SIGNED chain of an honest child is the live regtest schedule"
    );
    claim_until(&bob, PAY, "CONTROL").await?;
    println!(
        "SDK82 - control: the verifier ADMITTED the child ({admitted} sat, signed exit {required_wait} blocks) and bob adopted it"
    );

    // ============================================================================================
    // [B1] THE GATE'S OWN INPUT, FORGED. Rewrite ONLY the declared `csv` field on every tier of the
    // control piece — no signature, txid or nSequence is touched — so that a verifier reading the
    // FIELD would see a 9-block exit where the signatures commit to 53.
    // ============================================================================================
    let mut forged = control_piece_bundle.clone();
    for lvl in forged.parent.levels.iter_mut() {
        lvl.extension.csv = Some(1);
        lvl.state.csv = Some(1);
    }
    for seg in forged.ancestors.iter_mut() {
        // [CATS] `ChildSegment::extension` is an `Option` — `None` is a SPINE segment, which has one
        // tier and no extension rung to forge. Forge whatever is there; the point of the fixture is
        // that ONLY declared fields move, and a segment with no extension has one fewer to move.
        if let Some(ext) = seg.extension.as_mut() {
            ext.csv = Some(1);
        }
        seg.state.csv = Some(1);
    }
    forged.child_extension.as_mut().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").csv = Some(1);
    forged.child_state.csv = Some(1);
    // Nothing that is signed has changed: same tier transactions, byte for byte.
    for (a, b) in mercuryrustlib::tesr::child_exit_chain(&forged)
        .iter()
        .zip(mercuryrustlib::tesr::child_exit_chain(&control_piece_bundle).iter())
    {
        assert_eq!(a.0, b.0, "the forgery must touch ONLY the declared field");
    }
    let declared: Vec<Option<u16>> = mercuryrustlib::tesr::child_exit_chain(&forged)
        .into_iter()
        .map(|(_, csv)| csv)
        .collect();
    let declared_required = mercurylib::transfer::receiver::exit_wait_blocks(&declared);
    assert!(
        declared_required < required_wait,
        "the forgery must actually shrink the DECLARED requirement ({declared_required} vs {required_wait})"
    );

    // The receiver refuses it, and the refusal names the mismatch rather than silently preferring
    // one of the two values.
    let err = mercuryrustlib::tesr::verify_conveyed_child(&cc, &control_payee, &forged)
        .await
        .err()
        .ok_or_else(|| {
            anyhow!(
                "B1 IS OPEN: the receiver ACCEPTED a child whose declared timelocks contradict the \
                 nSequence its own signatures commit to — any bound computed from the declared \
                 field is bypassable by the sender"
            )
        })?;
    let msg = format!("{err:#}");
    println!("SDK82 - [B1] the receiver's verifier REFUSED the forgery: {msg}");
    assert!(
        msg.contains("declared-CSV mismatch"),
        "the refusal must NAME the mismatch, not report some downstream symptom: {msg}"
    );
    assert!(msg.contains("nSequence"), "the refusal must say which copy is authoritative: {msg}");
    assert!(
        msg.contains("a relative timelock of 1 block(s)"),
        "the refusal must quote what was DECLARED: {msg}"
    );
    assert!(
        msg.contains("parent level 0 extension"),
        "the refusal must name the tier that lied: {msg}"
    );
    // The binding is a property of the bundle, not of the coin: one forged field is enough.
    let mut forged_one = control_piece_bundle.clone();
    forged_one.child_state.csv = Some(1);
    let one_err = mercuryrustlib::tesr::child_exit_chain_bound(&forged_one)
        .err()
        .ok_or_else(|| anyhow!("B1 IS OPEN: a single forged `csv` field was accepted by the binding"))?;
    assert!(
        one_err.to_string().contains("child state"),
        "the refusal must name the forged tier: {one_err}"
    );
    println!("SDK82 - [B1] one forged field on the honest child is refused too: {one_err}");

    // ============================================================================================
    // NO EPOCH: the same payment from a coin aged PAST `initlock`. Under the old rule this coin's
    // flat backup matured at `H_deposit + initlock`, every child of it was refused for headroom
    // from `initlock − 53` on, and `F` was spendable by the sender from `initlock` on. Under the
    // rule nothing on the coin matures: the payment is admitted, adopted, and `F` stays unspent.
    // ============================================================================================
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;
    let (aged_sid, aged_f_txid, aged_f_vout) = laddered_coin(&alice, &cc, ALICE).await?;
    let est = alice.estimate_exit_cost(&aged_sid).await?;
    assert_eq!(
        est.wait_blocks, 0,
        "a laddered coin's exit has NO wait before it can start: `wait_blocks` must be 0, got {} (a flat backup \
         would have reported ~{initlock})",
        est.wait_blocks
    );
    assert!(
        est.exit_deadline_block.is_none(),
        "a laddered coin has no absolute deadline: exit_deadline_block must be None, got {:?}",
        est.exit_deadline_block
    );
    let born = tip(&cc)?;
    let target = born + initlock + PAST_EPOCH;
    println!(
        "SDK82 - aged coin {aged_sid} laddered at tip {born} (wait_blocks 0, no deadline); mining to {target} \
         (initlock {initlock} + {PAST_EPOCH})"
    );
    mine_to(&cc, &core, target).await?;
    let now = tip(&cc)?;
    assert!(now >= born + initlock, "the chain must be past the old epoch: tip {now}, born {born}, initlock {initlock}");
    assert!(
        !is_outpoint_spent(&cc, &aged_f_txid, aged_f_vout),
        "NOTHING MATURED: the aged coin's F must still be unspent after {initlock}+ blocks — there is no flat backup to spend it"
    );

    alice
        .in_ladder_pay(
            &aged_sid,
            &bob_address,
            PAY,
            mercury_utexo_sdk::transfer::InLadderLatch::None,
        )
        .await
        .map_err(|e| anyhow!("the SENDER refused a payment from a coin merely because it is old — there is no epoch: {e:#}"))?;
    let (aged_payee, aged_piece) = recipient_piece(&cc, &aged_sid).await?;
    assert!(aged_piece.parent_flat_backups.is_empty(), "no flat backup rides with the aged child either");
    let aged_admitted = mercuryrustlib::tesr::verify_conveyed_child(&cc, &aged_payee, &aged_piece)
        .await
        .map_err(|e| {
            let m = format!("{e:#}");
            if m.contains("exit-headroom shortfall") {
                anyhow!("THE CALENDAR IS BACK: the receiver refused a child of a {}-block-old coin for exit headroom — there is no epoch to run out of: {m}", now - born)
            } else {
                anyhow!("the receiver must ADMIT a child of a coin {} blocks old exactly as it admits a fresh one: {m}", now - born)
            }
        })?;
    // The SAME derived value as the control's: an aged coin's child is priced exactly like a fresh
    // one, which is the whole point of this test. Asserting the nominal here would assert the
    // pre-ladder shape, where a piece kept its full value and was exited by a flat backup.
    assert_eq!(
        aged_admitted, control_expected,
        "the aged child's census-bound exit value is the SAME as the fresh one's: {PAY} minus the \
         child's own two tiers. Age changes nothing, because there is no epoch to age against"
    );
    claim_until(&bob, 2 * PAY, "AGED").await?;
    assert!(
        !is_outpoint_spent(&cc, &aged_f_txid, aged_f_vout),
        "the in-ladder split is off-chain: the aged coin's F is still unspent after the payment"
    );
    println!(
        "SDK82 - NO EPOCH: a child of a coin {} blocks old ({initlock}+{PAST_EPOCH} past its deposit) was ADMITTED and adopted; F unspent",
        now - born
    );

    println!(
        "SDK82 - SUCCESS [B1]: every timelock the receiver measures is read from the SIGNED nSequence \
         of the tier, so a sender cannot shrink a child's exit by re-declaring the bundle's `csv` \
         fields — the forged child is refused by name, and an honest one binds and is admitted."
    );
    println!(
        "SDK82 - SUCCESS [NO EPOCH]: a conveyed child has no funding epoch to fit inside. The flat \
         backup that used to mature at H_deposit + initlock and void the tree does not exist, so a \
         payment from a coin {initlock}+{PAST_EPOCH} blocks old is admitted exactly like a fresh one, \
         and the coin's F is never spent by anything that merely aged."
    );
    Ok(())
}
