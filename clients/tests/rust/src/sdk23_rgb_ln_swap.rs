//! E2E: RGB assets over Lightning (rgb-lightning-node), driven entirely by the SDK's new RlnClient
//! asset methods — the Lightning half of a statechain <-> LN RGB swap. Node A issues an RGB asset,
//! opens a COLORED channel to node B, B raises an RGB (asset) invoice, A decodes it (SDK
//! `RlnClient::decode` must read asset_id + asset_amount) and pays it; the asset then moves A -> B
//! over Lightning (offchain outbound/inbound balances shift by the exact amount).
//!
//! This validates create_utxos / issue_asset / open_asset_channel / ln_invoice_asset / decode /
//! send_payment / asset_balance against a live RLN. The statechain colored latch that bridges this
//! to a Mercury coin (latch_tokens / latch_tokens_se_preimage) is covered by the token E2Es; a full
//! cross-rail swap additionally needs an asset bridge onto a statechain coin (documented follow-up).
//!
//! Run: SDK_E2E=23 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::ssp::RlnClient;
use serde_json::{json, Value};
use std::time::Duration;

use crate::{bitcoin_core, rln};

/// Fund an RLN node on-chain and create colorable UTXOs (prerequisite for RGB).
async fn fund_and_utxos(node: &rln::RlnNode, client: &RlnClient) -> Result<()> {
    // Fund generously: colorable UTXOs (for RGB) + plenty of vanilla sats to fund an LN channel.
    let addr = node.onchain_address().await?;
    bitcoin_core::sendtoaddress(20_000_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut funded = false;
    for _ in 0..60 {
        let info = node.post("btcbalance", json!({ "skip_sync": false })).await.unwrap_or(Value::Null);
        let spendable = info.get("vanilla").and_then(|v| v.get("spendable")).and_then(|v| v.as_u64()).unwrap_or(0);
        if spendable >= 15_000_000 {
            funded = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !funded {
        return Err(anyhow!("RLN node never saw its on-chain funds"));
    }
    // A COLORED channel's funding output carries the asset, so it is funded from a COLORABLE UTXO —
    // that UTXO must hold enough sats to cover the whole channel capacity. Make them large.
    client.create_utxos(4, Some(300_000)).await?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(1, &core)?;
    Ok(())
}

pub async fn execute() -> Result<()> {
    // Two RLN nodes: A = issuer/payer, B = receiver.
    let a = rln::RlnNode::start("/tmp/rln-sdk23/a", 3111, 9845, true).await?;
    let b = rln::RlnNode::start("/tmp/rln-sdk23/b", 3112, 9846, true).await?;
    a.init_unlock("sdk23nodepass-a").await?; // RLN requires >= 8 chars
    b.init_unlock("sdk23nodepass-b").await?;
    let ca = RlnClient::new(&a.api);
    let cb = RlnClient::new(&b.api);
    println!("SDK23 - two RLN nodes up");

    fund_and_utxos(&a, &ca).await?;
    fund_and_utxos(&b, &cb).await?;
    println!("SDK23 - both nodes funded + colorable UTXOs created");

    // A issues 1000 units of a fungible RGB asset.
    let asset_id = ca.issue_asset(1000, "SWAP", "SwapCoin", 0).await?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(1, &core)?;
    let issued = ca.asset_balance(&asset_id).await?;
    assert!(issued.settled >= 1000, "A issued >= 1000 of {asset_id} (got {})", issued.settled);
    println!("SDK23 - A issued 1000 of {asset_id}");

    // A opens a COLORED channel to B carrying 600 units (A keeps all 600 outbound toward B).
    let b_pub = b.pubkey().await?;
    ca.open_asset_channel(&b_pub, b.peer_port, 100_000, 20_000_000, &asset_id, 600, 0).await?;
    // Confirm the funding + wait for the channel to be usable on both ends.
    let mut usable = false;
    for _ in 0..60 {
        let core = bitcoin_core::getnewaddress()?;
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        if a.usable_channels().await.unwrap_or(0) >= 1 && b.usable_channels().await.unwrap_or(0) >= 1 {
            usable = true;
            break;
        }
    }
    if !usable {
        return Err(anyhow!("colored channel never became usable"));
    }
    let a_out_before = ca.asset_balance(&asset_id).await?.offchain_outbound;
    println!("SDK23 - colored channel usable; A offchain outbound = {a_out_before} units");
    assert!(a_out_before >= 600, "A should have >= 600 units outbound in the channel");

    // B raises an RGB invoice for 250 units of the asset.
    let invoice = cb.ln_invoice_asset(None, Some(&asset_id), Some(250), None, 3600).await?;
    println!("SDK23 - B raised an RGB invoice for 250 of the asset");

    // A DECODES it with the SDK — the asset id + amount must be read from the invoice.
    let decoded = ca.decode(&invoice).await?;
    assert_eq!(decoded.asset_id.as_deref(), Some(asset_id.as_str()), "decode reads the asset id");
    assert_eq!(decoded.asset_amount, Some(250), "decode reads the asset amount");
    println!("SDK23 - SDK decoded the RGB invoice: asset {} amount {:?} \u{2713}", asset_id, decoded.asset_amount);

    // A PAYS the RGB invoice over the colored channel.
    ca.send_payment(&invoice).await?;
    let mut settled = false;
    for _ in 0..40 {
        if cb.invoice_status(&invoice).await? == "Succeeded" {
            settled = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !settled {
        return Err(anyhow!("RGB lightning payment did not settle"));
    }

    // The asset moved A -> B over Lightning. Channel balances update asynchronously after the HTLC
    // settles, so poll: A's spendable-over-LN (offchain_outbound) drops by 250, and B can now send
    // 250 (its offchain_outbound), i.e. B genuinely received the asset.
    let mut a_out_after = a_out_before;
    let mut b_out_after = 0;
    for _ in 0..20 {
        a_out_after = ca.asset_balance(&asset_id).await?.offchain_outbound;
        b_out_after = cb.asset_balance(&asset_id).await?.offchain_outbound;
        if a_out_after <= a_out_before - 250 && b_out_after >= 250 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK23 - after pay: A outbound {a_out_before}->{a_out_after}, B now holds {b_out_after} (sendable)");
    assert!(a_out_after <= a_out_before - 250, "A's sendable asset dropped by >= 250 (got {a_out_after})");
    assert!(b_out_after >= 250, "B received >= 250 of the asset over Lightning (holds {b_out_after})");

    println!("SDK23 - SUCCESS: an RGB asset moved over a colored Lightning channel driven end-to-end by the SDK's RlnClient (issue + createutxos + colored openchannel + asset invoice + decode + pay + asset-balance accounting). This is the Lightning half of the statechain<->LN RGB swap; the colored latch (latch_tokens / latch_tokens_se_preimage) bridges it to a Mercury coin.");
    Ok(())
}
