//! E2E (security): an **auto-racing watchtower** turns the sdk13 scenario-2 loss into a defended win,
//! and quantifies the two things an honest receiver needs — HOW it knows to exit, and HOW MUCH fee.
//!
//! Key insight: for a split/branch coin the exit (the branch) is locktime-FREE, while the attacker's
//! only stale state (the parent deposit backup) is locktimed at L_old. So inside the window
//! [now, L_old) the attacker literally cannot broadcast (non-final) — the watcher's exit is
//! UNCONTESTED and wins at a NORMAL fee, with no bidding war. The receiver therefore does not need to
//! "win a race"; it only needs to exit before the deadline L_old. The watchtower's whole job is:
//! know the deadline, watch the height, and broadcast the branch a safety margin before L_old.
//!
//! Run: SDK_E2E=14 ML_NETWORK=regtest cargo run

use std::str::FromStr;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_spark_sdk::{SdkConfig, SparkWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

const SAFETY_MARGIN: u32 = 40; // exit this many blocks before the deadline

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

struct Stale {
    o_txid: String,
    o_vout: u32,
    backup_hex: String,
    locktime: u32,
}

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> bool {
    use electrum_client::bitcoin::Txid;
    let raw = match cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()) {
        std::result::Result::Ok(r) => r,
        _ => return true,
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk).unwrap_or_default();
    !listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout)
}

fn height(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

async fn capture_stale(cc: &ClientConfig, wallet_name: &str, amount: u32) -> Result<Stale> {
    let wallet = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let parent = wallet
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(amount))
        .ok_or_else(|| anyhow!("no confirmed {amount}-sat parent in {wallet_name}"))?
        .clone();
    let id = parent.statechain_id.clone().ok_or_else(|| anyhow!("parent has no statechain_id"))?;
    let bkps = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, &id).await?;
    let b = bkps.first().ok_or_else(|| anyhow!("parent has no deposit backup"))?;
    Ok(Stale {
        o_txid: parent.utxo_txid.clone().ok_or_else(|| anyhow!("no utxo_txid"))?,
        o_vout: parent.utxo_vout.ok_or_else(|| anyhow!("no utxo_vout"))?,
        backup_hex: b.tx.clone(),
        locktime: mercurylib::utils::get_blockheight(b)?,
    })
}

async fn fund_and_confirm(cc: &ClientConfig, alice: &SparkWallet, amount: u32) -> Result<()> {
    let t = prepaid_token(cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats < amount as u64 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Ok(())
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk14_alice"), None).await?;
    let (bob, _) = SparkWallet::initialize(SdkConfig::regtest("sdk14_bob"), None).await?;
    let bob_addr = bob.get_spark_address().await?;

    // alice pays bob 15k off-chain; bob receives a branch sub-coin.
    fund_and_confirm(&cc, &alice, 40_000).await?;
    let stale = capture_stale(&cc, "sdk14_alice", 40_000).await?;
    for _ in 0..2 { let t = prepaid_token(&cc).await?; alice.add_prepaid_token(&t).await; }
    let r = alice.transfer(&bob_addr, 15_000).await?;
    assert!(r.used_split);
    assert_eq!(bob.claim().await?.claimed_transfers, 1);
    let piece = r.coins[0].statechain_id.clone();

    // --- What the watcher KNOWS up front: the deadline and the cost --------------------------------
    let deadline = stale.locktime; // attacker's stale backup is non-final until here
    let est = bob.estimate_exit_cost(&piece).await?;
    let now = height(&cc)?;
    println!("SDK14 - coin received. DEADLINE (stale backup locktime) = block {deadline}; now = {now}; window = {} blocks (~{} days @10min)",
        deadline.saturating_sub(now), (deadline.saturating_sub(now) as f64 * 10.0 / 1440.0) as u32);
    println!("SDK14 - EXIT COST: {} branch tx + backup = {} vB. Fee to broadcast the exit:", est.branch_txs, est.total_vbytes);
    for rate in [1.0_f64, 2.0, 5.0, 10.0, 50.0] {
        println!("SDK14 -    @ {:>4.0} sat/vB -> {:>6} sats", rate, est.fee_sats_at(rate));
    }
    println!("SDK14 - Because the attacker's stale state is non-final until the deadline, an exit BEFORE it is UNCONTESTED — normal fee, no race premium.");

    // --- The watchtower loop: watch the height; exit a safety margin before the deadline -----------
    // (We fast-forward the chain toward the deadline to simulate the passage of time.)
    let mut exited_at = 0u32;
    loop {
        let h = height(&cc)?;
        // Defensive: if the funding is somehow already spent, stop.
        if is_outpoint_spent(&cc, &stale.o_txid, stale.o_vout) {
            break;
        }
        if h + SAFETY_MARGIN >= deadline {
            // Deadline approaching -> ACT NOW. The branch is locktime-free; the attacker is still
            // locked out (h < deadline), so this exit is uncontested.
            assert!(h < deadline, "watcher must act strictly before the deadline");
            let _ = bob.unilateral_exit(Some(vec![piece.clone()]), None).await?;
            bitcoin_core::generatetoaddress(1, &core)?;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            exited_at = h;
            break;
        }
        // advance time toward the deadline
        let step = (deadline.saturating_sub(h).saturating_sub(SAFETY_MARGIN)).min(500).max(1);
        bitcoin_core::generatetoaddress(step, &core)?;
    }

    assert!(exited_at > 0 && exited_at < deadline, "watcher exited within the window (before the deadline)");
    assert!(is_outpoint_spent(&cc, &stale.o_txid, stale.o_vout), "the branch spent the parent funding");
    println!("SDK14 - [watchtower] exited at block {exited_at} (deadline {deadline}, margin {SAFETY_MARGIN}) — branch confirmed, funding secured \u{2713}");

    // --- The attacker is now too late: the stale backup is rejected (its input is spent) -----------
    // Mine past the deadline so the stale backup would be final, and try it anyway.
    let mut rem = deadline.saturating_sub(height(&cc)?) + 2;
    while rem > 0 { let c = rem.min(500); bitcoin_core::generatetoaddress(c, &core)?; rem -= c; }
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let attack = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&stale.backup_hex)?);
    assert!(attack.is_err(), "stale backup must be rejected: the branch already spent the funding; got {attack:?}");
    println!("SDK14 - [attack] past the deadline the stale backup is REJECTED (input already spent by the branch) \u{2713}");

    println!("SDK14 - SUCCESS: an auto-racing watchtower needs only (1) the DEADLINE (the stale-state locktime, ~a week out) and (2) a NORMAL exit fee (~{} sats @2sat/vB for {} vB) — it exits within the window UNCONTESTED, so there is no bidding war. The 'race' only appears if you miss the deadline; act in time and you win deterministically at ordinary fees.",
        est.fee_sats_at(2.0), est.total_vbytes);
    Ok(())
}
