//! E2E (adversarial): **broadcasting old/stale state** — an attacker (a previous owner) tries to
//! claw back a coin they already split/transferred by broadcasting its stale on-chain backup, and we
//! prove the decrementing-locktime ladder defeats it AND that a watcher detects it.
//!
//! The stale state here is the PARENT deposit backup. After alice splits her parent into a branch
//! (locktime-free) that funds bob's sub-coin, alice still holds the parent's deposit backup — a
//! valid tx that also spends the parent's funding outpoint O, but locktimed HIGH.
//!
//! Scenario 1 (DEFENDED): bob's watcher acts.
//!   A. alice broadcasts the stale parent backup early  -> REJECTED (non-final: locktime not reached).
//!   B. bob broadcasts the locktime-free branch          -> confirms, spends O.
//!   C. alice broadcasts the stale backup again          -> REJECTED (its input O is already spent).
//!   D. watcher(O, bob's branch txid) -> SafeExited.
//!
//! Scenario 2 (UNDEFENDED, proves the risk is real + detection works): bob stays offline.
//!   alice waits out the parent backup's locktime, broadcasts it -> CONFIRMS, clawing back the coin.
//!   watcher(O, bob's branch txid) -> Fraud (O spent by a txid that is NOT bob's branch).
//!
//! Run: SDK_E2E=13 ML_NETWORK=regtest cargo run

use std::str::FromStr;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_spark_sdk::{SdkConfig, SparkWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// Claim exactly one incoming transfer, polling for message propagation.
async fn claim_one(w: &SparkWallet) -> Result<()> {
    for _ in 0..30 {
        if w.claim().await?.claimed_transfers >= 1 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(anyhow!("receiver claimed no transfer"))
}

/// Stale on-chain state an attacker could broadcast: the parent coin's funding outpoint O, its
/// pre-signed deposit backup (hex), and that backup's locktime.
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
        _ => return true, // can't fetch -> treat as gone
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk).unwrap_or_default();
    !listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout)
}

/// The txid that actually spent outpoint `o_txid:o_vout` on-chain, if any (by scanning the funding
/// script's history for a tx that consumes O).
fn spender_of(cc: &ClientConfig, o_txid: &str, o_vout: u32) -> Option<String> {
    use electrum_client::bitcoin::Txid;
    let raw = cc.electrum_client.transaction_get_raw(&Txid::from_str(o_txid).ok()?).ok()?;
    let otx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).ok()?;
    let spk = &otx.output.get(o_vout as usize)?.script_pubkey;
    for h in cc.electrum_client.script_get_history(spk).ok()? {
        let htxid = h.tx_hash.to_string();
        if htxid == o_txid {
            continue; // the funding tx itself
        }
        let hraw = match cc.electrum_client.transaction_get_raw(&h.tx_hash) {
            std::result::Result::Ok(r) => r,
            _ => continue,
        };
        let htx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hraw).ok()?;
        if htx.input.iter().any(|i| {
            i.previous_output.txid.to_string() == o_txid && i.previous_output.vout == o_vout
        }) {
            return Some(htxid);
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum Watch {
    SafeOffchain, // funding intact, coin still off-chain
    SafeExited,   // funding spent by MY own exit (branch/backup)
    Fraud,        // funding spent by someone else — stale-state claw-back detected
}

/// A minimal watcher/watchtower classifier: given the coin's funding outpoint O and the owner's OWN
/// legitimate exit txid, decide whether the coin is safe, safely exited, or being stolen. It reads
/// the ACTUAL on-chain spender of O — it does not trust anyone's word.
fn watch(cc: &ClientConfig, o_txid: &str, o_vout: u32, my_exit_txid: &str) -> Watch {
    match spender_of(cc, o_txid, o_vout) {
        None => Watch::SafeOffchain,
        Some(t) if t == my_exit_txid => Watch::SafeExited,
        Some(_) => Watch::Fraud,
    }
}

/// Capture alice's newest CONFIRMED `amount`-sat deposit as stale material (outpoint + backup).
async fn capture_stale(cc: &ClientConfig, wallet_name: &str, amount: u32) -> Result<(String, Stale)> {
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
    Ok((
        id,
        Stale {
            o_txid: parent.utxo_txid.clone().ok_or_else(|| anyhow!("no utxo_txid"))?,
            o_vout: parent.utxo_vout.ok_or_else(|| anyhow!("no utxo_vout"))?,
            backup_hex: b.tx.clone(),
            locktime: mercurylib::utils::get_blockheight(b)?,
        },
    ))
}

/// The branch-root txid bob would broadcast to exit his sub-coin (the tx that spends O legitimately).
async fn branch_root_txid(cc: &ClientConfig, wallet_name: &str, piece_id: &str) -> Result<String> {
    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, &format!("branch-{piece_id}")).await?;
    let root = branch.first().ok_or_else(|| anyhow!("no branch stored for {piece_id}"))?;
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&root.tx)?)?;
    Ok(tx.txid().to_string())
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

    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk13_alice"), None).await?;
    let (bob, _) = SparkWallet::initialize(SdkConfig::regtest("sdk13_bob"), None).await?;
    let bob_addr = bob.get_spark_address().await?;

    // =====================================================================================
    // SCENARIO 1 — DEFENDED: the watcher exits the branch before the stale backup can win.
    // =====================================================================================
    fund_and_confirm(&cc, &alice, 40_000).await?;
    let (_id_a, stale_a) = capture_stale(&cc, "sdk13_alice", 40_000).await?;
    println!("SDK13 - alice parent A funded 40k; stale backup locktime={} (funding {}:{})",
        stale_a.locktime, stale_a.o_txid, stale_a.o_vout);

    // A. The attacker tries to broadcast the stale backup RIGHT NOW -> rejected (non-final).
    let early = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&stale_a.backup_hex)?);
    assert!(early.is_err(),
        "stale parent backup MUST be rejected before its locktime (non-final); got {early:?}");
    println!("SDK13 - [A] early broadcast of the stale backup REJECTED (non-final, locktime not reached) \u{2713}");

    // Split: alice pays bob 15k off-chain; bob claims his branch sub-coin.
    for _ in 0..2 { let t = prepaid_token(&cc).await?; alice.add_prepaid_token(&t).await; }
    let r = alice.transfer(&bob_addr, 15_000).await?;
    assert!(r.used_split);
    assert_eq!(bob.claim().await?.claimed_transfers, 1);
    let bob_piece = r.coins[0].statechain_id.clone();
    let branch_txid = branch_root_txid(&cc, "sdk13_bob", &bob_piece).await?;

    // The watcher, before any exit: funding intact -> coin is still safely off-chain.
    assert_eq!(watch(&cc, &stale_a.o_txid, stale_a.o_vout, &branch_txid), Watch::SafeOffchain);
    println!("SDK13 - [watcher] pre-exit: funding intact -> SafeOffchain");

    // B. bob's watcher acts: broadcast the locktime-free branch. It spends O immediately.
    let _ = bob.unilateral_exit(Some(vec![bob_piece.clone()]), None).await?;
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(is_outpoint_spent(&cc, &stale_a.o_txid, stale_a.o_vout), "branch must spend the parent funding O");
    println!("SDK13 - [B] bob broadcast the branch; parent funding O is now spent by the branch \u{2713}");

    // C. attacker tries the stale backup again -> its input O is gone, rejected forever.
    let late = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&stale_a.backup_hex)?);
    assert!(late.is_err(), "stale backup MUST be rejected after O is spent by the branch; got {late:?}");
    println!("SDK13 - [C] stale backup REJECTED again (its input O already spent by the branch) \u{2713}");

    // D. watcher after the exit: O spent by bob's OWN branch -> SafeExited.
    assert_eq!(watch(&cc, &stale_a.o_txid, stale_a.o_vout, &branch_txid), Watch::SafeExited);
    println!("SDK13 - [D][watcher] post-exit: O spent by bob's branch -> SafeExited \u{2713}");
    println!("SDK13 - SCENARIO 1 DEFENDED: the locktime-free branch always beats the higher-locktime stale backup; the stale state can never confirm.");

    // =====================================================================================
    // SCENARIO 2 — UNDEFENDED: bob stays offline past the deadline; the attack succeeds and the
    // watcher DETECTS it. Proves the risk is real and why acting within the window matters.
    // =====================================================================================
    fund_and_confirm(&cc, &alice, 40_000).await?;
    let (_id_b, stale_b) = capture_stale(&cc, "sdk13_alice", 40_000).await?;
    // Split 30k so the fresh 40k parent B is selected (alice's ~24k scenario-1 change can't cover it),
    // guaranteeing the branch roots at B's on-chain funding rather than an already-broadcast tx.
    for _ in 0..2 { let t = prepaid_token(&cc).await?; alice.add_prepaid_token(&t).await; }
    let r2 = alice.transfer(&bob_addr, 30_000).await?;
    assert!(r2.used_split);
    claim_one(&bob).await?;
    let bob_piece2 = r2.coins[0].statechain_id.clone();
    let branch_txid2 = branch_root_txid(&cc, "sdk13_bob", &bob_piece2).await?;
    println!("SDK13 - alice parent B funded 40k; bob received his sub-coin but STAYS OFFLINE (no exit). stale locktime={}", stale_b.locktime);

    // bob does NOT exit. Mine past the stale backup's locktime so it becomes final (spendable).
    let tip = cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
    let mut remaining = stale_b.locktime.saturating_sub(tip) + 2;
    println!("SDK13 - mining {remaining} blocks to pass the stale backup locktime {}...", stale_b.locktime);
    while remaining > 0 {
        let chunk = remaining.min(500);
        bitcoin_core::generatetoaddress(chunk, &core)?;
        remaining -= chunk;
    }
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // The attacker broadcasts the (now-final) stale backup. O was never spent -> it CONFIRMS.
    let attack = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&stale_b.backup_hex)?);
    assert!(attack.is_ok(),
        "past its locktime and with the funding unspent, the stale backup DOES confirm — the attack succeeds when the receiver is absent; got {attack:?}");
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(is_outpoint_spent(&cc, &stale_b.o_txid, stale_b.o_vout), "attacker's stale backup spent O");
    println!("SDK13 - [attack] bob was offline past the deadline; alice's stale backup CONFIRMED and clawed back the coin");

    // The watcher detects it: O is spent, but NOT by bob's branch -> Fraud.
    let culprit = spender_of(&cc, &stale_b.o_txid, stale_b.o_vout);
    assert_ne!(culprit.as_deref(), Some(branch_txid2.as_str()), "O was spent by a stranger, not bob's branch");
    assert_eq!(watch(&cc, &stale_b.o_txid, stale_b.o_vout, &branch_txid2), Watch::Fraud);
    println!("SDK13 - [watcher] DETECTED Fraud: parent funding O spent by a txid that is NOT bob's branch \u{2713}");

    println!("SDK13 - SUCCESS: stale-state broadcast is (1) rejected before its locktime, (2) permanently defeated once the honest party exits the locktime-free branch, and (3) if the receiver is absent past the deadline the attack succeeds but a watcher DETECTS it (funding spent by an unexpected txid) — so a client/watchtower must exit within the ladder window.");
    Ok(())
}
