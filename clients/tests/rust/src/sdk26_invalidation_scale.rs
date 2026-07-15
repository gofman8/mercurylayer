//! E2E (invalidation at SCALE): old-state invalidation must keep holding — and stay measurably
//! affordable — as the off-chain tree grows in DEPTH and WIDTH.
//!
//! (a) DEPTH LADDER: one 200k deposit is split 4 times in a chain (each split consumes the
//!     previous piece). After EVERY split we assert the three invalidation facts at once:
//!       - the parent is TERMINAL at the SE, checked via the PUBLIC spend-budget endpoint
//!         (REQ-18/INV-24 — anyone can audit that the consumed node is dead);
//!       - the sub-coin's stored exit branch has exactly `depth` rows (root-first chain back to
//!         the on-chain deposit outpoint);
//!       - `estimate_exit_cost().branch_txs == depth` (the advertised exit cost is the REAL
//!         pre-signed material, INV-17).
//! (b) MEASURED COST TABLE: after each depth one machine-parsable line is printed:
//!       ECON depth=<d> branch_txs=<n> branch_vb=<x> backup_vb=<y> total_vb=<z> wait_blocks=<w>
//!     plus the exact per-hop branch-vbyte deltas (expected ~155 vB per 1-in-2-out split tx,
//!     asserted within 100..=250) — the economics doc is generated from these lines.
//! (c) WIDTH: a second wallet deposits 100k and `transfer_many`s THREE amounts to one receiver
//!     wallet in ONE off-chain split (3 pieces + change from a single co-signed tx). Every piece
//!     is claimable, carries a 1-tx branch, and shares the same (un-broadcast) funding txid.
//! (d) EXIT AT DEPTH: the DEEPEST leaf exits unilaterally. The first call broadcasts the whole
//!     4-tx branch immediately (locktime-free, INV-4) and reports complete:false with
//!     wait_blocks ~ initlock (a FRESH sub-ladder: depth does not consume interval — the leaf
//!     backup is anchored at the split tip). After mining out the wait the second call completes
//!     and the funds land at the wallet's OWN backup address, verified on-chain via electrum.
//!
//! Run: SDK_E2E=26 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
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

/// Whether `txid:vout` is spent according to electrs. Electrum errors PROPAGATE (a transient
/// electrs failure must fail the run loudly, not vacuously pass the spent-check).
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let raw = cc.electrum_client.transaction_get_raw(&Txid::from_str(txid)?)?;
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw)?;
    let spk = &tx
        .output
        .get(vout as usize)
        .ok_or_else(|| anyhow!("outpoint {txid}:{vout} does not exist"))?
        .script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk)?;
    Ok(!listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

/// Deposit `amount` to `w`, mine, and poll claim until the wallet holds the CONFIRMED coin.
/// Returns the coin record (statechain id, outpoint, locktime of the deposit backup).
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

/// Poll claim until `n` incoming transfers have been claimed in total.
async fn claim_n(w: &UtexoWallet, n: u32) -> Result<()> {
    let mut got = 0u32;
    for _ in 0..60 {
        got += w.claim().await?.claimed_transfers;
        if got >= n {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("expected {n} claimed transfers, got {got}"))
}

pub async fn execute() -> Result<()> {
    // V2DEF-5: this test exercises V1-specific mechanisms (decrementing-locktime stale-state /
    // invalidation / re-anchor) that V2 replaces for roots but that remain for V1 split sub-coins &
    // the LN lane. Pin V1 so it stays a V1-lane regression test under the V2 default.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let initlock = si.initlock;
    let interval = si.interval;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk26_alice"), None).await?;

    // ===== (a)+(b) DEPTH LADDER: one 200k coin split 4 times in a chain =========================
    let deposit = deposit_confirmed_coin(&cc, &alice, "sdk26_alice", 200_000).await?;
    let root_id = deposit
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow!("deposit has no statechain id"))?;
    let o_txid = deposit.utxo_txid.clone().ok_or_else(|| anyhow!("no utxo_txid"))?;
    let o_vout = deposit.utxo_vout.ok_or_else(|| anyhow!("no utxo_vout"))?;
    println!("SDK26 - alice deposited 200k; funding O = {o_txid}:{o_vout}; initlock={initlock}");

    // Piece sizes respect piece >= 330 and change = parent - piece - reserve >= 330 at every
    // depth (reserve = (parent/100).clamp(300, 2000): 2000, 1200, 800, 500 here).
    let piece_sizes: [u64; 4] = [120_000, 80_000, 50_000, 30_000];
    let mut parent_id = root_id.clone();
    let mut branch_vbs: Vec<u64> = Vec::new();
    let mut leaf_id = String::new();
    for (i, &sz) in piece_sizes.iter().enumerate() {
        let depth = (i + 1) as u32;
        add_tokens(&cc, &alice, 2).await?; // two fresh statechain slots per split
        let (piece, change) = alice.split_coin(&parent_id, sz).await?;

        // 1. Parent TERMINAL — via the PUBLIC endpoint (what any receiver/auditor sees).
        let (budget, finalized, terminal) =
            mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_id).await?;
        assert!(
            terminal,
            "depth {depth}: split parent must be TERMINAL at the SE (budget {budget:?}, finalized {finalized})"
        );

        // 2. Branch row count == depth (root-first chain back to the on-chain deposit).
        let rows = mercuryrustlib::sqlite_manager::get_backup_txs(
            &cc.pool,
            "sdk26_alice",
            &format!("branch-{piece}"),
        )
        .await?;
        assert_eq!(
            rows.len() as u32,
            depth,
            "depth {depth}: sub-coin branch must have exactly `depth` txs"
        );

        // 3. The advertised exit cost measures the real pre-signed material. Recompute the vbyte
        //    total INDEPENDENTLY from the stored rows (branch txs + latest backup): this pins both
        //    the arithmetic and WHICH txs the estimator counts (INV-17) — asserting
        //    total == branch + backup would only re-verify the estimator's own constructor.
        let est = alice.estimate_exit_cost(&piece).await?;
        assert_eq!(est.branch_txs, depth, "estimate_exit_cost.branch_txs == depth");
        let mut independent_vb = 0u64;
        for row in &rows {
            let tx: electrum_client::bitcoin::Transaction =
                electrum_client::bitcoin::consensus::deserialize(&hex::decode(&row.tx)?)?;
            independent_vb += tx.vsize() as u64;
        }
        let piece_backups =
            mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk26_alice", &piece).await?;
        let piece_latest = piece_backups
            .iter()
            .max_by_key(|b| b.tx_n)
            .ok_or_else(|| anyhow!("no backup tx stored for piece {piece}"))?;
        let piece_backup: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(&piece_latest.tx)?)?;
        independent_vb += piece_backup.vsize() as u64;
        assert_eq!(
            est.total_vbytes, independent_vb,
            "depth {depth}: total_vbytes must equal the independently measured stored txs (INV-17)"
        );
        // Fresh sub-ladder: the leaf backup is anchored at the SPLIT tip, and nothing is mined
        // between the split co-sign and this estimate, so the wait must sit within ONE interval
        // of initlock. This band pins the exact regression a ladder-INHERITING sub-coin would
        // show — locktime = h_split + initlock - depth*interval drops wait_blocks by >= interval
        // already at depth 1 (qt >= 1) — on both server profiles (1000/10 and 10000/100).
        assert!(
            est.wait_blocks > 0
                && est.wait_blocks <= initlock
                && est.wait_blocks + interval > initlock,
            "depth {depth}: fresh leaf ladder must wait within one interval of initlock (got {} vs initlock {initlock}, interval {interval})",
            est.wait_blocks
        );

        // Machine-parsable cost line (harvested for the economics doc).
        println!(
            "ECON depth={depth} branch_txs={} branch_vb={} backup_vb={} total_vb={} wait_blocks={}",
            est.branch_txs, est.branch_vbytes, est.backup_vbytes, est.total_vbytes, est.wait_blocks
        );

        // INVARIANT: branch vbytes STRICTLY increase with depth.
        if let Some(prev) = branch_vbs.last() {
            assert!(
                est.branch_vbytes > *prev,
                "branch vbytes must strictly increase with depth ({} !> {prev})",
                est.branch_vbytes
            );
        }
        branch_vbs.push(est.branch_vbytes);

        println!(
            "SDK26 - depth {depth}: parent {parent_id} TERMINAL (budget {budget:?}, finalized {finalized}); piece {piece} ({sz} sats, {depth}-tx branch), change {change}"
        );
        parent_id = piece.clone();
        leaf_id = piece;
    }

    // INVARIANT: per-hop branch growth is one 1-in-2-out split tx (~155 vB), band 100..=250.
    let mut prev = 0u64;
    let mut deltas: Vec<u64> = Vec::new();
    for vb in &branch_vbs {
        deltas.push(vb - prev);
        prev = *vb;
    }
    for (i, d) in deltas.iter().enumerate() {
        assert!(
            (100..=250).contains(d),
            "per-hop branch vbyte delta out of band at depth {}: {d} vB (expected ~155, band 100..=250)",
            i + 1
        );
    }
    println!("ECON depth_deltas_vb={deltas:?} band=100..=250 expected_per_hop_vb=~155");
    println!("SDK26 - depth ladder complete: 4 splits, every parent publicly terminal, branch grows ~155 vB/hop");

    // ===== (c) WIDTH: one split fans out to 3 recipient pieces + change =========================
    let (dave, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk26_dave"), None).await?;
    let (erin, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk26_erin"), None).await?;
    let erin_addr = erin.get_utexo_address().await?;

    let dave_dep = deposit_confirmed_coin(&cc, &dave, "sdk26_dave", 100_000).await?;
    let dave_parent = dave_dep
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow!("dave's deposit has no statechain id"))?;
    add_tokens(&cc, &dave, 4).await?; // 3 recipient pieces + 1 change slot
    let results = dave
        .transfer_many(&[
            (erin_addr.clone(), 20_000),
            (erin_addr.clone(), 25_000),
            (erin_addr.clone(), 30_000),
        ])
        .await?;
    assert_eq!(results.len(), 3, "one result per recipient amount");
    // NOTE: `results[i].used_split` is unconditionally `true` in transfer_many (it has no
    // non-split path), so asserting it would be tautological. The structural proof that ONE
    // off-chain split funds all pieces is below: one shared funding txid across the three
    // pieces + change, and a single 4-output branch tx.
    claim_n(&erin, 3).await?;
    assert_eq!(
        erin.get_balance().await?.available_sats,
        75_000,
        "erin claimed all three width pieces"
    );

    // The parent is publicly terminal after the ONE split that carved all pieces.
    let (_, _, dave_terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &dave_parent).await?;
    assert!(dave_terminal, "width parent must be terminal after the single fan-out split");

    // Every piece: claimable (CONFIRMED at erin), 1-tx branch, same un-broadcast funding txid.
    let erin_rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk26_erin").await?;
    let mut split_txids = std::collections::HashSet::new();
    for r in &results {
        let pid = &r.coins[0].statechain_id;
        let c = erin_rec
            .coins
            .iter()
            .rev()
            .find(|c| c.statechain_id.as_deref() == Some(pid.as_str()) && c.status == CoinStatus::CONFIRMED)
            .ok_or_else(|| anyhow!("erin has no confirmed coin {pid}"))?;
        split_txids.insert(c.utxo_txid.clone().unwrap_or_default());
        let rows = mercuryrustlib::sqlite_manager::get_backup_txs(
            &cc.pool,
            "sdk26_erin",
            &format!("branch-{pid}"),
        )
        .await?;
        assert_eq!(rows.len(), 1, "width piece {pid} has a 1-tx exit branch");
        let est = erin.estimate_exit_cost(pid).await?;
        assert_eq!(est.branch_txs, 1, "width piece exit = 1 branch tx + backup");
    }
    assert_eq!(split_txids.len(), 1, "ONE split tx funds all three width pieces");
    let split_txid = split_txids.into_iter().next().unwrap();

    // Change stayed with dave, on the SAME split tx: 100k - 75k - 1k reserve = 24k.
    let dave_rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk26_dave").await?;
    let change = dave_rec
        .coins
        .iter()
        .rev()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(24_000))
        .ok_or_else(|| anyhow!("dave has no 24k change sub-coin"))?;
    assert_eq!(
        change.utxo_txid.as_deref(),
        Some(split_txid.as_str()),
        "dave's change is an output of the same split tx"
    );

    // That single branch tx has exactly 4 outputs: 3 pieces + change.
    let first_pid = &results[0].coins[0].statechain_id;
    let rows = mercuryrustlib::sqlite_manager::get_backup_txs(
        &cc.pool,
        "sdk26_erin",
        &format!("branch-{first_pid}"),
    )
    .await?;
    let width_split_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&rows[0].tx)?)?;
    assert_eq!(width_split_tx.output.len(), 4, "width split = 3 pieces + change in one tx");
    println!(
        "ECON width=3 split_txs=1 split_outputs=4 split_vb={} split_txid={split_txid}",
        width_split_tx.vsize()
    );
    println!("SDK26 - width fan-out complete: one co-signed split minted 3 claimable pieces + change");

    // ===== (d) EXIT AT DEPTH: unilaterally exit the DEEPEST leaf ================================
    let leaf_rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk26_alice").await?;
    let leaf_coin = leaf_rec
        .coins
        .iter()
        .rev()
        .find(|c| c.statechain_id.as_deref() == Some(leaf_id.as_str()))
        .ok_or_else(|| anyhow!("no leaf coin record"))?
        .clone();
    let est = alice.estimate_exit_cost(&leaf_id).await?;
    println!(
        "ECON exit_depth=4 branch_txs={} branch_vb={} backup_vb={} total_vb={} wait_blocks={} fee@1={} fee@2={} fee@10={} fee@30={} fee@100={}",
        est.branch_txs,
        est.branch_vbytes,
        est.backup_vbytes,
        est.total_vbytes,
        est.wait_blocks,
        est.fee_sats_at(1.0),
        est.fee_sats_at(2.0),
        est.fee_sats_at(10.0),
        est.fee_sats_at(30.0),
        est.fee_sats_at(100.0)
    );

    // First call: the whole 4-tx branch goes out NOW (locktime-free); the backup waits ~initlock.
    let st = alice.unilateral_exit(Some(vec![leaf_id.clone()]), None).await?;
    assert!(!st[0].complete, "first exit call must not complete (leaf backup is locktimed)");
    assert!(st[0].wait_blocks > 0, "fresh leaf ladder must report a positive wait");
    // The tight one-interval band already ran per-depth in part (a). Here a LOOSER 100-block
    // allowance is needed: part (c) mined blocks after the leaf split (dave's deposit
    // confirmations + token-payment confirmations), each of which legitimately shrinks the
    // remaining wait against the leaf's fixed locktime.
    assert!(
        st[0].wait_blocks + 100 > initlock,
        "fresh leaf ladder waits ~initlock (got {} vs initlock {initlock})",
        st[0].wait_blocks
    );
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        is_outpoint_spent(&cc, &o_txid, o_vout)?,
        "the branch root must have spent the deposit outpoint O"
    );
    let leaf_funding = leaf_coin.utxo_txid.clone().ok_or_else(|| anyhow!("leaf has no funding txid"))?;
    assert!(
        cc.electrum_client
            .transaction_get(&electrum_client::bitcoin::Txid::from_str(&leaf_funding)?)
            .is_ok(),
        "the deepest split tx (leaf funding) must be relayed — the WHOLE branch was broadcast"
    );
    println!(
        "SDK26 - first exit call broadcast the whole 4-tx branch (O spent, depth-4 funding relayed); backup waiting {} blocks",
        st[0].wait_blocks
    );

    // Mine out the leaf ladder to its exact locktime, then the second call completes.
    let leaf_backups =
        mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk26_alice", &leaf_id).await?;
    let latest_backup = leaf_backups
        .iter()
        .max_by_key(|b| b.tx_n)
        .ok_or_else(|| anyhow!("no leaf backup"))?;
    let l_leaf = mercurylib::utils::get_blockheight(latest_backup)?;
    let mut now = tip(&cc)?;
    while now < l_leaf {
        let chunk = (l_leaf - now).min(500);
        bitcoin_core::generatetoaddress(chunk, &core)?;
        now = wait_tip_at_least(&cc, now + chunk).await?;
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut complete = false;
    for _ in 0..10 {
        let st2 = alice.unilateral_exit(Some(vec![leaf_id.clone()]), None).await?;
        if st2[0].complete {
            complete = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(complete, "second exit call must complete once the leaf locktime is mined out");
    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The funds landed at the wallet's OWN backup address (P2TR of coin.user_pubkey).
    let backup_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&latest_backup.tx)?)?;
    let backup_txid = backup_tx.txid();
    let out0 = &backup_tx.output[0];
    let expected_addr =
        mercurylib::transaction::get_user_backup_address(&leaf_coin, "regtest".to_string())?;
    let expected_spk = electrum_client::bitcoin::Address::from_str(&expected_addr)?
        .require_network(electrum_client::bitcoin::Network::Regtest)?
        .script_pubkey();
    assert_eq!(
        out0.script_pubkey, expected_spk,
        "the leaf backup pays the wallet's own backup address"
    );
    let unspent = cc.electrum_client.script_list_unspent(&out0.script_pubkey)?;
    assert!(
        unspent
            .iter()
            .any(|u| u.tx_hash == backup_txid && u.value == out0.value),
        "the exited value must sit unspent at the wallet's backup address"
    );
    println!(
        "SDK26 - depth-4 exit landed {} sats at alice's backup address {expected_addr} (backup tx {backup_txid})",
        out0.value
    );

    println!(
        "SDK26 - SUCCESS: invalidation holds at scale — 4-deep split chain (every consumed parent PUBLICLY terminal at the SE; branch grows ~155 vB per level: deltas {deltas:?}), a 3-wide fan-out from ONE co-signed split, and a full unilateral exit of the deepest leaf (whole branch instantly broadcastable, backup after its own fresh ~initlock ladder, funds at the owner's backup address). Exit cost scales LINEARLY in depth and is known in advance."
    );
    Ok(())
}
