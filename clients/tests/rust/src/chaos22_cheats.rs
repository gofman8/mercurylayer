//! Cheat injections for the chaos test: a fuzz user occasionally tries to STEAL. Every cheat must
//! be refused by the protocol (per spec), and the oracle re-checks on-chain that the cheater never
//! ended up with value they gave away. Emitted as `chaos_fault` trace events the oracle audits.

use std::sync::Arc;

use rand::rngs::StdRng;
use rand::Rng;
use serde_json::json;
use tokio::sync::Mutex;

use crate::chaos22_concurrent_users::{Registry, Trace, UserHandle};

/// Pick a random cheat and run it. Both are "broadcast old state" claw-backs that must be refused —
/// one after MOVING the coin (send), one after MUTATING it inside the DAG (split into fresh
/// sub-coins). Each emits a `chaos_fault` the oracle audits on-chain (the funding outpoint must
/// never be spent by the stale tx).
pub async fn run_random_cheat(
    me: &Arc<UserHandle>,
    registry: &Arc<Registry>,
    trace: &Arc<Trace>,
    bitcoin: &Arc<Mutex<()>>,
    rng: &mut StdRng,
) {
    if rng.gen_bool(0.5) {
        steal_after_send(me, registry, trace, rng).await;
    } else {
        steal_after_split(me, trace, bitcoin, rng).await;
    }
}

/// THE cheat: capture a coin's backup, legitimately SEND the coin to someone else, then broadcast
/// the now-STALE backup to try to claw the coin back to myself. Per INV-5/18/19 this must be
/// REJECTED (the stale backup is non-final — its locktime is above the tip — and the recipient's
/// fresh state has a strictly lower locktime, so it always wins the exit race). The oracle later
/// verifies on-chain that the funding outpoint was never spent by this stale backup (Fraud).
async fn steal_after_send(
    me: &Arc<UserHandle>,
    registry: &Arc<Registry>,
    trace: &Arc<Trace>,
    rng: &mut StdRng,
) {
    use electrum_client::ElectrumApi;

    let cc = me.wallet.client_config();
    let name = me.wallet.wallet_name();

    // Pick any confirmed coin I hold. If its funding is on-chain the stale backup is rejected as
    // "non-final"; if it is an off-chain sub-coin the same broadcast is rejected as "missing input".
    // Either way the claw-back MUST fail — and the oracle's on-chain backstop confirms the funding
    // outpoint was never spent by the stale tx. (Prefer on-chain coins so the strongest rejection
    // path is exercised, but fall back to sub-coins so the cheat still fires in a deep-DAG run.)
    let coins = match me.wallet.list_coins().await {
        Ok(c) => c,
        Err(_) => return,
    };
    let elig = |c: &&mercury_utexo_sdk::types::CoinInfo| {
        c.status == "CONFIRMED" && c.statechain_id.is_some() && c.utxo_txid.is_some()
            && c.amount_sats >= 15_000
    };
    let Some(coin) = coins.iter().find(|c| !c.off_chain && elig(c)).or_else(|| coins.iter().find(elig))
    else {
        return;
    };
    let id = coin.statechain_id.clone().unwrap();
    let o_txid = coin.utxo_txid.clone().unwrap();
    let o_vout = coin.utxo_vout.unwrap_or(0);

    // Capture the stale backup (the latest one for this coin).
    let backups = match mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, name, &id).await {
        Ok(b) if !b.is_empty() => b,
        _ => return,
    };
    let stale = backups.iter().max_by_key(|b| b.tx_n).unwrap().tx.clone();
    let stale_txid = match hex::decode(&stale)
        .ok()
        .and_then(|raw| electrum_client::bitcoin::consensus::deserialize::<electrum_client::bitcoin::Transaction>(&raw).ok())
    {
        Some(tx) => tx.txid().to_string(),
        None => return,
    };

    // Legitimately send the coin away to a random OTHER user.
    let victim = loop {
        let j = rng.gen_range(0..registry.users.len());
        if j != me.idx {
            break j;
        }
    };
    let to_addr = registry.users[victim].address.clone();
    let sent = mercuryrustlib::transfer_sender::execute(cc, &to_addr, name, &id, None, false, None).await;
    if sent.is_err() {
        // Couldn't even send (contention); nothing staged, skip quietly.
        return;
    }

    // Now broadcast the STALE backup to try to claw the coin back. Must be rejected.
    let raw = match hex::decode(&stale) {
        Ok(r) => r,
        Err(_) => return,
    };
    let broadcast = cc.electrum_client.transaction_broadcast_raw(&raw);
    let refused = broadcast.is_err();
    trace.emit(
        me.idx,
        "chaos_fault",
        "result",
        if refused { "refused" } else { "succeeded" },
        json!({
            "cheat": "broadcast_old_state",
            "coin": id,
            "o_txid": o_txid,
            "o_vout": o_vout,
            "stale_txid": stale_txid,
            "victim": victim,
            "broadcast_err": broadcast.err().map(|e| e.to_string()),
        }),
    );
}

/// THE second cheat, a DAG variant: capture a coin's backup, then SPLIT the coin (its value moves
/// into fresh off-chain sub-coins rooted at the same funding outpoint — a new, lower-locktime
/// branch), then broadcast the now-STALE parent backup to try to exit the pre-split state. Per
/// INV-5/18 this must be REJECTED: the stale backup is non-final (locktime above tip) and the
/// split's branch is unconditionally broadcastable (locktime 0), so it always wins. Exercises a
/// stale unilateral exit racing an ACTIVE DAG mutation. Same emit/audit shape as steal_after_send,
/// so the oracle's on-chain fraud backstop covers it unchanged.
async fn steal_after_split(
    me: &Arc<UserHandle>,
    trace: &Arc<Trace>,
    bitcoin: &Arc<Mutex<()>>,
    rng: &mut StdRng,
) {
    use electrum_client::ElectrumApi;

    let cc = me.wallet.client_config();
    let name = me.wallet.wallet_name();

    // A confirmed coin I hold that is big enough to split. Prefer on-chain funding (strongest
    // rejection path) but fall back to a sub-coin so the cheat fires in a deep-DAG run; either way
    // broadcasting the pre-split backup MUST be rejected.
    let coins = match me.wallet.list_coins().await {
        Ok(c) => c,
        Err(_) => return,
    };
    let elig = |c: &&mercury_utexo_sdk::types::CoinInfo| {
        c.status == "CONFIRMED" && c.statechain_id.is_some() && c.utxo_txid.is_some()
            && c.amount_sats >= 30_000
    };
    let Some(coin) = coins.iter().find(|c| !c.off_chain && elig(c)).or_else(|| coins.iter().find(elig))
    else {
        return;
    };
    let id = coin.statechain_id.clone().unwrap();
    let o_txid = coin.utxo_txid.clone().unwrap();
    let o_vout = coin.utxo_vout.unwrap_or(0);
    let parent_sats = coin.amount_sats;

    // Capture the stale (pre-split) backup for this coin.
    let backups = match mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, name, &id).await {
        Ok(b) if !b.is_empty() => b,
        _ => return,
    };
    let stale = backups.iter().max_by_key(|b| b.tx_n).unwrap().tx.clone();
    let stale_txid = match hex::decode(&stale)
        .ok()
        .and_then(|raw| electrum_client::bitcoin::consensus::deserialize::<electrum_client::bitcoin::Transaction>(&raw).ok())
    {
        Some(tx) => tx.txid().to_string(),
        None => return,
    };

    // Legitimately SPLIT the coin — value moves into fresh sub-coins; the captured backup is now stale.
    let piece = rng.gen_range(5_000..(parent_sats / 2).max(5_001));
    if me.wallet.split_coin(&id, piece).await.is_err() {
        // Couldn't split (contention); nothing staged, skip quietly.
        return;
    }

    // Now broadcast the STALE parent backup to try to exit the pre-split state. Must be rejected.
    let raw = match hex::decode(&stale) {
        Ok(r) => r,
        Err(_) => return,
    };
    let broadcast = {
        let _g = bitcoin.lock().await;
        cc.electrum_client.transaction_broadcast_raw(&raw)
    };
    let refused = broadcast.is_err();
    trace.emit(
        me.idx,
        "chaos_fault",
        "result",
        if refused { "refused" } else { "succeeded" },
        json!({
            "cheat": "stale_branch_after_split",
            "coin": id,
            "o_txid": o_txid,
            "o_vout": o_vout,
            "stale_txid": stale_txid,
            "piece": piece,
            "broadcast_err": broadcast.err().map(|e| e.to_string()),
        }),
    );
}
