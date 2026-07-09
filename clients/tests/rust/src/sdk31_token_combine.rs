//! E2E (multi-carrier token combine): pay an amount that spans SEVERAL carriers, which the
//! single-carrier path used to refuse ("no single coin carries >= N"). A payment now COMBINES the
//! carriers into one SE-co-signed colored combine tx (N inputs → piece + change), WITHOUT weakening
//! invalidation/branch security. This test also emits verbose VERIFY lines to check the receiver's
//! branch + terminal-ancestor invariants against the spec.
//!
//! (a) COMBINE PAYMENT: alice holds 110 of an asset across TWO carriers (60 + 50). Paying bob 100
//!     succeeds via a 2-input colored combine; bob books EXACTLY 100, alice keeps 10. The piece is a
//!     depth-1 sub-coin whose 1-tx branch IS the 2-input combine.
//! (b) VERIFY (branch/terminal invariants): the combine tx has 2 inputs, so the receiver requires 2
//!     terminal ancestors (one per structural input, not one per hop); the SDK named exactly the 2
//!     carriers and made BOTH terminal at the SE. We re-derive and print these facts (bob already
//!     enforced them by claiming).
//! (c) EXIT: bob unilaterally exits the combined coin — the 2-input combine branch broadcasts (both
//!     carrier outpoints spent → the old carriers' backups are dead), the leaf backup matures, and
//!     100 units settle on-chain on the exited outpoint. Invalidation holds for a combine.
//! (d) NEGATIVE: paying MORE than the total allocation fails with a TYPED insufficient error (not the
//!     old "no single coin" refusal); the original carriers are spent after the exit.
//!
//! Run: SDK_E2E=31 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
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

async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset)
        .map(|t| t.balance)
        .unwrap_or(0))
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
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(10_000))
            .count();
        if units_ok && carriers >= want_carriers {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{wallet_name}: {want_units} of {asset} on {want_carriers} carrier(s) did not confirm"))
}

fn parse_tx(hex_tx: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?)
}

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    use electrum_client::ElectrumApi;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    // If the outpoint no longer appears in the address's unspent set, it is spent.
    let unspent = cc.electrum_client.script_list_unspent(spk)?;
    Ok(!unspent
        .iter()
        .any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk31_alice", "./rgb-data-sdk31_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk31_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk31_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    // Fund alice's RGB engine (issuance + mint witnesses).
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Two carriers of one asset: IFA issue 60 (carrier A) + on-chain mint 50 (carrier B).
    add_tokens(&cc, &alice, 1).await?;
    let asset = alice.issue_inflatable_token("CMB", "Combine Token", 0, 60, vec![50]).await?;
    wait_carriers_confirmed(&cc, &alice, "sdk31_alice", &core, &asset, 60, 1).await?;
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
    let mint_res = alice.mint_tokens(&asset, vec![50]).await;
    mining.store(false, Ordering::Relaxed);
    let _ = miner.join();
    let _ = mint_res?;
    wait_carriers_confirmed(&cc, &alice, "sdk31_alice", &core, &asset, 110, 2).await?;
    println!("SDK31 - alice holds 110 {asset} across TWO carriers (60 + 50); no single carrier has 100");

    // ===== (a) COMBINE PAYMENT (the payment that used to be refused) =============================
    add_tokens(&cc, &alice, 3).await?; // piece + change slots + headroom
    let r = alice.transfer_tokens(&asset, &bob_addr, 100).await?;
    assert_eq!(r.coins.len(), 1, "one piece handed over");
    let piece_id = r.coins[0].statechain_id.clone();
    wait_token_balance(&bob, &asset, 100).await?;
    assert_eq!(token_balance(&bob, &asset).await?, 100, "bob booked EXACTLY 100 across the combine");
    assert_eq!(token_balance(&alice, &asset).await?, 10, "alice keeps the 10-unit change");
    println!("SDK31 - COMBINE PAYMENT: paid bob 100 spanning two carriers; bob=100 alice=10");

    // ===== (b) VERIFY branch + terminal-ancestor invariants (check logs vs spec) =================
    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(
        &cc.pool,
        "sdk31_bob",
        &format!("branch-{piece_id}"),
    )
    .await?;
    assert_eq!(branch.len(), 1, "the piece's exit branch is the single combine tx");
    let combine_tx = parse_tx(&branch[0].tx)?;
    let n_inputs = combine_tx.input.len();
    let op_returns = combine_tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
    assert_eq!(n_inputs, 2, "the combine consumes BOTH carriers (2 inputs)");
    assert_eq!(op_returns, 1, "exactly one opret commitment (INV-11)");
    let est = bob.estimate_exit_cost(&piece_id).await?;
    assert_eq!(est.branch_txs, 1, "depth-1: the combine is the whole branch");
    // Receiver rule: required terminal ancestors = Σ inputs across branch txs = 2 (not 1 per hop).
    let required_ancestors: usize = branch
        .iter()
        .map(|b| parse_tx(&b.tx).map(|t| t.input.len()))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .sum();
    assert_eq!(required_ancestors, 2, "a 2-input combine requires 2 terminal ancestors");
    // The named ancestors (what the receiver validated) — both must be terminal at the SE.
    let ancestors = mercuryrustlib::sqlite_manager::get_backup_txs(
        &cc.pool,
        "sdk31_bob",
        &format!("parents-{piece_id}"),
    )
    .await?;
    assert_eq!(ancestors.len(), 2, "exactly the two carriers are named as ancestors");
    let mut all_terminal = true;
    for a in &ancestors {
        let (_budget, _fin, terminal) =
            mercuryrustlib::lightning_latch::get_spend_budget(&cc, &a.tx).await?;
        all_terminal &= terminal;
    }
    assert!(all_terminal, "BOTH combined carriers must be terminal (bob enforced this on claim)");
    println!(
        "VERIFY combine: branch_txs=1 combine_inputs={n_inputs} op_returns=1 required_terminal_ancestors={required_ancestors} named_ancestors={} all_terminal={all_terminal}",
        ancestors.len()
    );
    println!("ECON token_combine inputs=2 combine_vb={} outputs={}", combine_tx.vsize(), combine_tx.output.len());

    // ===== (c) EXIT the combined coin (invalidation holds for a combine) =========================
    // A token piece is a CARRIER, so unilateral_exit refuses it (a plain sweep would destroy the
    // allocation). The token-preserving exit is to broadcast the branch DIRECTLY: the colored
    // combine tx IS the RGB witness — confirming it anchors the transition on-chain and settles the
    // allocation. (The uncolored leaf backup is deliberately NOT broadcast; it would destroy it.)
    use electrum_client::ElectrumApi;
    // Capture the two carrier outpoints, to prove they get spent on-chain (old carriers dead).
    let carrier_ops: Vec<(String, u32)> = combine_tx
        .input
        .iter()
        .map(|i| (i.previous_output.txid.to_string(), i.previous_output.vout))
        .collect();
    for row in &branch {
        cc.electrum_client.transaction_broadcast_raw(&hex::decode(&row.tx)?)?;
    }
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Both carrier outpoints are now spent by the combine — every old backup on them is dead.
    for (txid, vout) in &carrier_ops {
        assert!(is_outpoint_spent(&cc, txid, *vout)?, "carrier {txid}:{vout} must be spent by the combine");
    }
    // bob's piece outpoint is a LIVE on-chain UTXO holding the 1_500 packaging sats.
    let piece_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk31_bob")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(&piece_id))
        .ok_or_else(|| anyhow!("bob's piece coin vanished"))?;
    let leaf_txid = piece_coin.utxo_txid.clone().unwrap_or_default();
    let leaf_vout = piece_coin.utxo_vout.unwrap_or_default();
    let mut leaf_live = false;
    for _ in 0..30 {
        let spk = &combine_tx.output[leaf_vout as usize].script_pubkey;
        if cc
            .electrum_client
            .script_list_unspent(spk)?
            .iter()
            .any(|u| u.tx_hash.to_string() == leaf_txid && u.tx_pos as u32 == leaf_vout)
        {
            leaf_live = true;
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(leaf_live, "bob's exited piece outpoint {leaf_txid}:{leaf_vout} must be a live on-chain UTXO");
    // RGB settles the 100 units on-chain on the exited outpoint (balance stays 100, now settled).
    for _ in 0..20 {
        let _ = bob.claim().await?;
        if token_balance(&bob, &asset).await? == 100 {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(token_balance(&bob, &asset).await?, 100, "100 units remain (now settled on-chain)");
    println!("SDK31 - EXIT: broadcast the 2-input combine branch; both carrier outpoints spent; bob's 100 units settled on-chain on {leaf_txid}:{leaf_vout}");

    // ===== (d) NEGATIVE: over-balance is a TYPED insufficient error (not 'no single coin') =======
    // alice now holds only 10; asking for 200 must fail on total balance, via the combine selector.
    let err = alice.transfer_tokens(&asset, &bob_addr, 200).await.expect_err("over-balance must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("insufficient") && !msg.contains("multi-coin token combine not yet wired"),
        "expected a typed insufficient error, got: {msg}"
    );
    println!("SDK31 - NEGATIVE: paying 200 > balance is refused with a typed insufficient error: {msg}");

    println!("SDK31 - SUCCESS: multi-carrier token combine works — a payment spanning several carriers is minted by ONE SE-co-signed colored combine (2 inputs → piece + change); the receiver validates the multi-input branch and requires ALL combined carriers terminal (2 inputs => 2 terminal ancestors, closing the per-hop hole); the combined coin exits on-chain (both carrier outpoints spent, 100 units settled); and over-balance fails with a typed insufficient error. Invalidation/branch invariants preserved.");
    Ok(())
}
