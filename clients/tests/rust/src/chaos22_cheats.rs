//! Cheat injections for the chaos test: a fuzz user occasionally tries to STEAL. Every cheat must
//! be refused by the protocol (per spec), and the oracle re-checks on-chain that the cheater never
//! ended up with value they gave away. Emitted as `chaos_fault` trace events the oracle audits.
//!
//! THE SHAPE THE CHEATS TARGET. A coin's only exit material is its TES-R ladder: there is NO
//! absolute-locktime flat backup on any coin — none is co-signed at deposit (`create_tx1` is gone)
//! and none at any hop (transfers convey `backup_transactions: []`, and the oracle asserts ZERO flat
//! backup rows on every live coin). So "broadcast old state" no longer means "broadcast a stale flat
//! backup"; it means broadcasting a captured LADDER STATE `S` that a later transition superseded —
//! the receiver-paying `S'` of a whole-coin send, or the `SP` of an in-ladder split, each over the
//! SAME outpoint at a strictly LOWER CSV. Broadcast on its own the stale state is rejected outright:
//! its input is an un-broadcast tier output that does not exist on chain
//! (`bad-txns-inputs-missingorspent`), and even after a trigger walk it loses the CSV race to the
//! state that replaced it. The funding outpoint `F` is never spent by it — which is what the oracle's
//! on-chain backstop checks.

use std::sync::Arc;

use rand::rngs::StdRng;
use rand::Rng;
use serde_json::json;
use tokio::sync::Mutex;

use crate::chaos22_concurrent_users::{Registry, Trace, UserHandle};

/// Pick a random cheat and run it. Both are "broadcast a superseded ladder state" claw-backs that
/// must be refused — one after MOVING the coin (a whole-coin send replaces `S` with the receiver's
/// `S'`), one after MUTATING it in-ladder (a split replaces `S` with `SP`). Each emits a
/// `chaos_fault` the oracle audits on-chain (the funding outpoint must never be spent by the stale
/// tx).
pub async fn run_random_cheat(
    me: &Arc<UserHandle>,
    registry: &Arc<Registry>,
    trace: &Arc<Trace>,
    bitcoin: &Arc<Mutex<()>>,
    rng: &mut StdRng,
) {
    if rng.gen_bool(0.5) {
        steal_after_send(me, registry, trace, bitcoin, rng).await;
    } else {
        steal_after_split(me, trace, bitcoin, rng).await;
    }
}

/// THE cheat: capture a coin's CURRENT ladder state, legitimately SEND the coin to someone else,
/// then broadcast the now-STALE state to try to claw the coin back to myself.
///
/// On a laddered coin there is no flat backup to capture — a laddered coin carries NONE, at deposit
/// or at any hop — so the clawback vector is the sender's retained state `S`: the transfer replaces
/// it with the receiver-paying `S'` over the SAME outpoint (`X_m`'s payload output) at a strictly
/// LOWER CSV and discloses the old one as SUPERSEDED (the receiver's census counts it). Per
/// INV-18/19 the stale state must lose: broadcast on its own it is rejected outright (its input is
/// an un-broadcast tier output — `bad-txns-inputs-missingorspent`), and after a trigger walk it
/// loses the maturity race to `S'`. The oracle later verifies on-chain that the funding outpoint
/// was never spent by this stale tx (Fraud).
async fn steal_after_send(
    me: &Arc<UserHandle>,
    registry: &Arc<Registry>,
    trace: &Arc<Trace>,
    bitcoin: &Arc<Mutex<()>>,
    rng: &mut StdRng,
) {
    use electrum_client::ElectrumApi;

    let cc = me.wallet.client_config();
    let name = me.wallet.wallet_name();

    // Pick a confirmed LADDERED ROOT I hold (a `tesr-` row: the whole-coin send conveys its ladder).
    // A received in-ladder child (`ctesr-`) or a spine tip has no `tesr-` row and is handed over by
    // a different builder, so it is skipped here — the split cheat covers the in-ladder shape.
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

    // Capture the stale material: the CURRENT state `S` of the coin's ladder. (There is no flat
    // backup row to capture; if one ever appears the oracle flags it as a breach in its own right.)
    let bundle = match mercuryrustlib::tesr::load(cc, name, &id).await {
        Ok(Some(b)) => b,
        _ => return, // not a laddered root — nothing to stage here
    };
    let stale = bundle.current().state.signed_tx.clone();
    let stale_txid = bundle.current().state.txid.clone();

    // Legitimately send the coin away to a random OTHER user. The send co-signs the receiver's `S'`
    // (one rung lower) and supersedes the state captured above; it conveys NO flat backup.
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

    // Now broadcast the STALE state to try to claw the coin back. Must be rejected.
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
            "cheat": "superseded_state_after_send",
            "coin": id,
            "o_txid": o_txid,
            "o_vout": o_vout,
            "stale_txid": stale_txid,
            "victim": victim,
            "broadcast_err": broadcast.err().map(|e| e.to_string()),
        }),
    );
}

/// THE second cheat, an in-ladder variant: capture a coin's CURRENT ladder state, then SPLIT the
/// coin in-ladder (its value moves into fresh children under `SP` over the SAME outpoint at a
/// strictly LOWER CSV — the captured state becomes SUPERSEDED), then broadcast the now-STALE
/// pre-split state to try to exit the pre-split coin. Per INV-18 this must be REJECTED: broadcast
/// alone its input does not exist on chain, and after a trigger walk `SP` (CSV 0) wins the race
/// unconditionally. Exercises a stale unilateral exit racing an ACTIVE in-ladder mutation. Same
/// emit/audit shape as steal_after_send, so the oracle's on-chain fraud backstop covers it unchanged.
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
    // broadcasting the pre-split state MUST be rejected.
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

    // Capture the coin's CURRENT ladder state BEFORE the split — the clawback vector on a laddered
    // coin: the in-ladder split replaces this state with `SP` over the SAME outpoint (X_m.out[0]) at a
    // strictly LOWER CSV, so the captured one becomes SUPERSEDED and must lose the maturity race.
    // (A laddered coin has no absolute-locktime backup at all — the superseded state is the only
    // stale material there is.)
    let bundle = match mercuryrustlib::tesr::load(cc, name, &id).await {
        Ok(Some(b)) => b,
        _ => return, // not a laddered root (e.g. a received child) — nothing to stage here
    };
    let stale = bundle.current().state.signed_tx.clone();
    let stale_txid = bundle.current().state.txid.clone();

    // Legitimately SPLIT the coin in-ladder — value moves into fresh children and the captured state
    // is superseded. Pay ourselves so the cheat needs no peer.
    let piece = rng.gen_range(5_000..(parent_sats / 2).max(5_001));
    let self_addr = match me.wallet.get_utexo_address().await {
        Ok(a) => a,
        Err(_) => return,
    };
    if me
        .wallet
        .in_ladder_pay(&id, &self_addr, piece, mercury_utexo_sdk::transfer::InLadderLatch::None)
        .await
        .is_err()
    {
        // Couldn't split (contention / floors); nothing staged, skip quietly.
        return;
    }

    // Now broadcast the STALE pre-split state to try to exit the pre-split coin. Must be rejected.
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
            "cheat": "superseded_state_after_split",
            "coin": id,
            "o_txid": o_txid,
            "o_vout": o_vout,
            "stale_txid": stale_txid,
            "piece": piece,
            "broadcast_err": broadcast.err().map(|e| e.to_string()),
        }),
    );
}
