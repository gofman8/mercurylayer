//! E2E (token-carrier watchtower, CTES-R): the `auto_exit_due` watchtower AUTO-PROTECTS a received
//! token carrier that is nearing its clawback deadline, so an idle receiver cannot lose the
//! allocation to the sender's stale, RGB-unaware backup of the shared funding output `F`.
//!
//! **MIGRATED TO THE COLOURED LANE.** On the flat lane a received piece was a sub-coin with a
//! `branch-<id>` exit chain, and protecting it meant BROADCASTING that branch: one locktime-0
//! transaction, one block, done. With `colored_ladder` ON a received piece is a COLOURED CHILD —
//! there is no `branch-` row at all, and `sqlite_manager::get_backup_txs` is a `fetch_one`, which is
//! why the un-migrated test died on "no rows returned by a query that expected to return at least
//! one row". Its exit material is the five-tier chain `T -> X_m -> SP -> ext_child -> state_child`
//! in its `ctesr-` bundle, and every rung of that chain carries an RGB state transition.
//!
//! **What the test proved is preserved, and the machinery it proves it about was extended to match**
//! (`auto_exit_due`, the coloured-children loop):
//!
//!   * the ACTION is now the pre-signed WALK, driven through `unilateral_exit`, not a branch
//!     broadcast — broadcasting any RGB-unaware spend of `F` destroys the allocation, which is the
//!     very thing this watchtower exists to prevent;
//!   * the DEADLINE gets a HEAD START. A flat branch was locktime-0 and confirmed in one block, so
//!     `L0 = H_deposit + initlock` was a usable deadline. A walk of RELATIVE timelocks must be
//!     STARTED at least `Σ csv` blocks before `L0`, and the head start is read off the child's own
//!     chain rather than guessed.
//!
//! (A) ISSUED/coloured-root carrier is NOT acted on: it has no ancestor — no `branch-` row and no
//!     `ctesr-` bundle of its own — so no stale backup can race it, at any margin.
//! (B) alice sends 250 to bob → bob holds a COLOURED CHILD whose chain roots at alice's carrier
//!     funding `F`; its effective deadline is `L0 - Σ csv`.
//! (C) Comfortably ahead of it, the watchtower does NOTHING (the whole chain stays off-chain, `F`
//!     unspent, 0 vB of rent).
//! (D) Near it, `auto_exit_due` ACTS: a `TokenCarrierMaterialized` event fires and the watchtower
//!     drives the walk — pass after pass, mining only what the SDK reports it is waiting for —
//!     until all five RGB-aware tiers are mined, `F` is spent by `T`, and the leaf consignment
//!     validates against the CHAIN ALONE for the full 250.
//! (E) The clawback is DEFEATED: after mining past `L0`, a broadcast of the sender's matured,
//!     RGB-unaware backup FAILS — the `F` it needs was already spent by the walk. Without the
//!     watchtower an idle bob would have lost the allocation here.
//!
//! Run: SDK_E2E=34 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}
async fn add_tokens(cc: &ClientConfig, w: &UtexoWallet, n: usize) -> Result<()> {
    for _ in 0..n {
        let t = prepaid_token(cc).await?;
        w.add_prepaid_token(&t).await;
    }
    Ok(())
}
async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances().await?.into_iter().find(|t| t.asset_id == asset).map(|t| t.balance).unwrap_or(0))
}
async fn wait_token_balance(w: &UtexoWallet, asset: &str, want: u64) -> Result<()> {
    for _ in 0..60 {
        w.claim().await?;
        if token_balance(w, asset).await? == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("settled balance of {asset} did not reach {want}"))
}
async fn wait_carrier(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str, asset: &str, units: u64) -> Result<mercuryrustlib::Coin> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        if token_balance(w, asset).await? >= units {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?;
            if let Some(c) = rec.coins.iter().rev().find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(mercury_utexo_sdk::tokens::TOKEN_CARRIER_SATS as u32)) {
                return Ok(c.clone());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: {units} of {asset} carrier did not confirm"))
}
fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}
fn mine_and_sync(cc: &ClientConfig, core: &str, n: u32) -> Result<u32> {
    let start = tip(cc)?;
    let target = start + n;
    let mut mined = 0;
    while mined < n {
        let batch = (n - mined).min(200);
        bitcoin_core::generatetoaddress(batch, core)?;
        mined += batch;
    }
    for _ in 0..120 {
        if tip(cc)? >= target {
            return Ok(tip(cc)?);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("electrs did not catch up to height {target}"))
}
/// Mine one block at a time and do not return until the INDEXER has seen each one — the rgb-lib
/// resolver races electrs otherwise and reports a well-mined tier as "can't be located" (sdk75/77).
fn mine_synced(cc: &ClientConfig, core: &str, n: u32) -> Result<()> {
    for _ in 0..n {
        let before = tip(cc)?;
        bitcoin_core::generatetoaddress(1, core)?;
        for _ in 0..60 {
            if tip(cc)? > before {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    Ok(())
}
fn onchain(cc: &ClientConfig, txid: &str) -> Option<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::Txid;
    let t = Txid::from_str(txid).ok()?;
    cc.electrum_client.transaction_get(&t).ok()
}
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    Ok(!cc.electrum_client.script_list_unspent(spk)?.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}
/// Confirmation height of `txid`, read from the history of the address it pays at `vout` — the same
/// anchor `deposit_anchored_deadline` uses inside the SDK.
fn confirmation_height(cc: &ClientConfig, txid: &str, vout: u32) -> Result<u32> {
    use electrum_client::bitcoin::Txid;
    let t = Txid::from_str(txid)?;
    let tx = cc.electrum_client.transaction_get(&t)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    cc.electrum_client
        .script_get_history(spk)?
        .iter()
        .find(|h| h.tx_hash == t && h.height > 0)
        .map(|h| h.height as u32)
        .ok_or_else(|| anyhow!("{txid} is not confirmed yet"))
}
fn drain_materialized(rx: &mut tokio::sync::broadcast::Receiver<WalletEvent>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let WalletEvent::TokenCarrierMaterialized { statechain_id, .. } = ev {
            out.push(statechain_id);
        }
    }
    out
}
/// The sid of the one adopted `ctesr-` child in `wallet_name`.
async fn adopted_child_sid(cc: &ClientConfig, wallet_name: &str) -> Result<Option<String>> {
    let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins;
    for c in coins.iter().filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0) {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if mercuryrustlib::tesr::load_child(cc, wallet_name, &sid).await?.is_some() {
            return Ok(Some(sid));
        }
    }
    Ok(None)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk34_alice", "./rgb-data-sdk34_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;

    let mut alice_cfg = SdkConfig::regtest("sdk34_alice");
    alice_cfg.colored_ladder = true;
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let mut bob_cfg = SdkConfig::regtest("sdk34_bob");
    bob_cfg.colored_ladder = true;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    add_tokens(&cc, &alice, 1).await?;
    let asset = alice.issue_token("WT", "Watch Token", 0, 1000).await?;
    let carrier = wait_carrier(&cc, &alice, "sdk34_alice", &core, &asset, 1000).await?;
    let carrier_id = carrier.statechain_id.clone().ok_or_else(|| anyhow!("carrier has no id"))?;
    let f_txid = carrier.utxo_txid.clone().ok_or_else(|| anyhow!("carrier has no funding txid"))?;
    let f_vout = carrier.utxo_vout.ok_or_else(|| anyhow!("carrier has no funding vout"))?;
    let carrier_bundle = mercuryrustlib::tesr::load(&cc, "sdk34_alice", &carrier_id).await?
        .ok_or_else(|| anyhow!("the carrier has no ladder — CTES-R is not on this wallet"))?;
    assert!(
        carrier_bundle.is_colored(),
        "this test drives the COLOURED lane; the carrier's ladder is plain, so nothing below is \
         testing what it claims to test"
    );
    println!("SDK34 - alice issued 1000 {asset} on carrier {carrier_id} (COLOURED ladder); F={f_txid}:{f_vout}, initlock={initlock}");

    // ===== (A) ISSUED carrier is not acted on ==================================================
    // An issued carrier has no ancestor: no `branch-<id>` chain and no `ctesr-<id>` bundle of its
    // own, so no stale backup exists that could race it. The watchtower must skip it even at an
    // absurd margin — the assertion is unchanged from the flat lane, and it still discriminates
    // (the coloured-children loop keys on `ctesr-` rows, of which alice has none yet).
    let a = alice.auto_exit_due(1_000_000).await?;
    assert!(
        !a.contains(&carrier_id),
        "the issued carrier has no ancestor and must NOT be acted on (got {a:?})"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout)?,
        "the watchtower broadcast something over an issued carrier's funding output"
    );
    println!("SDK34 - (A) issued carrier {carrier_id} is NOT acted on at margin 1_000_000 (no ancestor, no clawback risk) — F untouched");

    // ===== (B) set up a RECEIVED coloured child ================================================
    add_tokens(&cc, &alice, 3).await?;
    let r = alice.transfer_tokens(&asset, &bob_addr, 250).await?;
    assert!(r.used_split, "a token transfer is an off-chain split");
    wait_token_balance(&bob, &asset, 250).await?;
    let bob_piece = adopted_child_sid(&cc, "sdk34_bob").await?
        .ok_or_else(|| anyhow!("bob booked the tokens but adopted NO child bundle"))?;
    let bob_cb = mercuryrustlib::tesr::load_child(&cc, "sdk34_bob", &bob_piece).await?
        .ok_or_else(|| anyhow!("bob's child bundle vanished"))?;
    assert!(bob_cb.is_colored(), "bob's received child must be COLOURED — a plain tier over it destroys the 250");
    // The shape that broke the un-migrated test, asserted rather than assumed: there is NO
    // `branch-` row on this lane. The walk IS the branch.
    let all_rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(&cc.pool, "sdk34_bob").await?;
    assert!(
        !all_rows.iter().any(|(k, _)| *k == format!("branch-{bob_piece}")),
        "bob's coloured child unexpectedly has a `branch-` row — this test would then be exercising \
         the flat lane it was migrated off"
    );
    assert!(
        all_rows.iter().any(|(k, _)| *k == format!("ctesr-{bob_piece}")),
        "bob's coloured child has no `ctesr-` bundle — it has no exit material at all"
    );
    let chain = mercuryrustlib::tesr::child_exit_chain(&bob_cb);
    assert_eq!(chain.len(), 5, "a coloured child's chain is T, X_m, SP, ext_child, state_child");
    let root_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&chain[0].0)?)?;
    let root = root_tx.input[0].previous_output;
    assert_eq!(
        (root.txid.to_string(), root.vout), (f_txid.clone(), f_vout),
        "bob's chain must root at the carrier's own funding output, or it is not racing the \
         sender's backup at all"
    );
    // The effective deadline, computed exactly as the watchtower computes it: the deposit-anchored
    // `L0` of the funding output the chain roots at, MINUS the relative timelocks the walk must sit
    // through. A walk started later than this cannot finish before the sender's backup matures.
    let h_f = confirmation_height(&cc, &f_txid, f_vout)?;
    let l0 = h_f + initlock;
    let head_start: u32 = chain.iter().filter_map(|(_, csv)| *csv).map(u32::from).sum();
    let d = l0 - head_start;
    let headroom0 = d as i64 - tip(&cc)? as i64;
    assert!(headroom0 > 100, "bob's child should start well ahead of its deadline (headroom {headroom0})");
    println!("SDK34 - (B) alice→bob 250 {asset}: bob holds COLOURED CHILD {bob_piece} (no branch row, 5-tier chain rooted at F); L0={l0} (F confirmed at {h_f} + initlock {initlock}), Σcsv head start {head_start} ⟹ effective deadline D={d} (headroom {headroom0})");

    // ===== (C) watchtower does NOTHING while comfortably ahead of D ==============================
    let none = bob.auto_exit_due(20).await?;
    assert!(!none.contains(&bob_piece), "far from the deadline, the child must not be driven yet");
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout)?, "the chain is still off-chain (F unspent)");
    for (hex_tx, _) in chain.iter() {
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?;
        assert!(onchain(&cc, &tx.txid().to_string()).is_none(), "tier {} reached the chain early", tx.txid());
    }
    println!("SDK34 - (C) comfortably ahead of D (headroom {headroom0} > margin 20): watchtower is a no-op, all 5 tiers stay off-chain, 0 vB of rent");

    // ===== (D) near D → the watchtower DRIVES THE WALK ==========================================
    mine_and_sync(&cc, &core, 100)?;
    let headroom = d as i64 - tip(&cc)? as i64;
    assert!(headroom > 0, "still before the deadline when the watchtower fires (headroom {headroom})");
    let margin = (headroom + 30) as u32; // guarantees tip + margin >= D → due
    let mut rx = bob.subscribe();
    // NOTHING below calls `unilateral_exit` directly. Every tier that reaches the chain is put
    // there by an `auto_exit_due` pass, which is the only way this can be evidence about the
    // WATCHTOWER rather than about the exit API it happens to call.
    let tier_txids: Vec<String> = chain
        .iter()
        .map(|(hex_tx, _)| -> Result<String> {
            let tx: electrum_client::bitcoin::Transaction =
                electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?;
            Ok(tx.txid().to_string())
        })
        .collect::<Result<_>>()?;
    let mined_count = |cc: &ClientConfig| tier_txids.iter().filter(|t| onchain(cc, t).is_some()).count();
    let step = chain.iter().filter_map(|(_, csv)| *csv).max().unwrap_or(1) as u32 + 2;
    let mut passes = 0;
    let mut before = mined_count(&cc);
    loop {
        passes += 1;
        assert!(passes < 15, "the watchtower-driven walk did not converge ({before}/5 tiers mined)");
        let acted = bob.auto_exit_due(margin).await?;
        if passes == 1 {
            assert!(
                acted.contains(&bob_piece),
                "the watchtower must protect the near-deadline COLOURED CHILD (got {acted:?}). On \
                 this lane the child has no `branch-` row, so a watchtower that only knows how to \
                 broadcast one reports it as a safe flat coin and leaves the allocation to be \
                 destroyed."
            );
            let mat = drain_materialized(&mut rx);
            assert!(mat.contains(&bob_piece), "a TokenCarrierMaterialized event must fire for {bob_piece} (got {mat:?})");
        }
        // Batch the bulk and sync only the last block: on a long-lived regtest each single-block
        // `generatetoaddress` RPC costs seconds of wallet bookkeeping, so a per-block `mine_synced`
        // loop makes this section 100x slower than it needs to be. Same total blocks, same waits
        // that matter (the indexer only has to be current when the next pass READS the chain).
        if step > 1 {
            bitcoin_core::generatetoaddress(step - 1, &core)?;
        }
        mine_synced(&cc, &core, 1)?;
        let now = mined_count(&cc);
        if passes == 1 {
            // The trigger is out: `F` is spent by an RGB-AWARE transaction. That single fact is
            // what defeats (E), and it happened on the FIRST watchtower pass.
            assert!(is_outpoint_spent(&cc, &f_txid, f_vout)?, "the first pass must spend F with the child's trigger");
        }
        if now == tier_txids.len() {
            break;
        }
        assert!(
            now > before,
            "pass {passes} made no progress ({now}/5 tiers mined) — the watchtower stopped driving \
             a DUE child mid-walk, and a half-walked chain is not protection"
        );
        before = now;
    }
    mine_synced(&cc, &core, 3)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    for (hex_tx, _) in chain.iter() {
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?;
        assert!(onchain(&cc, &tx.txid().to_string()).is_some(), "tier {} never reached the chain", tx.txid());
        assert_eq!(
            tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(), 1,
            "every tier the watchtower broadcast must be RGB-AWARE (exactly one opret)"
        );
    }
    // The allocation survived. `get_asset_balance` is deliberately not the evidence (E7 measured it
    // reporting a full settled balance over a dead stock): the leaf consignment is validated against
    // the CHAIN ALONE (empty off-chain witness set), which only passes if every tier is really mined.
    let mut proof = bob.colored_child_exit_proof(&bob_piece).await;
    for _ in 0..20 {
        if proof.is_ok() {
            break;
        }
        let msg = proof.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        if !msg.contains("can't be located in the blockchain") {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        proof = bob.colored_child_exit_proof(&bob_piece).await;
    }
    let (proof_contract, proof_amount, _d) = proof.map_err(|e| anyhow!(
        "THE ALLOCATION DID NOT SURVIVE the watchtower-driven walk: {e}"
    ))?;
    assert_eq!(proof_contract, asset, "the surviving allocation is THIS contract");
    assert_eq!(proof_amount, 250, "all 250 units must survive");
    bob.probe_colored_child_tip(&bob_piece, 250).await
        .map_err(|e| anyhow!("the stock is DEAD after the walk: {e}"))?;
    assert!(
        bob.probe_colored_child_tip(&bob_piece, 251).await.is_err(),
        "the stock probe accepted MORE than the allocation — it is not reading the stock"
    );
    println!("SDK34 - (D) near D (headroom {headroom} ≤ margin {margin}): auto_exit_due PROTECTED {bob_piece} (TokenCarrierMaterialized) and drove all 5 RGB-aware tiers on chain over {passes} pass(es); F is spent by the child's trigger and the leaf consignment now validates against the CHAIN ALONE for 250 {asset} — settled on-chain, no SE");

    // ===== (E) the clawback is now DEFEATED =====================================================
    // The sender keeps ONE pre-signed, RGB-UNAWARE backup whose input is the same `F`. Mine past its
    // absolute locktime and try it: it FAILS, because the walk already spent `F`. Note what changed
    // with the lane — on the flat lane this backup RECOVERED the tokens for the sender; here it can
    // only destroy them (it carries no RGB transition), so the receiver's exposure is griefing
    // rather than theft. The defence, and this test, are the same either way.
    let alice_backup = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk34_alice", &carrier_id).await?;
    let alice_bk = alice_backup.iter().min_by_key(|b| b.tx_n)
        .ok_or_else(|| anyhow!("alice has no carrier backup"))?;
    let alice_bk_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&alice_bk.tx)?)?;
    assert_eq!(
        alice_bk_tx.input[0].previous_output.txid.to_string(), f_txid,
        "the sender's backup must spend the same F the walk spent, or it is no rival"
    );
    assert_eq!(
        alice_bk_tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(), 0,
        "the sender's retained backup must be the RGB-UNAWARE shape this section is about"
    );
    let l0_alice = mercurylib::utils::get_blockheight(alice_bk)?;
    mine_and_sync(&cc, &core, l0_alice.saturating_sub(tip(&cc)?) + 20)?;
    assert!(tip(&cc)? > l0_alice, "the sender's carrier backup is now mature (tip > L0={l0_alice})");
    let claw = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&alice_bk.tx)?);
    assert!(
        claw.is_err(),
        "the sender's stale backup must FAIL to broadcast — the F it needs was already spent by the \
         watchtower-driven walk"
    );
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout)?, "F remains spent by bob's trigger, not the sender's backup");
    assert_eq!(token_balance(&bob, &asset).await?, 250, "bob still holds all 250 after the failed sweep");
    println!("SDK34 - (E) CLAWBACK DEFEATED: even with its backup mature (tip {} > L0 {l0_alice}) the sender cannot spend F — {} — bob keeps 250; without the watchtower an idle bob would have lost the allocation here", tip(&cc)?, claw.err().map(|e| e.to_string().chars().take(80).collect::<String>()).unwrap_or_default());

    println!("SDK34 - SUCCESS: the auto_exit_due watchtower protects RECEIVED carriers on the COLOURED lane. A received piece is now a coloured CHILD with no `branch-` row, so the flat lane's 'broadcast the branch' has been replaced by driving the child's five RGB-aware tiers through unilateral_exit, against a deadline that is head-started by the walk's own Σcsv. Issued carriers (no ancestor, no rival backup) are still left untouched at any margin. The property sdk34 has always asserted is unchanged: an idle receiver is automatically protected — F is spent in time by an RGB-AWARE transaction, the allocation survives on chain, and the sender's stale RGB-unaware backup can never confirm.");
    Ok(())
}
