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

    // --- Create the child statechain coin FIRST so SP can pay its aggregate A_child. ----------------
    let x_m = bundle.current().extension.clone();
    let child_value = mercurylib::tesr::tier_out_total(x_m.out_value, 1, FEE_RATE).ok_or(anyhow!("fee too high"))?;
    let child_token = prepaid_token(&cc).await?;
    let child_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(&cc, wallet, &child_token, u32::try_from(child_value)?).await?;
    let child = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet).await?
        .coins.iter().find(|c| c.aggregated_address.as_deref() == Some(&child_addr)).cloned()
        .ok_or(anyhow!("child coin not found"))?;
    let child_sid = child.statechain_id.clone().ok_or(anyhow!("no child sid"))?;
    let child_baseline = num_sigs(&cc, &child_sid).await?;

    // --- Split IN-LADDER via the PRODUCTION sender (promoted from this test's earlier inline logic). -
    let receiver = crate::sdk58_inladder_split::taproot_addr();
    let mut children = vec![(child.clone(), receiver.clone(), child_value)];
    let bundles = mercuryrustlib::tesr::in_ladder_split(&cc, wallet, &mut parent, &bundle, &mut children).await?;
    let cb = bundles.into_iter().next().ok_or(anyhow!("in_ladder_split returned no child bundle"))?;

    // --- Fetch authoritative values a real child receiver would fetch. ------------------------------
    let f_txid = electrum_client::bitcoin::Txid::from_str(&bundle.f_txid).map_err(|_| anyhow!("bad f_txid"))?;
    let f_tx = cc.electrum_client.transaction_get(&f_txid).map_err(|_| anyhow!("F not on chain"))?;
    let f_spk_hex = hex::encode(f_tx.output[bundle.f_vout as usize].script_pubkey.as_bytes());

    let parent_num_sigs = num_sigs(&cc, &parent_sid).await?;
    let parent_agg = aggregate(&cc, &parent_sid).await?;
    let child_num_sigs = num_sigs(&cc, &child_sid).await?;
    let child_agg = aggregate(&cc, &child_sid).await?;
    println!("SDK58 - parent num_sigs={parent_num_sigs} (baseline {parent_baseline}); child num_sigs={child_num_sigs} (baseline {child_baseline})");

    // Terminality — the DURABLE guarantee the receiver must query (fail-closed), not just the census.
    let (_, _, parent_terminal) = mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_sid).await?;
    let (_, _, child_terminal) = mercuryrustlib::lightning_latch::get_spend_budget(&cc, &child_sid).await?;
    assert!(parent_terminal, "parent must be terminal after the split (budget=1, consumed by SP)");
    // The child is deliberately NOT terminalized any more: the receiver completes the key handover and
    // becomes a first-class owner (CHILDREN.md). Its census is made durable by that handover
    // plus the coordinator's pending-transfer lock, not by terminality. (The LN-latched lane still
    // terminalizes its piece; the verifier tolerates either.)
    assert!(!child_terminal, "a plain (unlatched) split child must stay NON-terminal so it can be re-transferred");

    mercuryrustlib::tesr::verify_child_bundle(
        &cb, &f_spk_hex,
        parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
        child_num_sigs, child_baseline, child_agg.as_deref(),
        &[],
        &receiver,
    ).map_err(|e| anyhow!("verify_child_bundle REJECTED a valid split child: {e}"))?;
    println!("SDK58 - control: valid split child ACCEPTED (parent terminal; child non-terminal by design).");

    // ---- ADVERSARIAL: every tampering of the authoritative inputs must REJECT. ---------------------
    let ok = |r: Result<()>, attack: &str| -> Result<()> {
        match r {
            Ok(()) => Err(anyhow!("SECURITY: {attack} was ACCEPTED")),
            Err(e) => { println!("SDK58 - {attack} correctly REJECTED: {e}"); Ok(()) }
        }
    };
    // A VALID but wrong x-only (a decoy aggregate — exercises the != check, not a parse failure) + a
    // not-the-receiver P2TR key.
    let decoy_xonly = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    let other_recv = {
        use electrum_client::bitcoin::{secp256k1::{Secp256k1, XOnlyPublicKey}, Address, Network};
        let x = XOnlyPublicKey::from_slice(&hex::decode("f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9").unwrap()).unwrap();
        Address::p2tr(&Secp256k1::new(), x, None, Network::Regtest).to_string()
    };
    // Convenience: verify_child_bundle with all-valid args (override individual fields per attack).
    let vcb = |p_agg: Option<&str>, p_term: bool, p_ns: u32, c_agg: Option<&str>, c_ns: u32, recv: &str| {
        mercuryrustlib::tesr::verify_child_bundle(&cb, &f_spk_hex, p_ns, parent_baseline, p_agg, p_term, c_ns, child_baseline, c_agg, &[], recv)
    };
    let (pa, ca) = (parent_agg.as_deref(), child_agg.as_deref());

    ok(vcb(None, parent_terminal, parent_num_sigs, ca, child_num_sigs, &receiver), "A (parent aggregate NULL — fail-closed)")?;
    ok(vcb(pa, parent_terminal, parent_num_sigs, None, child_num_sigs, &receiver), "B (child aggregate NULL — fail-closed)")?;
    ok(vcb(Some(decoy_xonly), parent_terminal, parent_num_sigs, ca, child_num_sigs, &receiver), "C (decoy parent aggregate != A_parent)")?;
    ok(vcb(pa, parent_terminal, parent_num_sigs, Some(decoy_xonly), child_num_sigs, &receiver), "D (decoy child aggregate != SP.out[j])")?;
    ok(vcb(pa, parent_terminal, parent_num_sigs + 1, ca, child_num_sigs, &receiver), "E (hidden parent state: parent num_sigs one higher)")?;
    ok(vcb(pa, parent_terminal, parent_num_sigs, ca, child_num_sigs + 1, &receiver), "F (hidden child state: child num_sigs one higher)")?;
    ok(vcb(pa, parent_terminal, parent_num_sigs, ca, child_num_sigs, &other_recv), "G (Model A violated: child state pays not-the-receiver)")?;
    ok(vcb(pa, false, parent_num_sigs, ca, child_num_sigs, &receiver), "H (parent NOT terminal — a rival trigger over F could still be co-signed)")?;
    // I is REPLACED, not dropped. The old I asserted a synthetic `child_terminal = false` flag, which no
    // longer exists: a child is deliberately non-terminal so it can be re-transferred, and its safety
    // now rests on the CHILD SUPERSEDED CENSUS. These two attacks exercise that census directly and are
    // strictly stronger than the flag was — they forge the actual objects the census counts.
    //
    // I' (RIVAL RACE): disclose a child superseded state whose CSV is <= the LIVE child state's over the
    // same outpoint. A superseded tier is only safe to count because it LOSES the maturity race; one
    // that ties or wins could confirm first and pay the attacker.
    {
        let mut rival = cb.clone();
        let live = rival.child_state.clone();
        let mut sup = live.clone();
        sup.csv = live.csv.map(|c| c.saturating_sub(1)); // strictly LOWER CSV ⟹ matures FIRST
        rival.child_superseded_states.push(sup);
        let r = mercuryrustlib::tesr::verify_child_bundle(
            &rival, &f_spk_hex,
            parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
            child_num_sigs + 1, child_baseline, child_agg.as_deref(),
            &[],
            &receiver,
        );
        ok(r, "I' (child superseded state at a CSV <= the live state's — could out-race the owner)")?;
    }
    // I'' (COUNT PADDING, the [S1] class — reachable on the child segment for the first time): pad the
    // child's superseded list with a structurally plausible entry that was never co-signed by A_child,
    // to make the census balance an inflated num_sigs. Only a valid signature proves a tier consumed a
    // co-sign, so this MUST reject.
    {
        let mut padded = cb.clone();
        let mut junk = padded.child_state.clone();
        junk.csv = junk.csv.map(|c| c.saturating_add(1)); // would lose the race — but is not co-signed
        junk.txid = "0".repeat(64);
        padded.child_superseded_states.push(junk);
        let r = mercuryrustlib::tesr::verify_child_bundle(
            &padded, &f_spk_hex,
            parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
            child_num_sigs + 1, child_baseline, child_agg.as_deref(),
            &[],
            &receiver,
        );
        ok(r, "I'' (padded child superseded entry, not co-signed by A_child — count padding)")?;
    }
    // J (VALUE-GATE SPOOF): declare a larger `out_value` than `state_child.out[0]` actually pays. A
    // payer crafting a near-worthless piece while claiming invoice value would pass a value gate that
    // trusts the declared field (the SSP pre-pay census). verify_child_bundle binds out[0].value to the
    // declared out_value, so this MUST reject. Proves the value-binding fix by breaking it.
    {
        let mut spoof = cb.clone();
        spoof.child_state.out_value += 10_000;
        let r = mercuryrustlib::tesr::verify_child_bundle(
            &spoof, &f_spk_hex,
            parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
            child_num_sigs, child_baseline, child_agg.as_deref(),
            &[],
            &receiver,
        );
        ok(r, "J (value-gate spoof: declared out_value > state_child.out[0].value)")?;
    }

    // ---- THE PAYOFF: exit the child through the FULL pre-co-signed chain; the receiver is paid. -------
    // F -> T -> X_m -> SP -> ext_child -> state_child(receiver). Every tx is already co-signed; the
    // recipient exits unilaterally (no keyupdate needed — the child is terminal, exit uses signed txs).
    use crate::sdk40_tesr_consensus::{broadcast, mine, tx_exists, wait_for_address};
    let mut chain: Vec<(String, Option<u16>)> =
        cb.parent.exit_tiers().iter().map(|t| (t.signed_tx.clone(), t.csv)).collect();
    chain.push((cb.child_extension.signed_tx.clone(), cb.child_extension.csv));
    chain.push((cb.child_state.signed_tx.clone(), cb.child_state.csv));
    let _ = broadcast(&cc, &chain[0].0)?; // trigger — no timelock, F is on-chain
    for (signed, csv) in &chain[1..] {
        let _ = mine(csv.unwrap() as u32);
        let _ = broadcast(&cc, signed)?;
    }
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &cb.child_state.txid), "child final state confirms on-chain");
    assert!(
        wait_for_address(&cc, &receiver, cb.child_state.out_value as u32).await.is_ok(),
        "the split child's value lands at the RECEIVER's key"
    );
    println!("SDK58 - child EXITED: {} sat landed at the receiver via the pre-signed chain.", cb.child_state.out_value);

    println!("SDK58 - ✓ PASS: split child ACCEPTED (non-terminal by design — it is handed over, not frozen); 11 attacks REJECTED (aggregates/hidden-state/Model-A/parent-terminality/child-superseded race + count-padding/value-spoof); and the child EXITS to pay the receiver. B1 closed, split is a real payment, no SGX.");
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
