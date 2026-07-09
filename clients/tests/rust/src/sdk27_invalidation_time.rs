//! E2E (invalidation over TIME): the decrementing-locktime ladder, the exit-maturity boundary,
//! the audit-[17] deadline gap, and the SE epoch gate — each measured EXACTLY, not approximately.
//!
//! (a) LADDER OVER HOPS: a 50k coin hops alice->bob->alice->... six times as FULL-amount native
//!     key handovers (no split). After each hop the receiving wallet's coin locktime must equal
//!     L0 - k*interval EXACTLY, where L0 is the deposit backup's locktime (hop backups anchor at
//!     the tx0 locktime, not the current tip — lib/src/transaction.rs::calculate_block_height).
//!     This is REQ-16/INV-2: every prior owner's stale backup matures LATER than the current
//!     owner's, giving the honest owner an exclusive `interval`-block exit window per hop.
//! (b) MATURITY BOUNDARY: after 6 hops the owner's backup locktime is L6 = L0 - 6*interval.
//!     At tip = L6 - 2 a unilateral exit must report complete:false with wait_blocks == 2 EXACT
//!     (never less — the boundary is sharp, REQ-25); two blocks later the exit completes.
//! (c) DEADLINE GAP (open audit [17], encoded as executable knowledge): a coin that hopped k=2
//!     times is then split once by its final owner. `estimate_exit_cost` on the sub-coin reports
//!     `exit_deadline_block = H_deposit + initlock` (the deposit-anchored bound of audit [10] —
//!     equal to the parent's ORIGINAL deposit-backup locktime up to the few blocks between the
//!     funding confirmation and the tx0 co-sign). But the TRUE earliest hostile maturity is the
//!     k=2 hop backup at L0 - 2*interval: the reported deadline is TOO LATE by ~k*interval.
//!     An online receiver is safe (the branch is locktime-free — broadcast immediately); a
//!     receiver offline past the TRUE deadline is exposed. We print a WARN line with the exact
//!     gap and assert the arithmetic. DO NOT paper over this: it is an OPEN audit item.
//! (d) EPOCH OVER TIME (sats path; rgb07 covers the RGB variant): a single-use deposit with a
//!     near epoch deadline confirms, the SE clock passes the deadline, and the coin's FIRST
//!     co-sign attempt is refused with the epoch error (fresh coin — single-use cannot be the
//!     cause). Unilateral exit never needs the SE, so the deadline bounds liveness, not funds.
//!
//! Run: SDK_E2E=27 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

async fn wait_tip_at_least(cc: &ClientConfig, target: u32) -> Result<u32> {
    for _ in 0..120 {
        let t = tip(cc)?;
        if t >= target {
            return Ok(t);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("electrs tip did not reach {target}"))
}

/// Claim exactly one incoming transfer, polling for message propagation.
async fn claim_one(w: &UtexoWallet) -> Result<()> {
    for _ in 0..30 {
        if w.claim().await?.claimed_transfers >= 1 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("receiver claimed no transfer"))
}

/// Deposit `amount` to `w`, mine, and poll claim until the wallet holds the CONFIRMED coin.
async fn deposit_confirmed_coin(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    amount: u32,
) -> Result<mercuryrustlib::Coin> {
    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for _ in 0..60 {
        let _ = w.claim().await?;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
        if let Some(c) = rec.coins.iter().rev().find(|c| {
            c.status == CoinStatus::CONFIRMED
                && c.amount == Some(amount)
                && c.aggregated_address.as_deref() == Some(addr.as_str())
        }) {
            return Ok(c.clone());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("deposit of {amount} sats did not confirm for {wallet_name}"))
}

/// One full-amount native hop (no split): sender -> receiver. Returns the RECEIVING wallet's
/// coin locktime after the claim (the receiver's own newest pre-signed backup).
async fn hop(
    sender: &UtexoWallet,
    receiver: &UtexoWallet,
    receiver_name: &str,
    cc: &ClientConfig,
    receiver_addr: &str,
    id: &str,
    amount: u64,
) -> Result<u32> {
    let r = sender.transfer(receiver_addr, amount).await?;
    assert!(!r.used_split, "a full-amount hop must be a native key handover (no split)");
    assert_eq!(r.coins[0].statechain_id, id, "the hop moves the SAME statechain coin");
    claim_one(receiver).await?;
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, receiver_name).await?;
    let c = rec
        .coins
        .iter()
        .rev()
        .find(|c| c.statechain_id.as_deref() == Some(id) && c.status == CoinStatus::CONFIRMED)
        .ok_or_else(|| anyhow!("{receiver_name} has no confirmed coin {id} after the hop"))?;
    c.locktime.ok_or_else(|| anyhow!("received coin has no locktime"))
}

/// Confirmation height of an on-chain funding tx (via its output script's history).
fn tx_confirm_height(cc: &ClientConfig, txid_str: &str, vout: u32) -> Result<u32> {
    use electrum_client::bitcoin::Txid;
    let txid = Txid::from_str(txid_str)?;
    let tx = cc.electrum_client.transaction_get(&txid)?;
    let spk = &tx
        .output
        .get(vout as usize)
        .ok_or_else(|| anyhow!("no output {vout} in {txid_str}"))?
        .script_pubkey;
    let history = cc.electrum_client.script_get_history(spk)?;
    history
        .iter()
        .find(|h| h.tx_hash == txid && h.height > 0)
        .map(|h| h.height as u32)
        .ok_or_else(|| anyhow!("funding tx {txid_str} not confirmed"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let (initlock, interval) = (si.initlock, si.interval);
    println!("SDK27 - server ladder profile: initlock={initlock} interval={interval} (capacity {} hops)", initlock / interval);

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk27_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk27_bob"), None).await?;
    let alice_addr = alice.get_utexo_address().await?;
    let bob_addr = bob.get_utexo_address().await?;

    // ===== (a) LADDER OVER HOPS: EXACT one-interval decrement per hop ===========================
    let dep = deposit_confirmed_coin(&cc, &alice, "sdk27_alice", 50_000).await?;
    let id = dep.statechain_id.clone().ok_or_else(|| anyhow!("no statechain id"))?;
    let l0 = dep.locktime.ok_or_else(|| anyhow!("deposit coin has no locktime"))?;
    println!("SDK27 - alice deposited 50k; deposit backup locktime L0={l0}; hopping the FULL coin 6 times");

    for k in 1..=6u32 {
        let (sender, receiver, rname, raddr) = if k % 2 == 1 {
            (&alice, &bob, "sdk27_bob", &bob_addr)
        } else {
            (&bob, &alice, "sdk27_alice", &alice_addr)
        };
        let lk = hop(sender, receiver, rname, &cc, raddr, &id, 50_000).await?;
        let expected = l0 - k * interval;
        assert_eq!(
            lk, expected,
            "hop {k}: receiver locktime must decrement EXACTLY one interval per hop (got {lk}, want L0 - {k}*{interval} = {expected})"
        );
        println!("SDK27 - hop {k}: receiver locktime {lk} == L0 - {k}*interval (exclusive window vs previous owner: {interval} blocks)");
    }
    let l6 = l0 - 6 * interval;
    println!("ECON ladder initlock={initlock} interval={interval} hops=6 L0={l0} L6={l6} capacity_hops={}", initlock / interval);

    // ===== (b) MATURITY BOUNDARY: wait_blocks == 2 EXACT at tip = locktime - 2 ==================
    // Final owner after 6 hops is alice (even hop count).
    let est = alice.estimate_exit_cost(&id).await?;
    let t0 = tip(&cc)?;
    assert_eq!(
        est.wait_blocks,
        l6 - t0,
        "wait_blocks must be exactly locktime - tip (flat coin, no branch)"
    );
    assert_eq!(est.branch_txs, 0, "a hopped flat coin has no exit branch");
    assert!(est.wait_blocks > 2, "need room to probe the boundary (wait {})", est.wait_blocks);

    // Mine to EXACTLY locktime - 2.
    let target = l6 - 2;
    let mut now = t0;
    while now < target {
        let chunk = (target - now).min(500);
        bitcoin_core::generatetoaddress(chunk, &core)?;
        now = wait_tip_at_least(&cc, now + chunk).await?;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut exact = None;
    for _ in 0..10 {
        let st = alice.unilateral_exit(Some(vec![id.clone()]), None).await?;
        assert!(!st[0].complete, "the backup must NOT broadcast 2 blocks before its locktime");
        assert!(
            st[0].wait_blocks >= 2,
            "exit must NEVER be reported ready earlier than the boundary (got {})",
            st[0].wait_blocks
        );
        if st[0].wait_blocks == 2 {
            exact = Some(st[0].wait_blocks);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await; // electrs tip propagation
    }
    assert_eq!(
        exact,
        Some(2),
        "at tip = locktime-2 the exit must report wait_blocks == 2 EXACT (REQ-25: the boundary is sharp, not fuzzy)"
    );
    println!("SDK27 - boundary: at tip {target} (= L6-2) exit reports complete:false wait_blocks=2 EXACT");

    bitcoin_core::generatetoaddress(2, &core)?;
    wait_tip_at_least(&cc, l6).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut complete = false;
    for _ in 0..10 {
        let st = alice.unilateral_exit(Some(vec![id.clone()]), None).await?;
        if st[0].complete {
            complete = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(complete, "exactly at tip = locktime the exit must complete");
    bitcoin_core::generatetoaddress(1, &core)?;
    println!("SDK27 - boundary: +2 blocks -> the SAME call completes; the exit window opens exactly at the locktime");

    // ===== (c) DEADLINE GAP on a pre-transferred parent (OPEN audit [17]) =======================
    // Fresh coin, k=2 hops, then the final owner splits it once. The implemented deadline
    // (audit [10]) is deposit-anchored: H_deposit + initlock. The TRUE earliest hostile maturity
    // is the k=2 hop backup at L0 - 2*interval — earlier by ~k*interval. This test pins BOTH
    // numbers so the gap is executable knowledge, not a footnote.
    let dep2 = deposit_confirmed_coin(&cc, &alice, "sdk27_alice", 60_000).await?;
    let pid = dep2.statechain_id.clone().ok_or_else(|| anyhow!("no statechain id"))?;
    let l0c = dep2.locktime.ok_or_else(|| anyhow!("deposit coin has no locktime"))?;
    let fund_txid = dep2.utxo_txid.clone().ok_or_else(|| anyhow!("no utxo_txid"))?;
    let fund_vout = dep2.utxo_vout.ok_or_else(|| anyhow!("no utxo_vout"))?;

    let l1 = hop(&alice, &bob, "sdk27_bob", &cc, &bob_addr, &pid, 60_000).await?;
    assert_eq!(l1, l0c - interval);
    let l2 = hop(&bob, &alice, "sdk27_alice", &cc, &alice_addr, &pid, 60_000).await?;
    assert_eq!(l2, l0c - 2 * interval);
    println!("SDK27 - 60k coin hopped k=2 times (ladder at L0'-2*interval = {l2}); final owner splits it once");

    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let (piece, _change) = alice.split_coin(&pid, 20_000).await?;
    let est = alice.estimate_exit_cost(&piece).await?;
    assert_eq!(est.branch_txs, 1, "sub-coin of a flat parent: 1-tx branch");
    let reported = est
        .exit_deadline_block
        .ok_or_else(|| anyhow!("sub-coin must report an exit deadline"))?;

    // Implementation contract (audit [10]): reported == H_deposit_confirm + initlock — i.e. the
    // parent's ORIGINAL deposit-backup locktime, up to the few blocks between the funding
    // confirmation and the tx0 co-sign (tx0 is co-signed at the tip where the deposit is claimed,
    // confirmation_target blocks after H_deposit; L0' = h_cosign + initlock >= reported).
    let h_dep = tx_confirm_height(&cc, &fund_txid, fund_vout)?;
    assert_eq!(
        reported,
        h_dep + initlock,
        "exit_deadline_block must be the deposit-anchored bound H_deposit + initlock (audit [10])"
    );
    let delta = l0c as i64 - reported as i64; // = h_cosign - h_deposit
    assert!(
        (0..=4).contains(&delta),
        "reported deadline must equal the parent's original deposit-backup locktime up to the confirm/co-sign offset (L0'={l0c}, reported={reported}, delta={delta})"
    );

    // TRUE earliest hostile maturity: the newest parent backup (the k=2 hop rung) matures FIRST.
    let parent_rows =
        mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk27_alice", &pid).await?;
    let newest = parent_rows
        .iter()
        .max_by_key(|b| b.tx_n)
        .ok_or_else(|| anyhow!("no parent backups"))?;
    let true_earliest = mercurylib::utils::get_blockheight(newest)?;
    assert_eq!(
        true_earliest,
        l0c - 2 * interval,
        "the earliest-maturing ancestor backup is the k=2 hop rung at L0' - 2*interval"
    );

    // The gap: the reported deadline is LATER than the true earliest hostile maturity by
    // k*interval minus the confirm/co-sign offset. A watchtower honoring `reported` on a
    // pre-transferred parent would act ~2*interval blocks TOO LATE. Audit item [17], half-closed:
    // `auto_exit_due(margin_blocks)` (batch 5) acts on this deadline, but the receiver still
    // cannot compute the true min-ancestor deadline locally (ancestor backup locktimes are not
    // conveyed) — the margin argument must absorb exactly this gap.
    let gap = reported as i64 - true_earliest as i64;
    assert_eq!(
        gap,
        2 * interval as i64 - delta,
        "gap arithmetic: reported - true == k*interval - (h_cosign - h_deposit)"
    );
    assert!(
        gap >= interval as i64,
        "the reported deadline is too late by at least one full interval — audit [17] is material"
    );
    println!(
        "WARN deadline-gap: reported={reported} true={true_earliest} gap={gap} (k=2 hops x interval={interval}, confirm_delta={delta}) — audit [17] conveyed-locktimes half still open: exit_deadline_block is exact only for never-transferred parents; auto_exit_due's margin_blocks must absorb this gap, else an offline receiver past block {true_earliest} is exposed"
    );

    // ===== (d) EPOCH OVER TIME on the sats path =================================================
    // The lib-level sats deposit API exposes epoch (get_deposit_bitcoin_address_single_use_epoch,
    // clients/libs/rust/src/deposit.rs); the UtexoWallet wrapper does not, so this leg drives
    // mercuryrustlib directly (same DB, same SE). Single-use deposits skip the tx0 co-sign
    // (coin_status.rs), so the coin is FRESH (0 finalized signatures) and a refusal can only be
    // the epoch gate — not single-use.
    let epoch_wallet = "sdk27_epoch";
    let w = mercuryrustlib::wallet::create_wallet(epoch_wallet, &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &w).await?;
    let tkn = prepaid_token(&cc).await?;
    let epoch = now_unix() + 15;
    let sc_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use_epoch(
        &cc,
        epoch_wallet,
        &tkn,
        40_000,
        epoch,
    )
    .await?;
    bitcoin_core::sendtoaddress(40_000, &sc_addr)?;
    for _ in 0..60 {
        if electrs::check_address(&cc, &sc_addr, 40_000).await? {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    let mut coin_e: Option<mercuryrustlib::Coin> = None;
    for _ in 0..30 {
        mercuryrustlib::coin_status::update_coins(&cc, epoch_wallet).await?;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, epoch_wallet).await?;
        if let Some(c) = rec.coins.iter().find(|c| {
            c.aggregated_address.as_deref() == Some(sc_addr.as_str())
                && c.status == CoinStatus::CONFIRMED
        }) {
            coin_e = Some(c.clone());
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let coin_e = coin_e.ok_or_else(|| anyhow!("epoch coin did not confirm"))?;
    println!("SDK27 - sats epoch coin confirmed (single-use, epoch_deadline={epoch}); waiting for the SE clock to pass it");
    while now_unix() < epoch + 2 {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let nonces = mercuryrustlib::create_and_commit_nonces(&coin_e)?;
    let probe =
        mercuryrustlib::transaction::sign_first(&cc, &nonces.sign_first_request_payload).await;
    assert!(probe.is_err(), "the SE must REFUSE a new co-signature past the epoch deadline");
    let msg = probe.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        msg.contains("epoch"),
        "the refusal must be the epoch gate (fresh single-use coin, 0 finalized sigs): {msg}"
    );
    println!("SDK27 - epoch (sats): FIRST co-sign past the deadline REFUSED by the SE ({msg})");

    println!(
        "SDK27 - SUCCESS: invalidation over time is exact — the ladder decrements EXACTLY one interval per hop across 6 hops (L0={l0} -> L6={l6}); the exit-maturity boundary is sharp (wait_blocks==2 at locktime-2, completes at the locktime); the audit-[17] deadline gap on a k=2 pre-transferred parent is REAL and quantified (reported {reported} vs true {true_earliest}, gap {gap} blocks); and the SE epoch gate refuses new co-signs on the plain-sats path while unilateral exit needs no SE."
    );
    Ok(())
}
