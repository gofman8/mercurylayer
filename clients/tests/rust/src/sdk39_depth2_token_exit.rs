//! E2E (depth-2 token exit): a token piece that is TWO colored splits deep can still be exited
//! (materialized) on-chain end-to-end, preserving the RGB allocation. Closes the
//! granularity-deep-dive §5.6 test gap ("no E2E yet covers a depth >= 2 token exit").
//!
//! How a depth-2 colored sub-coin arises through the SDK: alice issues on a flat carrier (depth 0);
//! her first `transfer_tokens` splits the carrier -> recipient piece (depth 1) + colored change1
//! (an off-chain sub-coin); her second `transfer_tokens` must reuse change1 (the only carrier still
//! holding the asset) and splits IT -> the new recipient's piece inherits change1's branch and
//! appends the second split, so its exit branch is [split1, split2] = DEPTH 2. That recipient then
//! broadcasts both branch txs root-first, spending the on-chain root, and the 250 units settle
//! on-chain — no SE, allocation preserved.
//!
//! Run: SDK_E2E=39 ML_NETWORK=regtest cargo run

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
    Err(anyhow!("balance of {asset} did not reach {want}"))
}
async fn wait_carrier(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str, asset: &str, units: u64) -> Result<()> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        if token_balance(w, asset).await? >= units {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?;
            if rec.coins.iter().any(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(10_000)) {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: {units} of {asset} carrier did not confirm"))
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

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk39_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk39_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk39_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    add_tokens(&cc, &alice, 8).await?;
    let asset = alice.issue_token("D2", "Depth Two", 0, 1000).await?;
    wait_carrier(&cc, &alice, "sdk39_alice", &core, &asset, 1000).await?;
    println!("SDK39 - alice issued 1000 {asset} on a flat carrier (depth 0)");

    // First transfer: splits the carrier -> bob's depth-1 piece + alice's colored change1.
    let _r1 = alice.transfer_tokens(&asset, &bob_addr, 250).await?;
    wait_token_balance(&bob, &asset, 250).await?;
    assert_eq!(token_balance(&alice, &asset).await?, 750, "alice keeps 750 on change1");
    println!("SDK39 - alice -> bob 250 (depth 1); alice holds 750 on change1 (an off-chain sub-coin)");

    // Second transfer: alice's only asset carrier now is change1 (off-chain), so this splits change1
    // -> carol's piece inherits change1's branch and appends split2 = DEPTH 2.
    let r2 = alice.transfer_tokens(&asset, &carol_addr, 250).await?;
    let carol_piece = r2.coins[0].statechain_id.clone();
    wait_token_balance(&carol, &asset, 250).await?;
    println!("SDK39 - alice -> carol 250; carol's piece {carol_piece}");

    // Carol's exit branch must be DEPTH 2 (two colored splits from the on-chain carrier).
    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk39_carol", &format!("branch-{carol_piece}")).await?;
    assert_eq!(branch.len(), 2, "carol's piece is a DEPTH-2 sub-coin (branch = [split1, split2]); got depth {}", branch.len());
    // Root-first: branch[0] spends the on-chain carrier outpoint.
    let root_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&branch[0].tx)?)?;
    let root = root_tx.input[0].previous_output;
    assert!(!is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "the on-chain root is unspent before exit");
    println!("SDK39 - carol's exit branch is DEPTH 2; on-chain root {}:{} still unspent", root.txid, root.vout);

    // Materialize: broadcast BOTH branch txs root-first. This settles the depth-2 chain on-chain.
    for row in &branch {
        cc.electrum_client.transaction_broadcast_raw(&hex::decode(&row.tx)?)?;
    }
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "materializing the depth-2 branch spent the on-chain root");
    // The whole 2-tx chain is now on-chain; carol's 250 units settle as an ordinary RGB holding.
    for _ in 0..25 {
        let _ = carol.claim().await?;
        if token_balance(&carol, &asset).await? == 250 {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(token_balance(&carol, &asset).await?, 250, "carol's 250 units are preserved on-chain after the depth-2 exit");
    println!("SDK39 - carol materialized her DEPTH-2 branch (broadcast split1+split2 root-first), spent the on-chain root, and settled 250 {asset} on-chain — the allocation is preserved without the SE");

    println!("SDK39 - SUCCESS: a depth-2 colored sub-coin exits end-to-end. Two successive token transfers build a piece whose exit branch is [split1, split2]; broadcasting both root-first materializes the whole chain on-chain and preserves the 250-unit RGB allocation. The deep colored-exit path (granularity-deep-dive §5.6) is proven beyond depth 1.");
    Ok(())
}
