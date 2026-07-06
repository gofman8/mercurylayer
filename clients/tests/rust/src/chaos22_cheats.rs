//! Cheat injections for the chaos test: a fuzz user occasionally tries to STEAL. Every cheat must
//! be refused by the protocol (per spec), and the oracle re-checks on-chain that the cheater never
//! ended up with value they gave away. Emitted as `chaos_fault` trace events the oracle audits.

use std::sync::Arc;

use rand::rngs::StdRng;
use rand::Rng;
use serde_json::json;
use tokio::sync::Mutex;

use crate::chaos22_concurrent_users::{Registry, Trace, UserHandle};

/// Pick a random cheat and run it. Currently: "broadcast old state" — the canonical claw-back.
pub async fn run_random_cheat(
    me: &Arc<UserHandle>,
    registry: &Arc<Registry>,
    trace: &Arc<Trace>,
    _bitcoin: &Arc<Mutex<()>>,
    rng: &mut StdRng,
) {
    // Only one cheat implemented for now; extend with double-spend / nonce-reuse later.
    steal_after_send(me, registry, trace, rng).await;
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

    // Pick one of my confirmed coins whose funding is ON-CHAIN (so the backup's input exists and the
    // rejection is a meaningful "non-final", not "missing input").
    let coins = match me.wallet.list_coins().await {
        Ok(c) => c,
        Err(_) => return,
    };
    let Some(coin) = coins.iter().find(|c| {
        c.status == "CONFIRMED" && !c.off_chain && c.statechain_id.is_some()
            && c.utxo_txid.is_some() && c.amount_sats >= 20_000
    }) else {
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
