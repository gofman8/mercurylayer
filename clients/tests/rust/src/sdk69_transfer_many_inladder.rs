//! E2E (SDK_E2E=69) — **`transfer_many` on a LADDERED parent: the multi-child in-ladder split [B1]**.
//!
//! `transfer_many` used to build a PLAIN split of its parent's funding outpoint `F`, whatever the
//! parent's shape. That is exactly the case `split_coin` refuses ([B1]): the trigger `T` is
//! un-timelocked, fully co-signed at `establish`, and spends the SAME `F`, so whoever retains `T`
//! (the sender itself, or a prior owner of a Model-A-conveyed coin) can broadcast it, consume `F`,
//! and void the split tx — killing EVERY recipient's piece at once while their own ladder pays them
//! the full parent value. Since `claim()` ladders every fresh confirmed root coin unconditionally,
//! that was the DEFAULT path for an ordinary wallet, not an edge case.
//!
//! `transfer_many` now dispatches on the parent's shape like `transfer` does, and a laddered parent
//! takes a MULTI-CHILD IN-LADDER SPLIT: one split state `SP` over `X_m.out[0]` carving N recipient
//! children plus the sender's change. `SP` DESCENDS from `T` instead of racing it for `F`, so a
//! retained trigger has nothing to race — it can only start the clock on the recipients' own chains.
//!
//! What this test proves, in order:
//!   1. the parent really is laddered, and a PLAIN split of it is still refused ([B1] guard intact);
//!   2. `transfer_many` routes in-ladder anyway: ONE `SP` with 2 pieces + change + the P2A anchor,
//!      bob at `SP.out[0]`, carol at `SP.out[1]`, both censused and adopted with the key handover;
//!   3. the whole batch is OFF-CHAIN — `F` is untouched after paying two people;
//!   4. **the B1 attack is run for real**: alice broadcasts her retained trigger `T`, consuming `F`
//!      — and bob and carol STILL exit unilaterally for their exact amounts. On the old plain-split
//!      lane this same broadcast would have destroyed both pieces permanently.
//!
//! Run: SDK_E2E=69 ML_NETWORK=regtest cargo +stable run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const DEPOSIT: u64 = 100_000;
const PAY_BOB: u64 = 20_000;
const PAY_CAROL: u64 = 30_000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn v2_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

fn parse_tx(signed_hex: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(signed_hex)?)?)
}

fn is_outpoint_spent(
    cc: &mercuryrustlib::client_config::ClientConfig,
    txid: &str,
    vout: u32,
) -> bool {
    use electrum_client::bitcoin::Txid;
    let raw = match cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()) {
        std::result::Result::Ok(r) => r,
        _ => return true,
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = tx.output[vout as usize].script_pubkey.clone();
    match cc.electrum_client.script_list_unspent(&spk) {
        std::result::Result::Ok(utxos) => {
            !utxos.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos == vout as usize)
        }
        _ => true,
    }
}

/// Poll `claim()` until the wallet books a confirmed coin of exactly `want` sats; return its sid.
async fn claim_until(
    cc: &mercuryrustlib::client_config::ClientConfig,
    w: &UtexoWallet,
    name: &str,
    want: u64,
) -> Result<String> {
    for _ in 0..40 {
        w.claim().await?;
        if w.get_balance().await?.available_sats == want {
            return mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name)
                .await?
                .coins
                .iter()
                .find(|c| {
                    c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                        && c.amount == Some(want as u32)
                })
                .and_then(|c| c.statechain_id.clone())
                .ok_or_else(|| anyhow!("{name} booked no confirmed {want}-sat coin"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} did not claim a {want}-sat coin"))
}

/// Drive a wallet's keyless child exit to completion, mining out each tier's relative timelock.
async fn exit_child(w: &UtexoWallet, sid: &str, core: &str, label: &str) -> Result<()> {
    let mut passes = 0;
    loop {
        let st = w.unilateral_exit(Some(vec![sid.to_string()]), None).await?;
        let s = st.into_iter().next().ok_or(anyhow!("no exit status for {label}"))?;
        if s.complete {
            println!("SDK69 - {label}'s exit COMPLETE after {passes} pass(es)");
            return Ok(());
        }
        // Each pass waits out ONE tier's CSV (+1 block to confirm the tx just broadcast). The chain is
        // F -> T -> X_m -> SP -> ext_child -> state_child.
        bitcoin_core::generatetoaddress(s.wait_blocks.max(1) + 1, core)?;
        passes += 1;
        if passes > 40 {
            return Err(anyhow!("{label}'s exit did not complete"));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = v2_wallet("sdk69_alice").await?;
    let bob = v2_wallet("sdk69_bob").await?;
    let carol = v2_wallet("sdk69_carol").await?;
    let bob_address = bob.get_utexo_address().await?;
    let carol_address = carol.get_utexo_address().await?;

    // ---- Alice deposits: the only on-chain funding in the whole flow. --------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != DEPOSIT {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("alice's deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk69_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;

    // ---- [1] The parent IS laddered, and a plain split of it is still refused ([B1] guard). -----
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk69_alice", &alice_sid)
        .await?
        .ok_or(anyhow!("claim() must ladder a fresh confirmed root coin unconditionally"))?;
    let (f_txid, f_vout) = (bundle.f_txid.clone(), bundle.f_vout);
    // The retained, un-timelocked trigger — the B1 weapon. Kept aside now, fired in step [4].
    let retained_trigger = bundle.trigger.signed_tx.clone();
    let trigger_txid = bundle.trigger.txid.clone();
    let plain_split = alice.split_coin(&alice_sid, PAY_BOB).await;
    let err = plain_split.expect_err("a plain split of a laddered coin must be refused").to_string();
    assert!(
        err.contains("B1") && err.contains("cannot be split"),
        "the refusal must name the hazard, got: {err}"
    );
    assert_eq!(
        alice.get_balance().await?.available_sats, DEPOSIT,
        "the refusal is side-effect free — the parent is untouched and still spendable"
    );
    println!("SDK69 - [1] alice's {DEPOSIT}-sat coin is LADDERED (F = {f_txid}:{f_vout}); a plain split of it is refused: {err}");

    // ---- [2] transfer_many pays TWO people out of that laddered parent, in-ladder. --------------
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let results = alice
        .transfer_many(&[(bob_address.clone(), PAY_BOB), (carol_address.clone(), PAY_CAROL)])
        .await?;
    assert_eq!(results.len(), 2, "one result per recipient");
    assert!(results.iter().all(|r| r.used_split), "both pieces came from one split");
    assert_eq!(results[0].total_sats, PAY_BOB);
    assert_eq!(results[1].total_sats, PAY_CAROL);

    let bob_sid = claim_until(&cc, &bob, "sdk69_bob", PAY_BOB).await?;
    let carol_sid = claim_until(&cc, &carol, "sdk69_carol", PAY_CAROL).await?;
    assert_eq!(bob_sid, results[0].coins[0].statechain_id, "bob adopted the piece he was promised");
    assert_eq!(carol_sid, results[1].coins[0].statechain_id, "carol adopted her piece");

    // Both recipients hold a CHILD bundle — i.e. the in-ladder route ran, not the plain split (whose
    // pieces are ordinary V1 sub-coins with no child bundle at all).
    let bob_cb = mercuryrustlib::tesr::load_child(&cc, "sdk69_bob", &bob_sid)
        .await?
        .ok_or(anyhow!("bob did not adopt a child bundle — transfer_many took the PLAIN split [B1]"))?;
    let carol_cb = mercuryrustlib::tesr::load_child(&cc, "sdk69_carol", &carol_sid)
        .await?
        .ok_or(anyhow!("carol did not adopt a child bundle — transfer_many took the PLAIN split [B1]"))?;

    // ONE split state, N children: same parent lineage, same SP, DIFFERENT outputs.
    assert_eq!(bob_cb.parent_statechain_id, alice_sid, "bob's child descends from alice's coin");
    assert_eq!(carol_cb.parent_statechain_id, alice_sid, "carol's child descends from the same coin");
    let sp_hex = bob_cb.parent.current().state.signed_tx.clone();
    assert_eq!(
        sp_hex, carol_cb.parent.current().state.signed_tx,
        "both pieces are carved by the SAME split state SP (one tx, two recipients)"
    );
    assert_eq!((bob_cb.sp_vout, carol_cb.sp_vout), (0, 1), "SP.out[j] == recipients[j]");
    let sp = parse_tx(&sp_hex)?;
    // SP spends X_m.out[0] — a DESCENDANT of the trigger, NOT a rival for F. This is the whole fix.
    let x_m = &bob_cb.parent.current().extension;
    assert_eq!(
        sp.input[0].previous_output.txid.to_string(), x_m.txid,
        "SP must spend X_m.out[0], not the funding outpoint F"
    );
    assert_eq!(sp.input[0].previous_output.vout, 0);
    assert_ne!(
        sp.input[0].previous_output.txid.to_string(), f_txid,
        "SP must NOT spend F — that is the trigger's outpoint and would be racing it [B1]"
    );
    // 2 pieces + change + the P2A anchor, with the exact amounts requested.
    assert_eq!(sp.output.len(), 4, "SP = 2 pieces + change + P2A anchor");
    assert_eq!(sp.output[0].value, PAY_BOB, "SP.out[0] pays bob his exact amount");
    assert_eq!(sp.output[1].value, PAY_CAROL, "SP.out[1] pays carol her exact amount");
    assert_eq!(sp.output[3].value, mercurylib::tesr::P2A_VALUE, "the P2A anchor comes last");
    // Value conservation, checked here as well as in the builder: nothing minted, nothing burned.
    let expected_total = mercurylib::tesr::tier_out_total(x_m.out_value, 3, bob_cb.parent.fee_rate)
        .ok_or(anyhow!("no tier_out_total"))?;
    let payload: u64 = sp.output[..3].iter().map(|o| o.value).sum();
    assert_eq!(payload, expected_total, "Sum(pieces + change) == X_m.out[0] - committed fee for 3");
    let change_sats = sp.output[2].value;
    assert_eq!(
        alice.get_balance().await?.available_sats, change_sats,
        "alice keeps exactly SP.out[2] as her change"
    );
    // Her change is itself a first-class child, exitable on its own.
    let alice_change_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk69_alice")
        .await?
        .coins
        .iter()
        .find(|c| {
            c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                && c.amount == Some(change_sats as u32)
        })
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice booked no change coin"))?;
    assert!(
        mercuryrustlib::tesr::load_child(&cc, "sdk69_alice", &alice_change_sid).await?.is_some(),
        "alice's change is an exitable child claim"
    );
    // The batch appears in payment history (an in-ladder piece never goes through transfer_sender).
    let transfers = alice.get_transfers().await?;
    assert_eq!(
        transfers.iter().filter(|a| a.action == "Transfer").count(),
        2,
        "both off-chain payments are recorded in history"
    );
    println!("SDK69 - [2] ONE in-ladder split SP (spending X_m.out[0], NOT F) paid bob {PAY_BOB} at SP.out[0] and carol {PAY_CAROL} at SP.out[1]; alice kept {change_sats} as a child claim");

    // ---- [3] The whole batch was off-chain. ----------------------------------------------------
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "two recipients paid with ZERO on-chain footprint: F is still unspent"
    );
    println!("SDK69 - [3] F ({f_txid}:{f_vout}) is UNSPENT after paying two people ✓");

    // ---- [4] Run the B1 attack for real: alice fires her retained, un-timelocked trigger. -------
    // The SDK refuses to exit a spent parent (the HF-4 guard), so this simulates the patched client
    // B1 assumes: broadcast the raw `T` that was co-signed at establish and never expires.
    assert!(
        alice.unilateral_exit(Some(vec![alice_sid.clone()]), None).await.is_err(),
        "the SDK itself must refuse to exit the spent parent"
    );
    cc.electrum_client
        .transaction_broadcast_raw(&hex::decode(&retained_trigger)?)
        .map_err(|e| anyhow!("the retained trigger should still relay (that is the premise of B1): {e}"))?;
    bitcoin_core::generatetoaddress(2, &core)?;
    assert!(
        is_outpoint_spent(&cc, &f_txid, f_vout),
        "the attack landed: the retained trigger consumed F"
    );
    println!("SDK69 - [4] alice broadcast her retained trigger T ({trigger_txid}); F is now SPENT — on the plain-split lane this is where both recipients' pieces would die");

    // ...and both recipients exit anyway, for their exact amounts, because their chains DESCEND from
    // that very trigger: T -> X_m -> SP -> ext_child -> state_child.
    exit_child(&bob, &bob_sid, &core, "bob").await?;
    exit_child(&carol, &carol_sid, &core, "carol").await?;
    bitcoin_core::generatetoaddress(2, &core)?;
    for (name, cb, want) in
        [("bob", &bob_cb, PAY_BOB), ("carol", &carol_cb, PAY_CAROL)]
    {
        let st = parse_tx(&cb.child_state.signed_tx)?;
        cc.electrum_client
            .transaction_get(&st.txid())
            .map_err(|e| anyhow!("{name}'s final child state is not on-chain: {e}"))?;
        let paid = st.output[0].value;
        assert!(
            paid < want && paid > want - 4 * (mercurylib::tesr::TIER_VBYTES * 2 + 240),
            "{name} exits with {want} sat minus only the pre-committed tier fees, got {paid}"
        );
        println!("SDK69 - {name} exited to their own key with {paid} sat (of {want} promised; the rest is the pre-committed tier fees)");
    }

    println!("SDK69 - ✓ PASS: `transfer_many` on a LADDERED parent no longer builds a plain split of the coin's funding outpoint. It carves ONE in-ladder split state SP over X_m.out[0] — 2 recipient children + change + P2A — conveys each child with the standard key handover, and keeps the whole batch off-chain. The [B1] attack was then executed for real: alice broadcast the retained, un-timelocked trigger and consumed F, which on the old lane would have voided the split and destroyed BOTH recipients' pieces while paying her the full parent value. Instead bob and carol each unilaterally exited for their exact amounts, because SP descends from that trigger rather than racing it.");
    Ok(())
}
