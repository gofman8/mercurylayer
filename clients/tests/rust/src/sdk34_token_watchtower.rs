//! E2E (token-carrier watchtower, CTES-R): a RECEIVED token piece has NO calendar deadline, and its
//! defence is EVENT-DRIVEN — `defend_ladders` answers a hostile trigger on the shared funding
//! output `F`, and nothing else ever needs to happen.
//!
//! **RE-DERIVED — the clawback this test defended against no longer exists.** Two earlier shapes:
//!
//!   * On the flat lane a received piece was a sub-coin with a `branch-<id>` exit chain, and the
//!     sender kept a pre-signed, RGB-unaware deposit backup over `F` with an absolute locktime
//!     `L0 = H_deposit + initlock`. An idle receiver lost the allocation the block that backup
//!     matured, so `auto_exit_due` forced the branch on chain before `L0`.
//!   * On the coloured lane the piece became a COLOURED CHILD (a `ctesr-` bundle, five RGB-aware
//!     tiers `T -> X_m -> SP -> ext_child -> state_child`), and this test drove `auto_exit_due` to
//!     walk it before `L0 - Σ csv` — still against the sender's retained flat backup.
//!
//! There is no flat backup any more. A carrier's ladder is co-signed at first sight of `F` in place
//! of the flat `tx1`; a transfer conveys `backup_transactions: []`; `coin.locktime` is `None` for
//! life. The sender holds NO absolute-locktime spend of `F` — the only pre-signed spends of `F` in
//! existence are the coloured trigger `T` and nothing else — so there is no height at which a
//! received piece becomes claw-back-able, no `L0`, and no head start to compute. `auto_exit_due`
//! acts on `exit_deadline_block`, which a laddered coin (and its children) never has; the child's
//! only exposure is the EVENT of `T` reaching the chain, and the per-block `defend_ladders` child
//! loop is what answers it.
//!
//! (A) ISSUED carrier: `auto_exit_due` and `deadline_safety_due` are no-ops at an absurd margin;
//!     `F` untouched.
//! (B) alice sends 250 to bob → bob holds a COLOURED CHILD rooted at alice's carrier funding `F`.
//!     Neither side holds a flat backup row; both coins carry `locktime == None`. `auto_exit_due`
//!     at an absurd margin does NOT act on the child — there is no height to be due against.
//! (C) THE OLD HORIZON PASSES WITH NOTHING HAPPENING. Both wallets idle past `H_F + initlock` —
//!     the block at which the sender's backup used to mature. Every automatic pass on both sides
//!     (`defend_ladders`, `auto_exit_due`, `deadline_safety_due`) is a no-op, `F` is unspent, all
//!     five tiers are off-chain: 0 vB of rent, and no clawback to defend against.
//! (D) A HOSTILE TRIGGER IS ANSWERED. An adversary broadcasts the parent's `T`. Bob only ever calls
//!     `defend_ladders()`; pass after pass it pushes the next matured tier — a `LadderDefended`
//!     event fires — until all five RGB-aware tiers are mined, `F` is spent by `T`, and the leaf
//!     consignment validates against the CHAIN ALONE for the full 250.
//! (E) THE SENDER HOLDS NO RIVAL. Zero flat rows for the carrier, `locktime == None`, the legacy
//!     `broadcast_backup_tx` entry point refuses her carrier by name ("no flat backup transaction
//!     to broadcast"), and every pre-signed spend of `F` her ladder row holds is RGB-aware (one
//!     OP_RETURN) and rooted at the very trigger bob's chain rides on.
//!
//! Run: SDK_E2E=34 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

const PAY: u64 = 250;
/// Far above any height a regtest coin could have been "due" at under the old calendar (`initlock`
/// is 1 000). Every deadline pass takes the margin as a PARAMETER, so this drives exactly the
/// branch a real deadline would have driven — and it must select nothing.
const HUGE_MARGIN: u32 = 1_000_000;

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
/// Confirmation height of `txid`, read from the history of the address it pays at `vout`. Used
/// only to locate the OLD horizon `H_F + initlock` — the block the sender's flat backup used to
/// mature at — so (C) can idle past it.
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
fn drain_defended(rx: &mut tokio::sync::broadcast::Receiver<WalletEvent>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(WalletEvent::LadderDefended { statechain_id, .. }) => out.push(statechain_id),
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
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
async fn coin_of(cc: &ClientConfig, name: &str, id: &str) -> Result<mercuryrustlib::Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?
        .coins.into_iter().find(|c| c.statechain_id.as_deref() == Some(id) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("{name} has no coin {id}"))
}
/// The flat backup rows stored for `sid` — `None` (no row) and an empty row both count as zero.
async fn flat_rows(cc: &ClientConfig, name: &str, sid: &str) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, name, sid)
        .await?
        .map_or(0, |rows| rows.len()))
}
fn txid_of(hex_tx: &str) -> Result<String> {
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?;
    Ok(tx.txid().to_string())
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
    let h_f = confirmation_height(&cc, &f_txid, f_vout)?;
    let old_horizon = h_f + initlock;
    println!("SDK34 - alice issued 1000 {asset} on carrier {carrier_id} (COLOURED ladder); F={f_txid}:{f_vout} confirmed at {h_f}; the OLD calendar horizon would have been {old_horizon} (initlock={initlock})");

    // ===== (A) ISSUED carrier is not acted on, at any margin ====================================
    // An issued carrier has no exit branch, and `auto_exit_due` acts only on a coin with a branch
    // (the one shape that has an `exit_deadline_block`); `deadline_safety_due` selects on
    // `coin_near_final`, which reads a `locktime` no laddered coin has. Both must be no-ops even at
    // an absurd margin — and must say so with `Ok`, not with an `Err` naming the carrier.
    let a = alice.auto_exit_due(HUGE_MARGIN).await?;
    assert!(
        !a.contains(&carrier_id),
        "the issued carrier has no exit branch and no deadline, and must NOT be acted on (got {a:?})"
    );
    let (re_anchored, severed) = alice.deadline_safety_due(HUGE_MARGIN).await.map_err(|e| {
        anyhow!("(A) deadline_safety_due must not go blind or report the carrier UNDEFENDED: {e:#}")
    })?;
    assert!(
        re_anchored.is_empty() && severed.is_empty(),
        "(A) the deadline pass acted on a coin with no calendar (re-anchored {re_anchored:?}, \
         severed {severed:?})"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout)?,
        "the watchtower broadcast something over an issued carrier's funding output"
    );
    assert_eq!(
        coin_of(&cc, "sdk34_alice", &carrier_id).await?.locktime,
        None,
        "a laddered carrier has no absolute calendar: coin.locktime must be None"
    );
    println!("SDK34 - (A) issued carrier {carrier_id} is NOT acted on at margin {HUGE_MARGIN} by either pass (no branch, no locktime) — F untouched");

    // ===== (B) set up a RECEIVED coloured child ================================================
    add_tokens(&cc, &alice, 3).await?;
    let r = alice.transfer_tokens(&asset, &bob_addr, PAY).await?;
    assert!(r.used_split, "a token transfer is an off-chain split");
    wait_token_balance(&bob, &asset, PAY).await?;
    let bob_piece = adopted_child_sid(&cc, "sdk34_bob").await?
        .ok_or_else(|| anyhow!("bob booked the tokens but adopted NO child bundle"))?;
    let bob_cb = mercuryrustlib::tesr::load_child(&cc, "sdk34_bob", &bob_piece).await?
        .ok_or_else(|| anyhow!("bob's child bundle vanished"))?;
    assert!(bob_cb.is_colored(), "bob's received child must be COLOURED — a plain tier over it destroys the 250");
    // There is NO `branch-` row on this lane, and no flat row on EITHER side. The walk IS the exit.
    let all_rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(&cc.pool, "sdk34_bob").await?;
    assert!(
        !all_rows.iter().any(|(k, _)| *k == format!("branch-{bob_piece}")),
        "bob's coloured child unexpectedly has a `branch-` row — this test would then be exercising \
         the retired flat lane"
    );
    assert!(
        all_rows.iter().any(|(k, _)| *k == format!("ctesr-{bob_piece}")),
        "bob's coloured child has no `ctesr-` bundle — it has no exit material at all"
    );
    assert_eq!(
        flat_rows(&cc, "sdk34_bob", &bob_piece).await?,
        0,
        "the receiver of a piece must hold ZERO flat backup rows for it — a conveyed flat backup is \
         refused by name, and there is none to convey"
    );
    assert_eq!(
        flat_rows(&cc, "sdk34_alice", &carrier_id).await?,
        0,
        "the SENDER must hold ZERO flat backup rows for the carrier — a flat rung would be exactly \
         the matured spend of F this test used to defend against"
    );
    assert_eq!(coin_of(&cc, "sdk34_bob", &bob_piece).await?.locktime, None, "bob's piece has no locktime");
    assert!(
        bob_cb.parent_flat_backups.is_empty(),
        "the child bundle must convey an EMPTY parent flat chain — got {}",
        bob_cb.parent_flat_backups.len()
    );
    let chain = mercuryrustlib::tesr::child_exit_chain(&bob_cb);
    assert_eq!(chain.len(), 5, "a coloured child's chain is T, X_m, SP, ext_child, state_child");
    let root_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&chain[0].0)?)?;
    let root = root_tx.input[0].previous_output;
    assert_eq!(
        (root.txid.to_string(), root.vout), (f_txid.clone(), f_vout),
        "bob's chain must root at the carrier's own funding output"
    );
    let trigger_txid = root_tx.txid().to_string();
    let tier_txids: Vec<String> = chain.iter().map(|(hex_tx, _)| txid_of(hex_tx)).collect::<Result<_>>()?;
    // No height to be due against: even at an absurd margin the child is left alone.
    let none = bob.auto_exit_due(HUGE_MARGIN).await?;
    assert!(
        !none.contains(&bob_piece),
        "auto_exit_due acted on the coloured child at margin {HUGE_MARGIN} — it invented a deadline \
         for a coin that has none (got {none:?})"
    );
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout)?, "the chain is still off-chain (F unspent)");
    println!("SDK34 - (B) alice→bob {PAY} {asset}: bob holds COLOURED CHILD {bob_piece} (no branch row, no flat row on either side, empty parent chain, 5-tier chain rooted at F, trigger {trigger_txid}); auto_exit_due({HUGE_MARGIN}) leaves it alone");

    // ===== (C) THE OLD HORIZON PASSES, AND NOTHING HAPPENS ======================================
    // Under the flat chain this is the block the sender's backup matured at, and an idle receiver
    // lost the allocation here. Idle both wallets past it and run every automatic pass on both
    // sides: each must be a verified no-op, `F` must be unspent, every tier off-chain.
    let target = old_horizon + 20;
    let now = tip(&cc)?;
    if now < target {
        mine_and_sync(&cc, &core, target - now)?;
    }
    let tip_c = tip(&cc)?;
    assert!(tip_c > old_horizon, "(C) the tip ({tip_c}) must be past the old horizon {old_horizon}");
    alice.claim().await?;
    bob.claim().await?;
    let a_def = alice.defend_ladders().await.map_err(|e| anyhow!("(C) alice's defence pass went blind: {e:#}"))?;
    let b_def = bob.defend_ladders().await.map_err(|e| anyhow!("(C) bob's defence pass went blind: {e:#}"))?;
    assert!(a_def.is_empty() && b_def.is_empty(), "(C) a defence pass broadcast something with F unspent (alice {a_def:?}, bob {b_def:?})");
    let a_exit = alice.auto_exit_due(HUGE_MARGIN).await?;
    let b_exit = bob.auto_exit_due(HUGE_MARGIN).await?;
    assert!(a_exit.is_empty() && b_exit.is_empty(), "(C) auto_exit_due acted past the old horizon (alice {a_exit:?}, bob {b_exit:?})");
    let (a_re, a_sev) = alice.deadline_safety_due(HUGE_MARGIN).await.map_err(|e| anyhow!("(C) alice's deadline pass must not go blind or report UNDEFENDED: {e:#}"))?;
    assert!(a_re.is_empty() && a_sev.is_empty(), "(C) the deadline pass acted past the old horizon (re-anchored {a_re:?}, severed {a_sev:?})");
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout)?,
        "(C) F was spent while both wallets sat idle past the old horizon — something still holds a \
         matured spend of F"
    );
    for t in tier_txids.iter() {
        assert!(onchain(&cc, t).is_none(), "(C) tier {t} reached the chain while idle — idle coins must cost 0 vB");
    }
    assert_eq!(token_balance(&bob, &asset).await?, PAY, "(C) bob still holds all {PAY}");
    println!("SDK34 - (C) tip {tip_c} > old horizon {old_horizon}: every pass on both sides is a no-op, F unspent, all 5 tiers off-chain — there is no clawback to defend against");

    // ===== (D) a HOSTILE TRIGGER is answered by defend_ladders ==================================
    // The child's only exposure. An adversary (anyone holding the co-signed `T` — the sender, a
    // griefer) puts it on chain; from here the CSVs are counting and bob must race. NOTHING below
    // calls `unilateral_exit` or `auto_exit_due`: every tier that reaches the chain is put there by
    // a `defend_ladders` pass, which is the only way this is evidence about the WATCHTOWER rather
    // than about the exit API it happens to call.
    let step = chain.iter().filter_map(|(_, csv)| *csv).max().unwrap_or(1) as u32 + 2;
    cc.electrum_client.transaction_broadcast_raw(&hex::decode(&chain[0].0)?)
        .map_err(|e| anyhow!("(D) the adversary could not broadcast the parent's trigger: {e}"))?;
    mine_synced(&cc, &core, 1)?;
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout)?, "(D) the hostile trigger must have spent F");
    // Let the trigger mature past the longest CSV, so every pass below is expected to make progress.
    mine_synced(&cc, &core, step - 1)?;
    let mut rx = bob.subscribe();
    let mined_count = |cc: &ClientConfig| tier_txids.iter().filter(|t| onchain(cc, t).is_some()).count();
    let mut saw_defended = false;
    let mut passes = 0;
    let mut before = mined_count(&cc);
    assert_eq!(before, 1, "(D) only the trigger is on chain before the first defence pass");
    loop {
        passes += 1;
        assert!(passes < 15, "(D) the watchtower-driven walk did not converge ({before}/5 tiers mined)");
        let acted = bob.defend_ladders().await.map_err(|e| anyhow!(
            "(D) defend_ladders went BLIND (or reported the child lost) mid-race: {e:#}"
        ))?;
        if drain_defended(&mut rx).contains(&bob_piece) {
            saw_defended = true;
            assert!(acted.contains(&bob_piece), "(D) a LadderDefended event without the pass reporting the child acted on");
        }
        // Batch the bulk and sync only the last block (the indexer only has to be current when the
        // next pass READS the chain).
        if step > 1 {
            bitcoin_core::generatetoaddress(step - 1, &core)?;
        }
        mine_synced(&cc, &core, 1)?;
        let now = mined_count(&cc);
        if now == tier_txids.len() {
            break;
        }
        assert!(
            now > before,
            "(D) pass {passes} made no progress ({now}/5 tiers mined) — the watchtower stopped \
             answering a hostile trigger mid-walk, and a half-walked chain is not protection"
        );
        before = now;
    }
    assert!(saw_defended, "(D) defend_ladders must emit LadderDefended for {bob_piece} so wrappers can forward it");
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
    assert_eq!(proof_amount, PAY, "all {PAY} units must survive");
    bob.probe_colored_child_tip(&bob_piece, PAY).await
        .map_err(|e| anyhow!("the stock is DEAD after the walk: {e}"))?;
    assert!(
        bob.probe_colored_child_tip(&bob_piece, PAY + 1).await.is_err(),
        "the stock probe accepted MORE than the allocation — it is not reading the stock"
    );
    println!("SDK34 - (D) a hostile trigger was ANSWERED: defend_ladders alone drove all 5 RGB-aware tiers on chain over {passes} pass(es) (LadderDefended fired); F is spent by the child's trigger and the leaf consignment validates against the CHAIN ALONE for {PAY} {asset}");

    // ===== (E) THE SENDER HOLDS NO RIVAL ========================================================
    // What (E) used to do was mine past the sender's backup locktime and prove the matured backup
    // could no longer broadcast because the walk had spent `F` first. There is no such backup: the
    // sender holds no flat row, no locktime, and no RGB-unaware spend of `F` at all.
    assert_eq!(flat_rows(&cc, "sdk34_alice", &carrier_id).await?, 0, "(E) alice holds no flat backup row for the carrier");
    assert_eq!(coin_of(&cc, "sdk34_alice", &carrier_id).await?.locktime, None, "(E) alice's carrier has no locktime");
    let legacy = mercuryrustlib::broadcast_backup_tx::execute(&cc, "sdk34_alice", &carrier_id, None, None).await;
    let legacy_msg = legacy.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        legacy_msg.contains("no flat backup transaction to broadcast"),
        "(E) the legacy flat-backup broadcast must refuse a laddered carrier BY NAME — got: {legacy_msg:?}"
    );
    let alice_row = mercuryrustlib::tesr::load(&cc, "sdk34_alice", &carrier_id).await?
        .ok_or_else(|| anyhow!("(E) alice's ladder row for the carrier vanished"))?;
    assert_eq!(
        alice_row.trigger.txid, trigger_txid,
        "(E) the only spend of F the sender holds must be the very trigger bob's chain rides on"
    );
    for t in alice_row.exit_tiers() {
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(&t.signed_tx)?)?;
        assert_eq!(
            tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(), 1,
            "(E) every pre-signed tier the sender retains must be RGB-AWARE — an RGB-unaware spend of F \
             is the clawback shape, and none may exist"
        );
    }
    assert_eq!(token_balance(&bob, &asset).await?, PAY, "(E) bob keeps all {PAY}");
    println!("SDK34 - (E) the sender holds NO rival: 0 flat rows, locktime=None, the legacy broadcast refuses by name ({}), and every tier she retains is RGB-aware and rooted at bob's own trigger", legacy_msg.chars().take(90).collect::<String>());

    println!("SDK34 - SUCCESS: a RECEIVED coloured child has NO calendar. The old horizon (H_F + initlock) passed with every automatic pass on both sides a verified no-op, F unspent and 0 vB spent; the child's only exposure is a hostile trigger, which defend_ladders answered tier by tier until the allocation settled on chain; and the sender holds no RGB-unaware spend of F to claw anything back with.");
    Ok(())
}
