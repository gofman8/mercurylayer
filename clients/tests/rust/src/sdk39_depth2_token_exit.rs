//! E2E (SDK_E2E=39, depth-2 token exit, CTES-R): a token piece that is TWO coloured splits deep can
//! still be exited (walked) on-chain end-to-end, preserving the RGB allocation. Closes the
//! granularity-deep-dive §5.6 test gap ("no E2E yet covers a depth >= 2 token exit").
//!
//! **RE-DERIVED ONTO THE COLOURED LANE.** The original drove the legacy flat coloured split
//! (`create_colored_split_tx` → `register_split_subcoins_n`), where each sub-coin got a flat
//! absolute-locktime backup as its exit material and an un-broadcast `branch-<id>` chain to reach
//! the chain; depth 2 was `branch == [split1, split2]` and the exit was "broadcast the branch
//! root-first". That lane is RETIRED: `register_split_subcoins_n` refuses by name ("the off-chain
//! branch split is retired"), because its sub-coins were exited by flat backups, which no longer
//! exist — a coin's only exit is its TES-R ladder, established at first sight of an on-chain `F`,
//! and a piece of a payment is carved by the in-ladder split.
//!
//! How depth 2 arises now: alice issues on a COLOURED carrier (depth 0); her first
//! `transfer_tokens` is an in-ladder split `T -> X_m -> SP_1` paying bob a coloured CHILD (depth 1,
//! `ancestors` empty) and leaving alice her change as a SPINE TIP; her second `transfer_tokens` pays
//! carol OUT OF THAT TIP, which mints `SP_2` one level deeper — carol's child descends through ONE
//! intermediate spine segment (`ancestors.len() == 1`) and its exit chain is
//! `T -> X_m -> SP_1 -> SP_2 -> ext_child -> state_child` (six tiers, all RGB-aware). It conveys an
//! EMPTY parent flat chain: there is nothing on a calendar at any depth. Carol then WALKS that chain
//! with `unilateral_exit` — no SE, no counterparty, only blocks — and the 250 units settle on chain:
//! every tier mined with exactly one OP_RETURN, `F` spent by `T`, and the leaf consignment validated
//! against the CHAIN ALONE (`colored_child_exit_proof`) for the full amount. Survival is measured
//! with the read-only stock probe, never with `get_asset_balance` (E7).
//!
//! Run: SDK_E2E=39 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

const SUPPLY: u64 = 1000;
const PAY: u64 = 250;

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
    for _ in 0..90 {
        w.claim().await?;
        if token_balance(w, asset).await? == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("balance of {asset} did not reach {want} (got {})", token_balance(w, asset).await?))
}
/// Every CONFIRMED coin of `wallet_name` whose `tesr-` row is a COLOURED ROOT ladder for `asset`.
async fn colored_carriers(
    cc: &ClientConfig,
    wallet_name: &str,
    asset: &str,
) -> Result<Vec<(String, mercuryrustlib::tesr::TesrBundle)>> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let mut out = Vec::new();
    for c in rec.coins.iter().filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0) {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if let Some(b) = mercuryrustlib::tesr::load(cc, wallet_name, &sid).await? {
            if b.rgb.as_ref().is_some_and(|r| r.contract_id == asset) {
                out.push((sid, b));
            }
        }
    }
    Ok(out)
}
async fn wait_colored_carrier(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    core: &str,
    asset: &str,
) -> Result<(String, mercuryrustlib::tesr::TesrBundle)> {
    for _ in 0..90 {
        w.claim().await?;
        if let Some(found) = colored_carriers(cc, wallet_name, asset).await?.into_iter().next() {
            return Ok(found);
        }
        bitcoin_core::generatetoaddress(1, core)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{wallet_name} never got a COLOURED carrier of {asset} — CTES-R establish did not happen"))
}
/// Every adopted `ctesr-` child of `wallet_name`, with its bundle.
async fn adopted_children(
    cc: &ClientConfig,
    wallet_name: &str,
) -> Result<Vec<(String, mercuryrustlib::tesr::ChildTesrBundle)>> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let mut out = Vec::new();
    for c in rec.coins.iter().filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0) {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if let Some(cb) = mercuryrustlib::tesr::load_child(cc, wallet_name, &sid).await? {
            out.push((sid, cb));
        }
    }
    Ok(out)
}
fn tip(cc: &ClientConfig) -> Result<usize> {
    Ok(cc.electrum_client.block_headers_subscribe()?.height)
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
fn parse_tx(hex_tx: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?)
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

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk39_alice", "./rgb-data-sdk39_bob", "./rgb-data-sdk39_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    // [D30] `colored_ladder` ships false; this is what puts the test on the CTES-R lane. The legacy
    // lane a default wallet would take is retired, so without it there is no depth at all.
    let open = |name: &str| {
        let mut cfg = SdkConfig::regtest(name);
        cfg.colored_ladder = true;
        cfg
    };
    let (alice, _) = UtexoWallet::initialize(open("sdk39_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(open("sdk39_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(open("sdk39_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    add_tokens(&cc, &alice, 8).await?;
    let asset = alice.issue_token("D2", "Depth Two", 0, SUPPLY).await?;
    let (carrier_sid, carrier_bundle) = wait_colored_carrier(&cc, &alice, "sdk39_alice", &core, &asset).await?;
    assert_eq!(
        carrier_bundle.rgb.as_ref().map(|r| r.amount),
        Some(SUPPLY),
        "the coloured ladder carries the WHOLE allocation"
    );
    let f_txid = carrier_bundle.f_txid.clone();
    let f_vout = carrier_bundle.f_vout;
    println!("SDK39 - alice issued {SUPPLY} {asset} on a COLOURED carrier {carrier_sid} (depth 0); F={f_txid}:{f_vout}");

    // ===== DEPTH 1: the root split pays bob a child and leaves alice a spine tip ================
    let r1 = alice.transfer_tokens(&asset, &bob_addr, PAY).await?;
    assert!(r1.used_split, "a partial token payment is an in-ladder split");
    wait_token_balance(&bob, &asset, PAY).await?;
    assert_eq!(token_balance(&alice, &asset).await?, SUPPLY - PAY, "alice keeps {} on her change tip", SUPPLY - PAY);
    let bob_kids = adopted_children(&cc, "sdk39_bob").await?;
    assert_eq!(bob_kids.len(), 1, "bob adopted exactly one coloured child");
    assert!(bob_kids[0].1.is_colored(), "bob's child must be COLOURED");
    assert!(bob_kids[0].1.ancestors.is_empty(), "a root split mints a DEPTH-1 child (no intermediate segment)");
    println!("SDK39 - alice -> bob {PAY} (depth 1: ancestors empty); alice holds {} on a SPINE TIP", SUPPLY - PAY);

    // ===== DEPTH 2: a second split OUT OF THE TIP pays carol one level deeper ===================
    // Alice's only holding of the asset is now her spine tip, so this payment is a spine batch:
    // `SP_2` spends the tip's output, and carol's piece rides beneath BOTH split transactions.
    let r2 = alice.transfer_tokens(&asset, &carol_addr, PAY).await?;
    assert!(r2.used_split, "a partial pay out of a tip carves a piece");
    let carol_piece = r2.coins[0].statechain_id.clone();
    wait_token_balance(&carol, &asset, PAY).await?;
    assert_eq!(token_balance(&alice, &asset).await?, SUPPLY - 2 * PAY, "alice keeps the rest on a fresh tip");
    let carol_cb = mercuryrustlib::tesr::load_child(&cc, "sdk39_carol", &carol_piece)
        .await?
        .ok_or_else(|| anyhow!("carol booked the tokens but adopted NO child bundle — her piece has no exit material"))?;
    assert!(carol_cb.is_colored(), "carol's child must be COLOURED — a plain tier over it destroys the {PAY}");
    assert_eq!(
        carol_cb.ancestors.len(),
        1,
        "carol's piece is DEPTH 2: exactly ONE intermediate spine segment (the batch's own SP_1 tip) \
         between the root ladder and its own rungs — got {} (0 would be a root split; the retired \
         lane's `branch == [split1, split2]` has no counterpart here)",
        carol_cb.ancestors.len()
    );
    assert!(
        carol_cb.parent_flat_backups.is_empty(),
        "a depth-2 leaf conveys an EMPTY parent flat chain — there is no calendar at any depth; got {} flat backup(s)",
        carol_cb.parent_flat_backups.len()
    );
    let chain = mercuryrustlib::tesr::child_exit_chain(&carol_cb);
    assert_eq!(
        chain.len(),
        6,
        "a depth-2 child's exit chain is T, X_m, SP_1, SP_2, ext_child, state_child — one more than \
         a depth-1 child's five (the intermediate spine segment contributes exactly its state)"
    );
    let root = parse_tx(&chain[0].0)?.input[0].previous_output;
    assert_eq!(
        (root.txid.to_string(), root.vout),
        (f_txid.clone(), f_vout),
        "the chain root must spend the carrier's ON-CHAIN funding output F"
    );
    let (hc, ha, health_txids, _) = carol.colored_child_health(&carol_piece).await?;
    assert_eq!(hc, asset, "carol's consignment chain is for THIS contract");
    assert_eq!(ha, PAY, "carol's consignment chain must assign her exactly {PAY} (never the sender's declared field)");
    assert_eq!(health_txids.len(), 6, "the consignment chain resolves against all six witnesses");
    for t in health_txids.iter() {
        assert!(onchain(&cc, t).is_none(), "tier {t} must still be un-broadcast — the whole depth-2 payment is off-chain");
    }
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout)?, "the on-chain root F is unspent before the exit");
    println!("SDK39 - alice -> carol {PAY} OUT OF THE TIP: carol's piece {carol_piece} is DEPTH 2 (1 intermediate segment, 6-tier chain rooted at F, empty parent flat chain), all six tiers off-chain");

    // ===== THE EXIT: carol walks all six tiers, keyless ==========================================
    // BEFORE the walk the probes must already discriminate, or the after-shots prove nothing (E7).
    carol
        .probe_colored_child_tip(&carol_piece, PAY)
        .await
        .map_err(|e| anyhow!("carol's stock is dead BEFORE the walk, so nothing is provable: {e}"))?;
    assert!(
        carol.probe_colored_child_tip(&carol_piece, PAY + 1).await.is_err(),
        "the stock probe accepted MORE than the allocation — it is not discriminating"
    );
    assert!(
        carol.colored_child_exit_proof(&carol_piece).await.is_err(),
        "the leaf consignment validated against the CHAIN ALONE before any tier was broadcast — the \
         empty-offchain-set proof would be vacuous"
    );
    let mut passes = 0;
    loop {
        passes += 1;
        assert!(passes < 30, "the depth-2 coloured child exit did not converge");
        let statuses = carol
            .unilateral_exit(Some(vec![carol_piece.clone()]), None)
            .await
            .map_err(|e| anyhow!("unilateral_exit REFUSED a depth-2 coloured CHILD — the piece is unexitable: {e}"))?;
        if statuses[0].complete {
            break;
        }
        let wait = statuses[0].wait_blocks.max(1);
        bitcoin_core::generatetoaddress(wait, &core)?;
        mine_synced(&cc, &core, 1)?;
    }
    mine_synced(&cc, &core, 3)?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Every tier MINED, every tier carrying exactly one opret anchor (INV-11 on the witnesses).
    for (i, (hex_tx, _)) in chain.iter().enumerate() {
        let tx = parse_tx(hex_tx)?;
        assert!(onchain(&cc, &tx.txid().to_string()).is_some(), "tier {i} ({}) never reached the chain", tx.txid());
        assert_eq!(
            tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(),
            1,
            "every tier of a COLOURED exit chain carries exactly one opret anchor (INV-11) — tier {i}"
        );
    }
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout)?, "the walk must have spent the on-chain root F");

    // THE ALLOCATION SURVIVED — the leaf consignment validates against the CHAIN ALONE (empty
    // off-chain witness set), achievable only if every one of the six tiers is genuinely mined.
    let mut proof = carol.colored_child_exit_proof(&carol_piece).await;
    for _ in 0..20 {
        if proof.is_ok() {
            break;
        }
        let msg = proof.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        if !msg.contains("can't be located in the blockchain") {
            break; // a real verdict, not indexer lag (see mine_synced)
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        proof = carol.colored_child_exit_proof(&carol_piece).await;
    }
    let (proof_contract, proof_amount, detail) = proof.map_err(|e| anyhow!(
        "THE ALLOCATION DID NOT SURVIVE THE DEPTH-2 WALK: the leaf consignment does not validate \
         against the chain alone after every tier was mined — {e}"
    ))?;
    assert_eq!(proof_contract, asset, "the surviving allocation is THIS contract");
    assert_eq!(proof_amount, PAY, "all {PAY} units must survive the depth-2 walk");
    carol
        .probe_colored_child_tip(&carol_piece, PAY)
        .await
        .map_err(|e| anyhow!("the stock is DEAD after the exit walk: {e}"))?;
    assert!(
        carol.probe_colored_child_tip(&carol_piece, PAY + 1).await.is_err(),
        "after the walk the probe accepted MORE than the allocation — it is not reading the stock"
    );
    println!("SDK39 - carol walked her DEPTH-2 chain in {passes} pass(es): all 6 RGB-aware tiers mined, F spent by T, and the leaf consignment validates against the CHAIN ALONE for {PAY} {asset} ({detail:?})");

    println!("SDK39 - SUCCESS: a depth-2 coloured piece exits end-to-end on the CTES-R lane. Two successive in-ladder splits (the second out of the sender's spine tip) build a child that descends through one intermediate segment with a six-tier exit chain and an EMPTY parent flat chain; walking it root-first settles the {PAY}-unit allocation on chain without the SE. The deep coloured-exit path (granularity-deep-dive §5.6) is proven beyond depth 1 — on the lane that exists, not the retired branch lane.");
    Ok(())
}
