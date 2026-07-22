//! E2E (SDK_E2E=58) — **in-ladder split: verify_child_bundle ACCEPTS a real split child**.
//!
//! Proves the B1 fix end-to-end with NO SGX (Stage-3-necessity ruling wqvoxvusg). A parent V2 coin is
//! split IN-LADDER: SP is a STATE tier spending X_m.out[0] (a DESCENDANT of the trigger, not a rival for
//! F), paying one child statechain coin; the parent is terminalized and its old owner state S_0 disclosed
//! as superseded (out-raced by SP). The child's two-aggregate exit bundle (ancestors under A_parent, child
//! tiers under A_child = SP.out[0]'s key) is then checked by verify_child_bundle against authoritative
//! values fetched from chain + /info/statechain — and must ACCEPT.
//!
//! This inlines the eventual in_ladder_split sender, so it also validates that flow. Run: SDK_E2E=58.

use std::env;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";
const FEE_RATE: f64 = 2.0;

async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc).await?.ok_or(anyhow!("no info"))?.num_sigs)
}
async fn aggregate(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<Option<String>> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc).await?.ok_or(anyhow!("no info"))?.aggregate_pubkey)
}
async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let t = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &t).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] { let _ = std::fs::remove_file(f); }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk58");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk58_alice";

    // --- Parent: deposit + establish (canonical schedule). Capture the V1 baseline count. -----------
    let mut parent = deposit_coin(&cc, wallet).await?;
    let parent_sid = parent.statechain_id.clone().ok_or(anyhow!("no parent sid"))?;
    let parent_baseline = num_sigs(&cc, &parent_sid).await?;
    let owner_exit = crate::bitcoin_core::getnewaddress()?;
    let bundle = mercuryrustlib::tesr::establish_auto(&cc, &mut parent, &owner_exit, NETWORK).await?;

    // X_m (deepest extension) hosts the split state SP on its out[0]. S_0 is the current owner state.
    let x_m = bundle.current().extension.clone();
    let p = bundle.params;
    let s0_csv = bundle.current().state.csv.ok_or(anyhow!("S_0 has no csv"))?;
    // SP must OUT-RACE S_0 over X_m.out[0]: one rung lower.
    let sp_csv = s0_csv.checked_sub(p.delta).filter(|c| *c >= p.d_floor).ok_or(anyhow!("state at floor"))?;

    // --- Create the child statechain coin FIRST so SP can pay its aggregate A_child. ----------------
    let child_value = mercurylib::tesr::tier_out_total(x_m.out_value, 1, FEE_RATE).ok_or(anyhow!("fee too high"))?;
    let child_token = prepaid_token(&cc).await?;
    let child_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(&cc, wallet, &child_token, u32::try_from(child_value)?).await?;
    // The child coin now exists (SE handshake done): statechain_id + aggregate == child_addr, no on-chain utxo.
    let child = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet).await?
        .coins.iter().find(|c| c.aggregated_address.as_deref() == Some(&child_addr)).cloned()
        .ok_or(anyhow!("child coin not found"))?;
    let child_sid = child.statechain_id.clone().ok_or(anyhow!("no child sid"))?;
    let child_baseline = num_sigs(&cc, &child_sid).await?;

    // --- Build SP (spends X_m.out[0], pays the child), terminalize the parent, co-sign SP. ----------
    let sp = mercurylib::tesr::build_split_state(&x_m.txid, x_m.out_value, &[(child_addr.clone(), child_value)], NETWORK, sp_csv, FEE_RATE)?;
    mercuryrustlib::lightning_latch::set_spend_budget(&cc, wallet, &parent_sid, 1).await?;
    let sp_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut parent, sp.tx_hex.clone(), x_m.out_value, NETWORK).await?;

    // --- Establish the child's headless ladder off SP.out[0], paying the receiver (Model A). --------
    let receiver = crate::sdk58_inladder_split::taproot_addr();
    let mut child_coin = child.clone();
    let child_ladder = mercuryrustlib::tesr::establish_child(
        &cc, &mut child_coin, &sp.txid, 0, child_value, &receiver,
        p.ext_csv(0), p.state_csv(0), FEE_RATE, NETWORK,
    ).await?;

    // --- Assemble the parent segment with SP as the current (terminal) state, S_0 superseded. -------
    let mut parent_seg = bundle.clone();
    let last = parent_seg.levels.len() - 1;
    parent_seg.superseded_states.push(parent_seg.levels[last].state.clone());
    parent_seg.levels[last].state = mercuryrustlib::tesr::TesrTier {
        txid: sp.txid.clone(), signed_tx: sp_signed, out_value: child_value, csv: Some(sp_csv),
    };

    let cb = mercuryrustlib::tesr::ChildTesrBundle {
        parent: parent_seg,
        parent_statechain_id: parent_sid.clone(),
        sp_vout: 0,
        child_statechain_id: child_sid.clone(),
        child_owner_exit_address: receiver.clone(),
        child_extension: child_ladder.extension,
        child_state: child_ladder.state,
        child_superseded_states: vec![],
        child_superseded_extensions: vec![],
    };

    // --- Fetch authoritative values a real child receiver would fetch. ------------------------------
    let f_txid = electrum_client::bitcoin::Txid::from_str(&bundle.f_txid).map_err(|_| anyhow!("bad f_txid"))?;
    let f_tx = cc.electrum_client.transaction_get(&f_txid).map_err(|_| anyhow!("F not on chain"))?;
    let f_spk_hex = hex::encode(f_tx.output[bundle.f_vout as usize].script_pubkey.as_bytes());

    let parent_num_sigs = num_sigs(&cc, &parent_sid).await?;
    let parent_agg = aggregate(&cc, &parent_sid).await?;
    let child_num_sigs = num_sigs(&cc, &child_sid).await?;
    let child_agg = aggregate(&cc, &child_sid).await?;
    println!("SDK58 - parent num_sigs={parent_num_sigs} (baseline {parent_baseline}); child num_sigs={child_num_sigs} (baseline {child_baseline})");

    mercuryrustlib::tesr::verify_child_bundle(
        &cb, &f_spk_hex,
        parent_num_sigs, parent_baseline, parent_agg.as_deref(),
        child_num_sigs, child_baseline, child_agg.as_deref(),
        &receiver,
    ).map_err(|e| anyhow!("verify_child_bundle REJECTED a valid split child: {e}"))?;

    println!("SDK58 - ✓ PASS: in-ladder split child bundle ACCEPTED — two-aggregate census sound, A_parent on-chain-rooted, A_child == SP.out[0], Model A holds. B1 fix verified with no SGX.");
    Ok(())
}

use std::str::FromStr;

/// A fixed valid regtest P2TR receiver address (from the secp generator x-coord).
pub(crate) fn taproot_addr() -> String {
    use electrum_client::bitcoin::{secp256k1::{Secp256k1, XOnlyPublicKey}, Address, Network};
    let xonly = XOnlyPublicKey::from_slice(
        &hex::decode("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798").unwrap(),
    ).unwrap();
    Address::p2tr(&Secp256k1::new(), xonly, None, Network::Regtest).to_string()
}
