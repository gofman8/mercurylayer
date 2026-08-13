//! E2E (SDK_E2E=58) — **in-ladder split: verify_child_bundle ACCEPTS a real split child**.
//!
//! Proves the B1 fix end-to-end with NO SGX (Stage-3-necessity ruling wqvoxvusg). A laddered parent coin is
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
/// **[D44] DERIVED, not written down.** This sized the child at a hard-coded 2.0 while the LADDER
/// was built at the preset's committed rate, so when D44 moved that rate to 3.0 the child came out
/// 125 sat too large (`ceil(125*3) - ceil(125*2)`) and the split's own conservation law refused it:
/// "child values sum to 98280 but must equal 98155". A test that writes down a rate the system reads
/// from a preset is a test that breaks on every schedule change, one release after the change.
fn fee_rate() -> f64 {
    mercurylib::tesr::TesrParams::for_network("regtest").committed_fee_rate
}

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

    // --- Parent: deposit + establish (canonical schedule). Capture the pre-ladder baseline count. ---
    let mut parent = deposit_coin(&cc, wallet).await?;
    let parent_sid = parent.statechain_id.clone().ok_or(anyhow!("no parent sid"))?;
    let parent_baseline = num_sigs(&cc, &parent_sid).await?;
    let owner_exit = crate::bitcoin_core::getnewaddress()?;
    let bundle = mercuryrustlib::tesr::establish_auto(&cc, &mut parent, &owner_exit, NETWORK).await?;

    // --- Create the child statechain coin FIRST so SP can pay its aggregate A_child. ----------------
    let x_m = bundle.current().extension.clone();
    let child_value = mercurylib::tesr::tier_out_total(x_m.out_value, 1, fee_rate()).ok_or(anyhow!("fee too high"))?;
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
    // [CATS change 2] `ChangeLeg::None`: this split is one recipient and NO change — the single
    // child takes the whole payload budget, so there is no sender's leg to shape as a spine tip.
    let split = mercuryrustlib::tesr::in_ladder_split(
        &cc,
        wallet,
        &mut parent,
        &bundle,
        &mut children,
        mercuryrustlib::tesr::ChangeLeg::None,
        // [K>1 prerequisite 2] No conveyance plan: this test drives the hand-over itself (or does
        // not hand over at all), so the journal records no recipient for the leg and the resume
        // driver correctly reports it `unactionable` rather than conveying it behind the test.
        &[],
    )
    .await?;
    assert!(split.tip.is_none(), "a ChangeLeg::None split must not mint a spine tip");
    let cb = split.pieces.into_iter().next().ok_or(anyhow!("in_ladder_split returned no child bundle"))?;

    // --- Fetch authoritative values a real child receiver would fetch. ------------------------------
    let f_txid = electrum_client::bitcoin::Txid::from_str(&bundle.f_txid).map_err(|_| anyhow!("bad f_txid"))?;
    let f_tx = cc.electrum_client.transaction_get(&f_txid).map_err(|_| anyhow!("F not on chain"))?;
    let f_spk_hex = hex::encode(f_tx.output[bundle.f_vout as usize].script_pubkey.as_bytes());
    // The funding output's VALUE, read from the same fetched transaction as its scriptPubKey. It is
    // what anchors the parent's trigger, so the tier chain conserves against the chain rather than
    // against a number the sender declared.
    let f_value_onchain = f_tx.output[bundle.f_vout as usize].value;

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
        &cb, &f_spk_hex, f_value_onchain,
        parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
        child_num_sigs, child_baseline, child_agg.as_deref(),
        &[],
        &receiver,
    ).map_err(|e| anyhow!("verify_child_bundle REJECTED a valid split child: {e}"))?;
    println!("SDK58 - control: valid split child ACCEPTED (parent terminal; child non-terminal by design).");

    // ---- ADVERSARIAL: every tampering of the authoritative inputs must REJECT. ---------------------
    // EVERY attack below pins the NAMED error it targets. A bare "rejected for some reason" helper was
    // removed on purpose (the D6 strictness discipline): a security test that passes on an unrelated
    // parse, address or network error reports a safety it never observed, so a rejection carrying any
    // other message is a FAILURE here, not a pass.
    let ok_named = |r: Result<()>, attack: &str, expect: &str| -> Result<()> {
        match r {
            Ok(()) => Err(anyhow!("SECURITY: {attack} was ACCEPTED")),
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains(expect) {
                    return Err(anyhow!(
                        "{attack} was rejected for the WRONG reason — expected an error containing {expect:?}, got: {msg}"
                    ));
                }
                println!("SDK58 - {attack} correctly REJECTED: {msg}");
                Ok(())
            }
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
        mercuryrustlib::tesr::verify_child_bundle(&cb, &f_spk_hex, f_value_onchain, p_ns, parent_baseline, p_agg, p_term, c_ns, child_baseline, c_agg, &[], recv)
    };
    let (pa, ca) = (parent_agg.as_deref(), child_agg.as_deref());

    // Each expectation names the guard under test:
    //   A/B  → the fail-closed `ok_or` on a NULL server aggregate ([2] / [5]),
    //   C/D  → the decoy-sid aggregate comparison ([2] / [5]),
    //   E/F  → the two EXACT-equality censuses (parent segment vs child leaf) — note E must surface the
    //          PARENT census through the `parent segment/census invalid:` wrapper, so that a child-side
    //          mismatch can never be mistaken for parent-side coverage,
    //   G    → the Model-A receiver-key binding,
    //   H    → the parent-terminality fail-closed gate.
    ok_named(vcb(None, parent_terminal, parent_num_sigs, ca, child_num_sigs, &receiver),
        "A (parent aggregate NULL — fail-closed)", "server recorded no aggregate for parent sid")?;
    ok_named(vcb(pa, parent_terminal, parent_num_sigs, None, child_num_sigs, &receiver),
        "B (child aggregate NULL — fail-closed)", "server recorded no aggregate for child sid")?;
    ok_named(vcb(Some(decoy_xonly), parent_terminal, parent_num_sigs, ca, child_num_sigs, &receiver),
        "C (decoy parent aggregate != A_parent)", "parent sid's server aggregate != A_parent")?;
    ok_named(vcb(pa, parent_terminal, parent_num_sigs, Some(decoy_xonly), child_num_sigs, &receiver),
        "D (decoy child aggregate != SP.out[j])", "child sid's server aggregate != SP.out[j] key")?;
    ok_named(vcb(pa, parent_terminal, parent_num_sigs + 1, ca, child_num_sigs, &receiver),
        "E (hidden parent state: parent num_sigs one higher)", "parent segment/census invalid: num_sigs mismatch")?;
    ok_named(vcb(pa, parent_terminal, parent_num_sigs, ca, child_num_sigs + 1, &receiver),
        "F (hidden child state: child num_sigs one higher)", "child num_sigs mismatch")?;
    ok_named(vcb(pa, parent_terminal, parent_num_sigs, ca, child_num_sigs, &other_recv),
        "G (Model A violated: child state pays not-the-receiver)", "Model A violated")?;
    ok_named(vcb(pa, false, parent_num_sigs, ca, child_num_sigs, &receiver),
        "H (parent NOT terminal — a rival trigger over F could still be co-signed)", "parent sid is NOT terminal")?;
    // I is REPLACED, not dropped. The old I asserted a synthetic `child_terminal = false` flag, which no
    // longer exists: a child is deliberately non-terminal so it can be re-transferred, and its safety
    // now rests on the CHILD SUPERSEDED CENSUS. These two attacks exercise that census directly and are
    // strictly stronger than the flag was — they forge the actual objects the census counts.
    //
    // I' (RIVAL RACE): a child superseded state whose CSV ties the LIVE child state's over the same
    // outpoint. A superseded tier is only safe to count because it LOSES the maturity race; one that
    // ties or wins could confirm first and pay the attacker.
    //
    // It must be a GENUINE rival, not a re-labelled copy of the live state. Cloning `child_state` and
    // editing only its declared `csv` field produces a tier whose txid is already a live txid, so the
    // [C-2] one-co-sign-one-slot dedup refuses it before the race check runs — and the declared-vs-tx
    // CSV check would refuse it even without that. Either way the maturity race, the property this
    // attack is named for, would never execute. So build what a real attacker builds: the SE is BLIND
    // and the child slot is deliberately non-terminal, so it will co-sign a SECOND child state over
    // `ext_child`'s payload output, paying the attacker, at the live state's own CSV. Distinct txid
    // (dedup blind), genuinely co-signed (the [S1] battery blind), CSV declared == tx CSV and inside
    // the schedule bounds, and the count passed MAKES the census balance. Only the per-outpoint
    // maturity race can reject it.
    {
        let live_csv = cb.child_state.csv.ok_or(anyhow!("live child state has no CSV"))?;
        let t = mercurylib::tesr::build_state_from(
            &cb.child_extension.txid,
            cb.child_extension.payload_vout,
            cb.child_extension.out_value,
            &other_recv, // the attacker's own key
            NETWORK,
            live_csv, // TIES the live state ⟹ does NOT lose the race
            bundle.fee_rate,
        )?;
        let signed = mercuryrustlib::tesr::cosign_tier(
            &cc,
            &mut children[0].0,
            t.tx_hex.clone(),
            cb.child_extension.out_value,
            NETWORK,
        )
        .await?;
        let child_num_sigs_after = num_sigs(&cc, &child_sid).await?;
        assert_eq!(
            child_num_sigs_after,
            child_num_sigs + 1,
            "the SE really co-signed the rival child state — this is a genuine co-sign, not a forgery"
        );
        let mut rival = cb.clone();
        rival.child_superseded_states.push(mercuryrustlib::tesr::TesrTier {
            txid: t.txid,
            signed_tx: signed,
            out_value: t.out_value,
            csv: Some(live_csv),
            payload_vout: t.payload_vout,
        });
        let r = mercuryrustlib::tesr::verify_child_bundle(
            &rival, &f_spk_hex, f_value_onchain,
            parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
            child_num_sigs_after, child_baseline, child_agg.as_deref(),
            &[],
            &receiver,
        );
        ok_named(
            r,
            "I' (a GENUINELY co-signed rival child state tying the live state's CSV)",
            "race",
        )?;

        // I''' (the same rival, UNDISCLOSED): the co-sign really happened, so the child's exact-equality
        // census must now refuse the untouched bundle at the SE's true count. This is the census doing
        // the job the race check cannot.
        let r = mercuryrustlib::tesr::verify_child_bundle(
            &cb, &f_spk_hex, f_value_onchain,
            parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
            child_num_sigs_after, child_baseline, child_agg.as_deref(),
            &[],
            &receiver,
        );
        ok_named(
            r,
            "I''' (the rival hidden rather than disclosed — exact-equality census)",
            "possible hidden child state",
        )?;
    }
    // I'' (COUNT PADDING, the [S1] class — reachable on the child segment for the first time): pad the
    // child's superseded list with a structurally plausible entry that was never co-signed by A_child,
    // to make the census balance an inflated num_sigs. Only a valid signature proves a tier consumed a
    // co-sign, so this MUST reject.
    //
    // Built by re-deriving a real tier: take the live child state, move one satoshi inside its payload
    // output and recompute its txid. It is still a well-formed, ladder-linked tier with a DISTINCT txid
    // (so the [C-2] dedup does not fire and the txid-binding check passes) — but the co-signature no
    // longer covers it, which is the only thing that proves a tier consumed a co-sign.
    {
        use electrum_client::bitcoin::{consensus::{deserialize, serialize}, Transaction};
        let mut padded = cb.clone();
        let src = cb.child_state.clone();
        let mut tx: Transaction = deserialize(&hex::decode(&src.signed_tx)?)?;
        tx.output[src.payload_vout as usize].value -= 1; // invalidates the signature; tier stays well-formed
        padded.child_superseded_states.push(mercuryrustlib::tesr::TesrTier {
            txid: tx.txid().to_string(),
            signed_tx: hex::encode(serialize(&tx)),
            out_value: src.out_value,
            csv: src.csv,
            payload_vout: src.payload_vout,
        });
        let r = mercuryrustlib::tesr::verify_child_bundle(
            &padded, &f_spk_hex, f_value_onchain,
            parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
            child_num_sigs + 1, child_baseline, child_agg.as_deref(),
            &[],
            &receiver,
        );
        ok_named(
            r,
            "I'' (padded child superseded entry, not co-signed by A_child — count padding)",
            "is not co-signed by A",
        )?;
    }
    // J (VALUE-GATE SPOOF): declare a larger `out_value` than `state_child.out[0]` actually pays. A
    // payer crafting a near-worthless piece while claiming invoice value would pass a value gate that
    // trusts the declared field (the SSP pre-pay census). verify_child_bundle binds out[0].value to the
    // declared out_value, so this MUST reject. Proves the value-binding fix by breaking it.
    {
        let mut spoof = cb.clone();
        spoof.child_state.out_value += 10_000;
        let r = mercuryrustlib::tesr::verify_child_bundle(
            &spoof, &f_spk_hex, f_value_onchain,
            parent_num_sigs, parent_baseline, parent_agg.as_deref(), parent_terminal,
            child_num_sigs, child_baseline, child_agg.as_deref(),
            &[],
            &receiver,
        );
        ok_named(
            r,
            "J (value-gate spoof: declared out_value > state_child.out[0].value)",
            "value-gate spoof",
        )?;
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

    println!("SDK58 - ✓ PASS: split child ACCEPTED (non-terminal by design — it is handed over, not frozen); 12 attacks REJECTED, each for the NAMED reason it targets (aggregates/hidden-state/Model-A/parent-terminality + a GENUINELY co-signed rival child state refused by the per-outpoint maturity race, the same rival refused by the census when hidden, count-padding, value-spoof); and the child EXITS to pay the receiver. B1 closed, split is a real payment, no SGX.");
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
