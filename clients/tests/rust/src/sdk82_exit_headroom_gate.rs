//! E2E (SDK_E2E=82): **[P0-1]** the exit-headroom admission gate, executed against a live SE.
//!
//! THE DEFECT. The only bound a conveyed split child ever had was `lock_time > tip`
//! (`lib/src/transfer/receiver.rs`, reached from `verify_conveyed_child`). But a child's unilateral
//! exit is a chain of sequential relative timelocks — `2124·d + 2160` blocks on the mainnet schedule
//! — while the funding epoch is only `lockheight_init` (10 000) blocks long. So for the last
//! `WAIT(d)` blocks of EVERY epoch (43% of it at depth 1) a sender could hand a payee a coin that
//! provably could not be materialised before the sender's own flat backup matures, spends the funding
//! outpoint `F`, and voids the whole tree. The census balanced, Model A held, the coin was worthless.
//!
//! THE TEST. Alice deposits and ladders a coin, then the chain is mined forward until her coin's flat
//! backup is only a few dozen blocks from maturing — less than the child's own exit needs. She pays
//! Bob through the in-ladder split, which succeeds (nothing is wrong with the split itself). Bob's
//! claim must then REFUSE the child, naming the shortfall, instead of adopting a coin he could never
//! materialise. A control run on a FRESH epoch proves the gate is not simply refusing everything.
//!
//! **[B1] AND THE GATE'S OWN INPUT.** A gate is only as good as the term it computes with, and this
//! one read `TesrTier::csv` — a plain serde field on the conveyed bundle. The second half of this
//! test takes the child just refused, rewrites nothing but that field to `1` on every tier (no
//! signature, no txid and no `nSequence` is touched), and shows two things: run against the DECLARED
//! chain the gate ADMITS it — the bypass, executed rather than argued — and run against the shipped
//! verifier it is refused BY NAME, because every timelock is now read from the signed transaction's
//! `nSequence` and a bundle whose two copies disagree is rejected rather than believed on either.
//!
//! Run: SDK_E2E=82 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const ALICE: &str = "sdk82_alice";
const BOB: &str = "sdk82_bob";
const DEPOSIT: u64 = 100_000;
const PAY: u64 = 30_000;
/// Blocks of epoch left when the doomed payment is made. Must be BELOW the regtest depth-1 exit wait
/// (`T 0 | X_m 12 | SP 0 | ext 12 | state 24` = 48 blocks of timelock + 5 confirmations = 53 — `SP`
/// is a [CATS] spine tier, so it contributes only its confirmation) and above zero, so the OLD
/// `lock_time > tip` check still passes and only the new gate can refuse.
const HEADROOM_LEFT: u32 = 40;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

/// Deposit + ladder one coin for `w`, returning its statechain id.
async fn laddered_coin(
    w: &UtexoWallet,
    cc: &mercuryrustlib::client_config::ClientConfig,
    name: &str,
) -> Result<String> {
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
    let sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name)
        .await?
        .coins
        .iter()
        .find(|c| {
            c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.amount == Some(DEPOSIT as u32)
        })
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("{name} has no confirmed coin"))?;
    assert!(
        mercuryrustlib::tesr::load(cc, name, &sid).await?.is_some(),
        "the coin must be laddered — this test is about the IN-LADDER split"
    );
    Ok(sid)
}

/// The height at which this coin's flat backup can spend `F` and void the tree: the LOWEST locktime
/// of its backup chain (the current owner's, INV-5).
async fn epoch_expiry(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet_name: &str,
    sid: &str,
) -> Result<u32> {
    let backups = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, sid).await?;
    backups
        .iter()
        .map(|b| mercurylib::utils::get_blockheight(b).map_err(|e| anyhow!("{e:?}")))
        .collect::<Result<Vec<u32>>>()?
        .into_iter()
        .min()
        .ok_or_else(|| anyhow!("coin {sid} has no flat backup"))
}

fn tip(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
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

    // ============================================================================================
    // CONTROL: a coin in a FRESH epoch pays and is adopted normally. Without this the test below
    // would pass just as well against a gate that refuses everything.
    // ============================================================================================
    let control_sid = laddered_coin(&alice, &cc, ALICE).await?;
    let expiry = epoch_expiry(&cc, ALICE, &control_sid).await?;
    let now = tip(&cc)?;
    println!(
        "SDK82 - control coin {control_sid}: epoch expires at {expiry}, tip {now} ({} blocks of \
         headroom)",
        expiry.saturating_sub(now)
    );
    alice
        .in_ladder_pay(
            &control_sid,
            &bob_address,
            PAY,
            mercury_utexo_sdk::transfer::InLadderLatch::None,
        )
        .await?;
    let mut waited = 0;
    loop {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats == PAY {
            break;
        }
        waited += 1;
        if waited > 30 {
            return Err(anyhow!("the CONTROL payment was not adopted — the gate is over-refusing"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    println!("SDK82 - control: a full-epoch child was ADOPTED normally ({PAY} sat)");

    // The exit a depth-1 child needs, derived from the live regtest schedule the coins are built
    // with: `T (no lock) | X_m E0 | SP 0 | ext_child E0 | state_child D0`, one confirmation per
    // tier. Both halves of this flow are measured against it.
    //
    // [CATS] `SP` is a SPINE tier at `SPINE_CSV`, not the state at `D0 − δ`. The window this test
    // steers into is only `required_wait` blocks wide, so this number is not decoration: when the
    // spine landed and this still said `state_csv(1)`, the flow mined to a tip chosen for a 71-block
    // requirement, left 56 blocks, and the gate — correctly — ADMITTED a child that now needs 53.
    // The test read that as "THE DEFECT IS OPEN". Deriving the constant from the same source the
    // builders sign is what keeps the failure honest.
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

    // The SAME predicate the exploit half will be refused by, run over the control child's REAL
    // conveyed material — so the refusal below is known to be discriminating on headroom rather than
    // rejecting every conveyed bundle.
    //
    // ⚠️ TIMING IS PART OF THE CLAIM, so this runs HERE and not at the end of the flow. The exploit
    // half drives the chain to within a few dozen blocks of the DOOMED coin's expiry, and the
    // control coin was deposited first, so its epoch expires EARLIER still: by the time the exploit
    // window opens the control child is genuinely out of headroom too, and asserting it is not would
    // be asserting something false. "A full-epoch child is admitted" is a statement about a full
    // epoch; it is made while there is one.
    let control_rec = mercuryrustlib::tesr::journal_records_for(&cc, ALICE, &control_sid)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("the control split left no journal record"))?;
    let control_bundles = control_rec.bundles()?;
    let alice_coins_at_control = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, ALICE).await?.coins;
    let control_piece = control_rec
        .children
        .iter()
        .position(|jc| {
            !alice_coins_at_control.iter().any(|c| {
                c.statechain_id.as_deref() == Some(jc.statechain_id.as_str())
                    && mercurylib::transaction::get_user_backup_address(c, "regtest".to_string())
                        .map(|a| a == jc.owner_exit_address)
                        .unwrap_or(false)
            })
        })
        .ok_or_else(|| anyhow!("the control split carved no recipient piece"))?;
    let control_headroom = epoch_expiry(&cc, ALICE, &control_sid).await?.saturating_sub(tip(&cc)?);
    assert!(
        control_headroom > required_wait,
        "the control must be checked while it really has a full epoch ({control_headroom} blocks \
         left vs an exit needing {required_wait}) or it proves nothing"
    );
    // (Already adopted by bob, so the census terms have moved on; only the headroom term is under
    // test here — it must not be the reason if this one fails.)
    if let Err(e) = mercuryrustlib::tesr::verify_conveyed_child(
        &cc,
        &control_rec.children[control_piece].owner_exit_address,
        &control_bundles[control_piece],
    )
    .await
    {
        assert!(
            !e.to_string().contains("exit-headroom shortfall"),
            "the control child, with {control_headroom} blocks of its epoch left and an exit needing \
             far fewer, must never be refused for headroom: {e}"
        );
    }
    println!(
        "SDK82 - control: the gate ADMITS it on headroom ({control_headroom} blocks of epoch left)"
    );

    // ============================================================================================
    // THE EXPLOIT: the same payment, made when the epoch is nearly over.
    // ============================================================================================
    let doomed_sid = laddered_coin(&alice, &cc, ALICE).await?;
    let expiry = epoch_expiry(&cc, ALICE, &doomed_sid).await?;
    let now = tip(&cc)?;

    // Make the payment FIRST, while the epoch is still young. Nothing in the split itself is wrong
    // — that is exactly why the missing gate was exploitable — and doing it now keeps the several
    // SE round-trips it needs out of the narrow window opened below.
    alice
        .in_ladder_pay(
            &doomed_sid,
            &bob_address,
            PAY,
            mercury_utexo_sdk::transfer::InLadderLatch::None,
        )
        .await?;
    println!(
        "SDK82 - alice carved a child out of {doomed_sid} (epoch expires at {expiry}, tip {now})"
    );

    // Now walk the chain to the end of the coin's epoch. ONE bulk mine, then poll electrs; the final
    // stretch is closed in small steps because the usable window is only `required_wait` blocks wide
    // and this regtest chain may be mined concurrently by other work.
    let bulk_target = (expiry - HEADROOM_LEFT).saturating_sub(400);
    if bulk_target > now {
        println!("SDK82 - mining {} blocks toward the end of the epoch", bulk_target - now);
        bitcoin_core::generatetoaddress(bulk_target - now, &core)?;
    }
    let mut waited = 0;
    while tip(&cc)? < bulk_target {
        waited += 1;
        if waited > 900 {
            return Err(anyhow!("electrs did not catch up to {bulk_target}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut steps = 0;
    // `mined_to` is the height we have ALREADY asked for. Deciding to mine again from a tip that
    // electrs has not indexed yet would mine the same stretch twice and sail straight past the
    // window — so a pending mine is waited out, never re-issued.
    let mut mined_to = 0u32;
    let headroom = loop {
        let t = tip(&cc)?;
        let h = expiry.saturating_sub(t);
        if h == 0 {
            return Err(anyhow!(
                "the epoch expired before the window could be used — this regtest chain is being \
                 mined concurrently; re-run when it is quiet"
            ));
        }
        if h < required_wait {
            break h;
        }
        if t >= mined_to && h > required_wait + 40 {
            let want = expiry - (required_wait - 15);
            mined_to = want;
            bitcoin_core::generatetoaddress(want - t, &core)?;
        }
        steps += 1;
        if steps > 3_000 {
            return Err(anyhow!("could not bring the tip into the exit-headroom window"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    // The OLD check (`lock_time > tip`) still passes here: the epoch has NOT expired. Only the new
    // gate can refuse this coin, and it must.
    println!(
        "SDK82 - in the window: {headroom} blocks of epoch left (expiry {expiry}), but this child's \
         exit needs {required_wait}"
    );

    // Bob must REFUSE it. `claim()` swallows a per-message validation failure (other transfers must
    // still land) and only prints it, so the refusal is read from the receiver's verifier directly,
    // run over the REAL conveyed material: the piece bundle as rebuilt from the split's journal.
    let rec = mercuryrustlib::tesr::journal_records_for(&cc, ALICE, &doomed_sid)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("the doomed split left no journal record"))?;
    let bundles = rec.bundles()?;
    // The piece is the child that does NOT pay a key of alice's own wallet.
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
    let payee = rec.children[piece_idx].owner_exit_address.clone();
    let err = mercuryrustlib::tesr::verify_conveyed_child(&cc, &payee, &bundles[piece_idx])
        .await
        .err()
        .ok_or_else(|| {
            anyhow!(
                "THE DEFECT IS OPEN: the receiver's verifier ACCEPTED a child whose exit cannot \
                 complete before the funding epoch expires"
            )
        })?;
    let msg = err.to_string();
    println!("SDK82 - the receiver's verifier REFUSED the child: {msg}");
    assert!(
        msg.contains("exit-headroom shortfall"),
        "the refusal must be the headroom gate, not some unrelated failure: {msg}"
    );
    assert!(msg.contains("short by"), "the refusal must state the shortfall in blocks: {msg}");

    // ============================================================================================
    // [B1] THE GATE'S OWN INPUT, FORGED — the bypass that made the refusal above optional.
    //
    // A `TesrTier` carries its relative timelock TWICE: as the serde field `csv` that travels with
    // the conveyed bundle, and inside the signed transaction's `nSequence`, which is the only copy
    // Bitcoin enforces. The gate read the FIELD. So the sender of the very coin just refused had
    // only to declare `csv: 1` on each tier — touching no signature, no txid and no nSequence — for
    // the requirement to collapse from the real 53 blocks to 9 and the coin to be admitted.
    // ============================================================================================
    let mut forged = bundles[piece_idx].clone();
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
    forged.child_extension.csv = Some(1);
    forged.child_state.csv = Some(1);
    // Nothing that is signed has changed: same tier transactions, byte for byte.
    for (a, b) in mercuryrustlib::tesr::child_exit_chain(&forged)
        .iter()
        .zip(mercuryrustlib::tesr::child_exit_chain(&bundles[piece_idx]).iter())
    {
        assert_eq!(a.0, b.0, "the forgery must touch ONLY the declared field");
    }

    // THE COUNTERFACTUAL, run rather than asserted in prose: feed the gate the DECLARED chain — what
    // it used to read — and watch it admit the coin it had just refused.
    let declared: Vec<Option<u16>> = mercuryrustlib::tesr::child_exit_chain(&forged)
        .into_iter()
        .map(|(_, csv)| csv)
        .collect();
    let declared_required = mercurylib::transfer::receiver::exit_wait_blocks(&declared);
    let now = tip(&cc)?;
    if expiry <= now {
        return Err(anyhow!(
            "the epoch expired before the B1 counterfactual could be run — this regtest chain is \
             being mined concurrently; re-run when it is quiet"
        ));
    }
    assert!(
        declared_required < required_wait,
        "the forgery must actually shrink the requirement ({declared_required} vs {required_wait})"
    );
    let would_have_passed =
        mercurylib::transfer::receiver::check_exit_headroom(&declared, now, expiry);
    assert!(
        would_have_passed.is_ok(),
        "COUNTERFACTUAL VACUOUS: with {} blocks of epoch left the declared chain ({declared_required} \
         blocks) would have been refused anyway, so this run proves nothing about the bypass — \
         re-run on a quiet chain: {would_have_passed:?}",
        expiry - now
    );
    println!(
        "SDK82 - [B1] the OLD gate would have ADMITTED this child: declared exit {declared_required} \
         blocks vs {} of epoch left (the SIGNED exit really needs {required_wait})",
        expiry - now
    );

    // The receiver now refuses it, and the refusal names the mismatch rather than silently
    // preferring one of the two values.
    let err = mercuryrustlib::tesr::verify_conveyed_child(&cc, &payee, &forged)
        .await
        .err()
        .ok_or_else(|| {
            anyhow!(
                "B1 IS OPEN: the receiver ACCEPTED a child whose declared timelocks contradict the \
                 nSequence its own signatures commit to — the headroom gate is bypassable by a \
                 sender-declared field"
            )
        })?;
    let msg = err.to_string();
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

    // (The control child's headroom was checked against the live verifier at the top of the flow,
    // while its epoch was still full — see the note there for why it cannot be re-checked here.)

    // [B1] And the binding is not simply refusing everything: the HONEST control child's two copies
    // agree, and the requirement read off its signatures is the live schedule's.
    let bound = mercuryrustlib::tesr::child_exit_chain_bound(&control_bundles[control_piece])
        .expect("an honest bundle's declared timelocks match its signatures");
    let bound_csvs: Vec<Option<u16>> = bound.iter().map(|(_, csv)| *csv).collect();
    assert_eq!(bound.len(), 5, "T | X_m | SP | ext_child | state_child");
    assert_eq!(
        mercurylib::transfer::receiver::exit_wait_blocks(&bound_csvs),
        required_wait,
        "the SIGNED chain of an honest child is the live regtest schedule"
    );
    // The same forgery on the honest child is refused too — the binding is a property of the
    // bundle, not of the coin's headroom.
    let mut forged_control = control_bundles[control_piece].clone();
    forged_control.child_state.csv = Some(1);
    let ctrl_err = mercuryrustlib::tesr::child_exit_chain_bound(&forged_control)
        .err()
        .ok_or_else(|| anyhow!("B1 IS OPEN on a full-epoch child: the forgery was accepted"))?;
    assert!(
        ctrl_err.to_string().contains("child state"),
        "the refusal must name the forged tier: {ctrl_err}"
    );
    println!(
        "SDK82 - [B1] the honest control child binds cleanly ({} blocks of signed exit), and the \
         same one-field forgery on it is refused: {ctrl_err}",
        mercurylib::transfer::receiver::exit_wait_blocks(&bound_csvs)
    );

    // And the claim path agrees: bob's balance does not grow.
    for _ in 0..3 {
        bob.claim().await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        bob.get_balance().await?.available_sats,
        PAY,
        "bob must still hold ONLY the control payment — the doomed child must never be adopted"
    );

    println!(
        "SDK82 - SUCCESS [B1]: the headroom requirement is now computed from the SIGNED nSequence of \
         every tier, so the refusal above cannot be lifted by re-declaring the bundle's `csv` \
         fields — the forged child is refused by name, and an honest one still binds and is still \
         admitted."
    );
    println!(
        "SDK82 - SUCCESS [P0-1]: the receiver now refuses a conveyed child whose unilateral exit \
         cannot complete before the funding epoch expires, naming the shortfall, while an identical \
         payment in a fresh epoch is adopted normally. The window this closes is the last WAIT(d) \
         blocks of every epoch — 43% of it at mainnet depth 1 — during which every in-ladder payment \
         handed the payee a coin that provably could not be materialised."
    );
    Ok(())
}
