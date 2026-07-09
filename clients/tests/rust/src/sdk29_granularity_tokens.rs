//! E2E (granularity, RGB tokens): partial token amounts down to 1 RAW UNIT, token exit at depth,
//! the spent-carrier → plain-BTC change transition, and the ONE-CARRIER-PER-TRANSFER limitation.
//!
//! Token amounts are RAW u64 units; `precision` is contract metadata the SDK never scales
//! (SPEC REQ-21/22, INV-13). Every token send is a COLORED off-chain split: the piece carries
//! exactly TOKEN_PIECE_SATS = 1_500 sats (packaging) + the exact token amount (payload).
//!
//! DOUBLE-RECEIVE: bob receives PT2 three times (10, 1, then the full remaining 9_985) and his
//! balance SUMS each time (10 → 11 → 9_996). The RGB accept path is idempotent on an already-known
//! asset (the genesis is imported only on first sight); a wallet receiving the same asset twice
//! books both allocations. (This was a bug — every receive re-imported the genesis and hit a
//! UNIQUE constraint, stranding the second allocation — now fixed in the rgb-lib fork.)
//!
//! (a) PRECISION + TINY: alice issues PT2 (precision 2, supply 10_000 raw = "100.00") and sends
//!     bob 10 raw units ("0.10"), then 1 raw unit ("0.01" — the minimum representable amount).
//!     bob books EXACTLY the consignment-derived amounts (10, then a summed 11); alice's change
//!     follows raw-unit conservation (9_990 → 9_985 across all part-(a)/(b) sends). The colored
//!     split tx vsize is measured from the branch row (plain split reference: 155 vB; the colored tx adds
//!     one OP_RETURN opret output).
//! (b) TOKEN EXIT AT DEPTH 2: carol receives 4 raw units on a DEPTH-2 sub-coin (a colored
//!     re-split of alice's depth-1 change carrier; bob's received 1_500-sat piece CANNOT re-split
//!     — asserted as the typed GRN-ERR-9 refusal: at 1_500 sats the FIT guard (piece + reserve
//!     >= carrier, i.e. any carrier <= 1_800) fires, far below the ~2_130-sat effective minimum;
//!     the exact 2129/2130 boundary is pinned by unit granularity_model::token_split_bounds_model,
//!     not here). carol's exit:
//!       - the SDK's plain unilateral exit REFUSES the carrier (audit [7]) — asserted;
//!       - token exit = broadcast the 2-tx colored branch (the split txs ARE the RGB witnesses;
//!         their opret anchors confirm with them); after mining, the deposit outpoint is spent,
//!         both colored txs are on-chain (each with exactly ONE OP_RETURN, INV-11) and carol's
//!         1_500-sat piece outpoint is a live UNSPENT on-chain UTXO;
//!       - RGB settlement: carol's rgb-lib shows exactly 4 raw units SETTLED on the exited
//!         outpoint (engine reopened read-only after dropping the SDK handle), and an INDEPENDENT
//!         observer wallet validates the consignment with the parent witness resolved FROM THE
//!         INDEXER (i.e. genuinely on-chain) — `validate_offchain` treats only the terminal txid
//!         as off-chain.
//!     The pre-signed leaf BACKUP (the 1_500-sat sweep) is asserted + measured but NOT broadcast:
//!     it is a plain (uncolored) spend of the piece outpoint, so broadcasting it would close the
//!     RGB seal without a transition and destroy the allocation — the same hazard the SDK's
//!     carrier guard exists for. The allocation is the payload; the 1_500 sats stay parked on the
//!     exited 2-of-2 outpoint. (INV-16's "backup lands" half applies to PLAIN coins; a
//!     token-preserving sats sweep needs a COLORED exit, which is not shipped.)
//! (c) SPENT-CARRIER CHANGE: alice sends her ENTIRE remaining PT2 allocation (token_change == 0)
//!     — her change sub-coin comes out PLAIN: its sats appear in available_sats (carrier sats are
//!     excluded, review H2/audit [23]) and a plain `split_coin` on it SUCCEEDS, where the same
//!     call was REFUSED (typed carrier error) while the coin still carried the allocation.
//! (d) CROSS-CARRIER COMBINE: alice ends up holding the SAME asset on TWO carriers (IFA issue
//!     60 + on-chain mint 50 bound to a second coin — the cleanest in-SDK construction). Paying
//!     100 — larger than any single carrier — now SUCCEEDS: `transfer_tokens` falls through to a
//!     TRANSPARENT colored combine (both carriers → one piece + change) so bob receives 100 and
//!     alice keeps a 10-unit change. (Combine is now a shipped SDK operation; dedicated coverage in
//!     sdk31. This section used to assert a one-carrier refusal, from before combine was wired in.)
//!
//! Run: SDK_E2E=29 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)
//! Cross-refs: SPEC.md REQ-21/22, INV-11/13/16/26; sdk02/sdk09 (token flows), sdk26 (economics),
//! rgb03/rgb06 (chained off-chain validation at lib level).

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

const RGB_ELECTRUM: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";

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
    Ok(w.get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset)
        .map(|t| t.balance)
        .unwrap_or(0))
}

/// Poll claim until the SETTLED balance of `asset` is exactly `want`.
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

/// Wait until (i) the settled balance of `asset` is >= `want_units` and (ii) the wallet holds
/// `want_carriers` CONFIRMED 10_000-sat statechain coins (the colored funding deposits). Waiting
/// on `available_sats` would deadlock: carrier sats are EXCLUDED from the BTC balance (H2/[23]).
async fn wait_carriers_confirmed(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    core: &str,
    asset: &str,
    want_units: u64,
    want_carriers: usize,
) -> Result<()> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        let units_ok = token_balance(w, asset).await? >= want_units;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
        let carriers = rec
            .coins
            .iter()
            .filter(|c| {
                c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(10_000)
            })
            .count();
        if units_ok && carriers >= want_carriers {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{wallet_name}: {want_units} of {asset} on {want_carriers} carrier(s) did not confirm"))
}

/// The wallet's most recent CONFIRMED coin of exactly `sats`.
async fn confirmed_coin(
    cc: &ClientConfig,
    wallet_name: &str,
    sats: u32,
) -> Result<mercuryrustlib::Coin> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    rec.coins
        .iter()
        .rev()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(sats) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("{wallet_name} has no CONFIRMED coin of {sats} sats"))
}

fn parse_tx(hex_tx: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?)
}

/// Whether `txid:vout` is spent according to electrs (errors propagate — sdk26 discipline).
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

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in [
        "./rgb-data-sdk29_alice",
        "./rgb-data-sdk29_bob",
        "./rgb-data-sdk29_carol",
        "./rgb-data-sdk29_observer",
    ] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk29_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk29_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk29_carol"), None).await?;
    // bob deliberately receives PT2 MULTIPLE times (10, then 1, then the full remaining 9_985) to
    // prove the double-receive fix: the RGB accept path is now idempotent on an already-known asset
    // (was: re-importing the genesis on every receive hit a UNIQUE constraint and stranded the
    // second allocation). dave first-sees a DIFFERENT asset (QTK) in part (d).
    let (dave, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk29_dave"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;
    let dave_addr = dave.get_utexo_address().await?;

    // Fund alice's RGB engine (issuance + IFA + mint witness txs are the ISSUER's on-chain cost).
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // ===== (a) PRECISION + TINY: issue PT2 (precision 2), send 0.10 then 0.01 ===================
    add_tokens(&cc, &alice, 1).await?;
    let asset_p2 = alice.issue_token("PT2", "Precision Two", 2, 10_000).await?;
    wait_carriers_confirmed(&cc, &alice, "sdk29_alice", &core, &asset_p2, 10_000, 1).await?;
    let alice_tok = alice
        .get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset_p2)
        .ok_or_else(|| anyhow!("issued asset not in alice's balances"))?;
    assert_eq!(alice_tok.precision, 2, "precision is contract metadata");
    assert_eq!(alice_tok.balance, 10_000, "supply is RAW units (10_000 raw = \"100.00\")");
    println!("SDK29 - issued {asset_p2}: 10_000 raw units at precision 2 (display \"100.00\")");

    // Send 10 raw units ("0.10"): a colored split whose piece carries EXACTLY 10.
    add_tokens(&cc, &alice, 2).await?;
    let r1 = alice.transfer_tokens(&asset_p2, &bob_addr, 10).await?;
    // (used_split is hard-coded `true` for token transfers, so asserting it would be vacuous —
    // the result contract is exercised via coins.len()/amount_sats/total_sats instead.)
    assert_eq!(r1.coins.len(), 1, "a token transfer hands over exactly ONE piece");
    assert_eq!(r1.coins[0].amount_sats, 1_500, "the piece coin carries TOKEN_PIECE_SATS");
    assert_eq!(r1.total_sats, 1_500, "a token piece always carries TOKEN_PIECE_SATS = 1_500");
    let bob_piece1 = r1.coins[0].statechain_id.clone();
    wait_token_balance(&bob, &asset_p2, 10).await?;
    assert_eq!(token_balance(&bob, &asset_p2).await?, 10, "bob booked EXACTLY 10 raw units");
    assert_eq!(token_balance(&alice, &asset_p2).await?, 9_990, "alice's change: 9_990 raw units");
    let bob_tok = bob
        .get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset_p2)
        .ok_or_else(|| anyhow!("received asset not in bob's balances"))?;
    assert_eq!(bob_tok.precision, 2, "precision metadata travels with the consignment");
    // The carrier's 1_500 sats are TOKEN packaging, not spendable BTC (H2/[23] exclusion).
    assert_eq!(bob.get_balance().await?.available_sats, 0, "carrier sats are not spendable BTC");

    // Measure the COLORED split tx (branch row of bob's piece): 1 input, piece + change + exactly
    // one OP_RETURN opret output (INV-11) — the coloring premium over the plain 155 vB split.
    let bob_branch = mercuryrustlib::sqlite_manager::get_backup_txs(
        &cc.pool,
        "sdk29_bob",
        &format!("branch-{bob_piece1}"),
    )
    .await?;
    assert_eq!(bob_branch.len(), 1, "depth-1 token piece has a 1-tx branch");
    let colored_tx = parse_tx(&bob_branch[0].tx)?;
    assert_eq!(
        colored_tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(),
        1,
        "exactly ONE opret commitment output (INV-11)"
    );
    let colored_vb = colored_tx.vsize() as u64;
    assert!(
        (150..=320).contains(&colored_vb),
        "colored split tx out of band: {colored_vb} vB (plain split is ~155 vB + opret output)"
    );
    println!("ECON token_split piece_sats=1500 colored_split_vb={colored_vb} plain_split_vb_ref=155");

    // ===== (b) part 1 — build carol's DEPTH-2 token sub-coin ====================================
    // bob's received piece carries only 1_500 sats: a further colored split needs
    // 1_500 (piece) + fee reserve + non-dust change ≈ 2_130 sats, so a received piece can NEVER
    // re-split. What fires HERE is the FIT guard (GRN-ERR-9, tokens.rs:485-488: piece + reserve
    // >= carrier ⇒ any carrier <= 1_800 refuses) — 1_500 is far below both bounds; the change
    // dust floor (the +330 of the 2_130) is enforced downstream at PSBT build and never reached,
    // and the exact 2129/2130 boundary is pinned by unit granularity_model::
    // token_split_bounds_model. The typed refusal documents the limitation; depth therefore
    // grows on the CHANGE side.
    let err = bob
        .transfer_tokens(&asset_p2, &carol_addr, 4)
        .await
        .expect_err("a 1_500-sat received piece must refuse a further colored split");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("carrier coin too small"),
        "expected the min-carrier refusal, got: {msg}"
    );
    println!("SDK29 - LIMITATION (fit guard fired at 1_500 sats; effective min carrier ~2_130): {msg}");

    // Depth 2 via a colored RE-SPLIT of a token SUB-COIN: alice's change carrier (8_200 sats,
    // 9_990 raw, depth 1) pays carol 4 raw units → carol's piece inherits the branch (2 txs).
    add_tokens(&cc, &alice, 2).await?;
    let r2 = alice.transfer_tokens(&asset_p2, &carol_addr, 4).await?;
    assert_eq!(r2.coins.len(), 1, "one piece per token transfer");
    let carol_piece = r2.coins[0].statechain_id.clone();
    wait_token_balance(&carol, &asset_p2, 4).await?;
    assert_eq!(token_balance(&alice, &asset_p2).await?, 9_986);
    let est = carol.estimate_exit_cost(&carol_piece).await?;
    assert_eq!(est.branch_txs, 2, "carol's token piece is a DEPTH-2 sub-coin");
    println!("SDK29 - carol received 4 raw units on a depth-2 coin (2-tx colored branch)");

    // ===== (a) continued — the 1-raw-unit minimum ("0.01"), and DOUBLE-RECEIVE ==================
    // Second PT2 receive to bob (who already holds 10): the accept path is idempotent, so bob's
    // balance SUMS (10 → 11) instead of stranding the second allocation.
    add_tokens(&cc, &alice, 2).await?;
    let r3 = alice.transfer_tokens(&asset_p2, &bob_addr, 1).await?;
    assert_eq!(r3.coins.len(), 1, "a 1-raw-unit send still mints one full 1_500-sat piece");
    wait_token_balance(&bob, &asset_p2, 11).await?;
    assert_eq!(token_balance(&bob, &asset_p2).await?, 11, "second receive SUMS: 10 + 1 = 11");
    assert_eq!(token_balance(&alice, &asset_p2).await?, 9_985);
    println!("SDK29 - TINY + DOUBLE-RECEIVE: 1 raw unit booked exactly; bob's 2nd PT2 receipt summed to 11");

    // ===== (c) SPENT-CARRIER CHANGE: full-allocation send frees the change as PLAIN BTC =========
    // alice's current carrier: change of the 1-raw-unit split — 4_600 sats, 9_985 raw, depth 3.
    let carrier3 = confirmed_coin(&cc, "sdk29_alice", 4_600).await?;
    let carrier3_id = carrier3.statechain_id.clone().unwrap_or_default();
    // While it CARRIES, a plain split is refused (typed, review H2/audit [7] family)...
    let err = alice
        .split_coin(&carrier3_id, 330)
        .await
        .expect_err("plain split of a token carrier must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("carries an RGB token allocation"),
        "expected the carrier-split refusal, got: {msg}"
    );
    // ...and its sats are invisible to the BTC balance (alice holds NO plain coins yet).
    assert_eq!(
        alice.get_balance().await?.available_sats,
        0,
        "all of alice's sats ride the carrier — none spendable as BTC"
    );

    // Send the ENTIRE remaining allocation to bob (his THIRD PT2 receipt — proves repeated
    // double-receive): token_change == 0 → alice's change sub-coin is PLAIN; bob sums to 9_996.
    add_tokens(&cc, &alice, 2).await?;
    let r4 = alice.transfer_tokens(&asset_p2, &bob_addr, 9_985).await?;
    assert_eq!(r4.coins.len(), 1, "a full-allocation send is still one piece (+ plain change)");
    wait_token_balance(&bob, &asset_p2, 9_996).await?;
    assert_eq!(token_balance(&bob, &asset_p2).await?, 9_996, "third receive SUMS: 11 + 9_985");
    assert_eq!(token_balance(&alice, &asset_p2).await?, 0, "alice's PT2 allocation fully spent");

    // The change (4_600 − 1_500 − 300 = 2_800 sats) is now an ordinary BTC sub-coin: it shows up
    // in available_sats (i.e. token_carrier_outpoints no longer contains its outpoint — the same
    // predicate the balance/selection guards read) and a plain split_coin on it SUCCEEDS.
    let change4 = confirmed_coin(&cc, "sdk29_alice", 2_800).await?;
    let change4_id = change4.statechain_id.clone().unwrap_or_default();
    assert_eq!(
        alice.get_balance().await?.available_sats,
        2_800,
        "the spent-carrier's change surfaces as spendable BTC"
    );
    // Mint a piece at the TRUE floor (330 dust + backup fee; 442 at 1 sat/vB, GRN-INV-1b) — a
    // 330-sat piece would strand this coin at backup creation (same guard gap as sdk28).
    let info = mercuryrustlib::utils::info_config(&cc).await?;
    let fee_rate = info.fee_rate_sats_per_byte.min(cc.max_fee_rate);
    let min_piece = 330 + (112.0f64 * fee_rate).ceil() as u64; // ≈ 442 at 1 sat/vB
    add_tokens(&cc, &alice, 2).await?;
    let (piece_id, change_id) = alice.split_coin(&change4_id, min_piece).await?;
    confirmed_coin(&cc, "sdk29_alice", min_piece as u32).await?;
    let change_after = 2_800 - min_piece - 300;
    confirmed_coin(&cc, "sdk29_alice", change_after as u32).await?;
    println!(
        "ECON spent_carrier change_sats=2800 plain_split piece={min_piece} change={change_after} reserve=300 (ids {piece_id}/{change_id})"
    );
    println!("SDK29 - SPENT-CARRIER CHANGE: full-allocation send left a PLAIN 2_800-sat change (splittable, in available_sats) — was refused while it carried");

    // ===== (b) part 2 — carol's TOKEN EXIT at depth 2 ===========================================
    // The SDK's PLAIN unilateral exit refuses the carrier — explicitly (typed) and in the sweep.
    let err = carol
        .unilateral_exit(Some(vec![carol_piece.clone()]), None)
        .await
        .expect_err("plain unilateral exit of a token carrier must be refused (audit [7])");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("a plain unilateral exit would destroy the tokens"),
        "expected the carrier exit refusal, got: {msg}"
    );
    let sweep = carol.unilateral_exit(None, None).await?;
    assert!(sweep.is_empty(), "the exit-everything sweep must skip the carrier");

    // Exit material: 2-tx colored branch + pre-signed backup + consignment (in the backup row).
    let carol_rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk29_carol").await?;
    let carol_coin = carol_rec
        .coins
        .iter()
        .rev()
        .find(|c| c.statechain_id.as_deref() == Some(carol_piece.as_str()) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("carol has no coin record for the piece"))?;
    let leaf_txid = carol_coin.utxo_txid.clone().ok_or_else(|| anyhow!("no leaf txid"))?;
    let leaf_vout = carol_coin.utxo_vout.ok_or_else(|| anyhow!("no leaf vout"))?;
    assert_eq!(carol_coin.amount, Some(1_500));

    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(
        &cc.pool,
        "sdk29_carol",
        &format!("branch-{carol_piece}"),
    )
    .await?;
    assert_eq!(branch.len(), 2, "depth-2 branch: two colored split txs");
    let root_tx = parse_tx(&branch[0].tx)?;
    let leaf_tx = parse_tx(&branch[1].tx)?;
    assert_eq!(leaf_tx.txid().to_string(), leaf_txid, "the leaf split funds carol's piece");
    let mut level_vbs = Vec::new();
    for (i, tx) in [&root_tx, &leaf_tx].iter().enumerate() {
        assert_eq!(
            tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(),
            1,
            "branch tx {} must carry exactly one opret anchor (INV-11)",
            i + 1
        );
        level_vbs.push(tx.vsize() as u64);
    }
    let deposit_outpoint = root_tx.input[0].previous_output;
    // Consignment rides the recovery material (BackupTx.rgb_consignment) — NOT seed-derivable.
    let carol_backups =
        mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk29_carol", &carol_piece).await?;
    let envelope_json = carol_backups
        .iter()
        .find_map(|b| b.rgb_consignment.clone())
        .ok_or_else(|| anyhow!("carol's exit material lacks the consignment envelope"))?;
    let envelope: serde_json::Value = serde_json::from_str(&envelope_json)?;
    assert_eq!(envelope["a"].as_u64(), Some(4), "envelope amount hint = 4 raw units");
    assert_eq!(envelope["s"].as_u64(), Some(1_500), "envelope sats = TOKEN_PIECE_SATS");
    let consignment_b64 = envelope["c"]
        .as_str()
        .ok_or_else(|| anyhow!("envelope has no consignment"))?
        .to_string();

    println!(
        "ECON token_exit depth=2 branch_txs={} branch_vb={} colored_tx_vbs={:?} backup_vb={} total_vb={} wait_blocks={} piece_sats=1500 fee@1={} fee@10={}",
        est.branch_txs,
        est.branch_vbytes,
        level_vbs,
        est.backup_vbytes,
        est.total_vbytes,
        est.wait_blocks,
        est.fee_sats_at(1.0),
        est.fee_sats_at(10.0)
    );

    // TOKEN EXIT = broadcast the branch. The colored split txs are the RGB witnesses; confirming
    // them anchors the transitions on-chain. Root-first (the root spends the on-chain deposit).
    for row in &branch {
        let raw = hex::decode(&row.tx)?;
        cc.electrum_client.transaction_broadcast_raw(&raw)?;
    }
    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        is_outpoint_spent(&cc, &deposit_outpoint.txid.to_string(), deposit_outpoint.vout)?,
        "the branch root must have spent the on-chain deposit outpoint"
    );
    // carol's piece outpoint is now a LIVE on-chain UTXO holding the 1_500 packaging sats.
    let mut leaf_unspent = false;
    for _ in 0..30 {
        let spk = &leaf_tx.output[leaf_vout as usize].script_pubkey;
        let listed = cc.electrum_client.script_list_unspent(spk)?;
        if listed
            .iter()
            .any(|u| u.tx_hash.to_string() == leaf_txid && u.tx_pos as u32 == leaf_vout && u.value == 1_500)
        {
            leaf_unspent = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(leaf_unspent, "carol's exited piece outpoint must be an unspent on-chain UTXO");
    println!("SDK29 - branch broadcast: both colored witnesses confirmed; piece outpoint {leaf_txid}:{leaf_vout} live on-chain");

    // INDEPENDENT on-chain settlement check: a fresh observer validates the consignment with the
    // PARENT witness resolved from the INDEXER (`validate_offchain` treats only the terminal txid
    // as off-chain) — this can only pass because the depth-1 witness is now genuinely on-chain.
    let (obs_valid, obs_detail) = tokio::task::block_in_place(|| -> Result<(bool, Option<String>)> {
        std::fs::create_dir_all("./rgb-data-sdk29_observer")?;
        let mnemonic = RgbWallet::generate_mnemonic("regtest")?;
        let observer =
            RgbWallet::open("./rgb-data-sdk29_observer", &mnemonic, "regtest", RGB_ELECTRUM, RGB_PROXY)?;
        observer.validate_offchain(&consignment_b64, &leaf_txid)
    })?;
    assert!(
        obs_valid,
        "observer validation against the on-chain parent witness failed: {obs_detail:?}"
    );

    // carol's own rgb-lib view: exactly 4 raw units SETTLED on the exited outpoint. Reopen her
    // engine directly (drop the SDK handle first — same data dir, persisted mnemonic).
    assert_eq!(token_balance(&carol, &asset_p2).await?, 4);
    drop(carol);
    let leaf_outpoint = format!("{leaf_txid}:{leaf_vout}");
    let carol_allocs = tokio::task::block_in_place(|| -> Result<Vec<(String, u64, bool)>> {
        let mnemonic = std::fs::read_to_string("./rgb-data-sdk29_carol/rgb.mnemonic")?
            .trim()
            .to_string();
        let mut w =
            RgbWallet::open("./rgb-data-sdk29_carol", &mnemonic, "regtest", RGB_ELECTRUM, RGB_PROXY)?;
        let _ = w.refresh(None); // observe the now-confirmed witnesses (no-op for registered rows)
        w.list_allocations(&asset_p2)
    })?;
    assert!(
        carol_allocs
            .iter()
            .any(|(op, amt, settled)| *op == leaf_outpoint && *amt == 4 && *settled),
        "carol's rgb-lib must show 4 raw units SETTLED on the exited outpoint {leaf_outpoint} (got {carol_allocs:?})"
    );
    println!("SDK29 - RGB settlement ON-CHAIN: 4 raw units settled on {leaf_outpoint} (rgb-lib view + independent indexer-resolved validation)");

    // The pre-signed BACKUP exists (the 1_500-sat sweep after ~initlock) but is NOT broadcast:
    // it is an UNCOLORED spend of the seal outpoint — landing it would destroy the allocation
    // (the exact hazard behind the SDK's carrier guard). The token-preserving exit ends here;
    // the packaging sats stay parked on the exited 2-of-2 outpoint until a colored exit ships.
    let latest_backup = carol_backups
        .iter()
        .max_by_key(|b| b.tx_n)
        .ok_or_else(|| anyhow!("no pre-signed backup for carol's piece"))?;
    let backup_tx = parse_tx(&latest_backup.tx)?;
    let backup_locktime = mercurylib::utils::get_blockheight(latest_backup)?;
    println!(
        "SDK29 - leaf backup pre-signed ({} vB, locktime {backup_locktime}, sweeps the 1_500-sat packaging) — NOT broadcast: an uncolored sweep of the seal outpoint would destroy the 4-unit allocation",
        backup_tx.vsize()
    );

    // ===== (d) ONE CARRIER PER TRANSFER =========================================================
    // Cleanest two-carriers-same-asset construction: IFA issue 60 (carrier A) + on-chain mint of
    // the 50 inflation-right bound to a SECOND coin (carrier B). (A received piece cannot be
    // re-carried onto the issuer's coin — see the min-carrier refusal above — so bob "sending 50
    // back" is not constructible; mint gives the same two-carrier end state in-SDK.)
    add_tokens(&cc, &alice, 1).await?;
    let asset_q = alice.issue_inflatable_token("QTK", "Q Token", 0, 60, vec![50]).await?;
    wait_carriers_confirmed(&cc, &alice, "sdk29_alice", &core, &asset_q, 60, 1).await?;
    println!("SDK29 - issued IFA {asset_q}: 60 units on carrier A (+50 inflation-right)");

    // Mint needs blocks while it polls (on-chain inflate): run a scoped background miner.
    let mining = Arc::new(AtomicBool::new(true));
    let miner = {
        let m = mining.clone();
        let core = core.clone();
        std::thread::spawn(move || {
            while m.load(Ordering::Relaxed) {
                let _ = bitcoin_core::generatetoaddress(1, &core);
                std::thread::sleep(Duration::from_secs(2));
            }
        })
    };
    add_tokens(&cc, &alice, 1).await?;
    let mint_res = alice.mint_tokens(&asset_q, vec![50]).await;
    mining.store(false, Ordering::Relaxed);
    let _ = miner.join();
    let (mint_txid, minted) = mint_res?;
    assert_eq!(minted, 50);
    wait_carriers_confirmed(&cc, &alice, "sdk29_alice", &core, &asset_q, 110, 2).await?;
    println!("SDK29 - minted +50 (inflate {mint_txid}): alice holds 110 QTK on TWO carriers (60 + 50)");

    // 110 available across TWO carriers (60 + 50), no SINGLE carrier holds 100. `transfer_tokens`
    // now spans carriers TRANSPARENTLY: when no single carrier covers the amount, colored_transfer
    // falls through to a colored COMBINE (both carriers → one piece + change) in one SE-co-signed
    // tx (dedicated coverage in sdk31). So 100 in one shot SUCCEEDS: bob receives 100, alice keeps a
    // 10-unit change. (This assertion was updated when combine was wired into the SDK — it used to
    // expect a one-carrier refusal, from before combine shipped.)
    add_tokens(&cc, &alice, 3).await?;
    let r5 = alice.transfer_tokens(&asset_q, &bob_addr, 100).await?;
    assert_eq!(r5.coins.len(), 1, "the combine hands the receiver a single piece coin");
    wait_token_balance(&bob, &asset_q, 100).await?;
    assert_eq!(token_balance(&bob, &asset_q).await?, 100, "bob receives the full 100 QTK via combine");
    assert_eq!(token_balance(&alice, &asset_q).await?, 10, "alice keeps the 10 QTK combine change");
    println!("SDK29 - CROSS-CARRIER COMBINE: transfer_tokens(100) spanned both carriers transparently (bob 100, alice 10 change)");

    println!(
        "SDK29 - SUCCESS: token granularity is exact to 1 RAW unit (precision is metadata only; 0.10 and 0.01 booked exactly), a wallet can receive the SAME asset repeatedly and its balance SUMS (bob 10 → 11 → 9_996 — double-receive fixed), a depth-2 token piece exits by broadcasting its colored branch (witnesses + opret anchors confirm; 4 units settled on-chain on the exited outpoint, independently validated against the indexer; the uncolored backup is deliberately NOT landed — it would destroy the allocation), a fully-spent carrier's change becomes ordinary splittable BTC, and a payment larger than any single carrier is served by a TRANSPARENT colored combine (100 across a 60 + 50 pair; combine now shipped in the SDK — sdk31)."
    );
    Ok(())
}
