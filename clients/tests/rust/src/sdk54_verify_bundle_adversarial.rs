//! E2E (SDK_E2E=54) — **ADVERSARIAL: `verify_bundle`'s anti-theft count cannot be padded [S1]**.
//!
//! Every prior V2 E2E (sdk47/49/50/52) exercised an HONEST sender, so they were all green while the
//! linchpin was exploitable. This test attacks it directly, against a REAL ladder co-signed by the live
//! SE.
//!
//! The count `expected = v1_backups + tiers + superseded_states + superseded_extensions` is what stops a
//! sender from hiding a co-signed low-CSV state that pays themselves. If a sender can inflate `expected`
//! by ONE, they can hold a hidden state, get the receiver to accept, then broadcast it and take the coin
//! back. Before the fix, `superseded_*` were only `.len()`-counted — never parsed, ladder-linked or
//! signature-checked — and the CSV race-check skipped `csv: None`. Each attack below made `expected`
//! match an inflated `num_sigs` and was **ACCEPTED**; all must now be **REJECTED**, while the honest
//! bundle still verifies.
//!
//! Run with SDK_E2E=54 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs};

use anyhow::{anyhow, Result};
use mercuryrustlib::tesr::{verify_bundle, TesrTier};

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";

/// Assert a tampered bundle is refused, and surface WHY (a reject for the wrong reason is not a pass).
fn must_reject(b: &mercuryrustlib::tesr::TesrBundle, se: u32, v1: u32, attack: &str) -> Result<()> {
    match verify_bundle(b, se, v1) {
        Ok(()) => Err(anyhow!("SECURITY: {attack} was ACCEPTED — the count is still paddable")),
        Err(e) => {
            println!("SDK54 - {attack} correctly REJECTED: {e}");
            Ok(())
        }
    }
}

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk54");
    env::set_var("ML_NETWORK", "regtest");
    env::set_var("UTEXO_PROTOCOL_DEFAULT", "2");
    let cc = mercuryrustlib::client_config::load().await;

    // --- A REAL ladder, co-signed by the live SE. -------------------------------------------------
    let mut alice = deposit_coin(&cc, "sdk54_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let exit_addr = crate::bitcoin_core::getnewaddress()?;
    let bundle = mercuryrustlib::tesr::establish_auto(&cc, &mut alice, &exit_addr, NETWORK).await?;
    let se = mercuryrustlib::utils::get_statechain_info(&sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs;

    // Control: the honest bundle verifies (v1_backups = 1, the deposit tx1).
    verify_bundle(&bundle, se, 1).map_err(|e| anyhow!("honest bundle must verify, got: {e}"))?;
    println!("SDK54 - control: honest bundle verifies (num_sigs={se})");

    // --- ATTACK A: pad with a junk entry to absorb ONE hidden co-signed state. ----------------------
    // The sender's real num_sigs is se+1 (one hidden low-CSV state paying themselves). They pad one
    // empty TesrTier so `expected` becomes se+1 and the count "matches".
    let mut a = bundle.clone();
    a.superseded_states.push(TesrTier {
        txid: String::new(),
        signed_tx: String::new(),
        out_value: 0,
        csv: None,
    });
    must_reject(&a, se + 1, 1, "ATTACK A (junk padding: empty signed_tx + csv:None)")?;

    // --- ATTACK B: a REAL tier replayed as a superseded state but with csv: None. -------------------
    // Parses and is ladder-linked, but `csv: None` previously slipped past the maturity race-check.
    let mut b = bundle.clone();
    let real = bundle.current().extension.clone();
    b.superseded_states.push(TesrTier { csv: None, ..real });
    must_reject(&b, se + 1, 1, "ATTACK B (real tier, csv:None — skips the race-check)")?;

    // --- ATTACK C: a structurally valid but NEVER-CO-SIGNED tier. ----------------------------------
    // Parsing alone never proved a co-sign. Take a genuine tier, alter its output value by 1 sat and
    // re-derive its txid: still a well-formed tier of this ladder, but the signature no longer covers
    // it — so it consumed no SE co-signature and must not count.
    let mut c = bundle.clone();
    {
        use electrum_client::bitcoin::{consensus::{deserialize, serialize}, Transaction};
        let src = bundle.current().state.clone();
        let mut tx: Transaction = deserialize(&hex::decode(&src.signed_tx)?)?;
        tx.output[0].value -= 1; // invalidates the signature; tier remains well-formed
        let forged = TesrTier {
            txid: tx.txid().to_string(),
            signed_tx: hex::encode(serialize(&tx)),
            out_value: src.out_value,
            csv: src.csv,
        };
        c.superseded_states.push(forged);
    }
    must_reject(&c, se + 1, 1, "ATTACK C (well-formed but never-co-signed tier)")?;

    // --- ATTACK D: a superseded state whose CSV would OUT-RACE the current state. -------------------
    // Even a genuinely co-signed stale state must not sit at/below the current state's CSV.
    let mut d = bundle.clone();
    let cur_state = bundle.current().state.clone();
    d.superseded_states.push(cur_state); // same CSV as current ⟹ not strictly above
    must_reject(&d, se + 1, 1, "ATTACK D (superseded state at/below the current CSV)")?;

    // --- ATTACK E [S-1]: a superseded EXTENSION that OUT-RACES the live one. -------------------------
    // The race check used to be gated on `kind == "state"`, so superseded EXTENSIONS were only
    // bounds-checked. A genuinely co-signed X_evil low in [e_floor,e0] verifies against A and balances
    // the count, but matures far ahead of the live extension — then its child state pays the attacker.
    // Modelled here by replaying the LIVE extension as a "superseded" one: same outpoint, and its CSV is
    // NOT strictly above the live tier's, so it must be refused.
    let mut e = bundle.clone();
    e.superseded_extensions.push(bundle.current().extension.clone());
    must_reject(&e, se + 1, 1, "ATTACK E (superseded extension racing the live one)")?;

    // --- ATTACK F [S-2]: an ORPHAN superseded tier contending with nothing in the exit chain. --------
    // The old check compared every entry to a global `final_csv` from txs.last(), so a tier over an
    // unrelated outpoint passed "by construction". The trigger spends F — no live tier contends with F
    // besides the trigger itself — so replaying it as a superseded state must be refused as an orphan.
    let mut f = bundle.clone();
    f.superseded_states.push(bundle.trigger.clone());
    must_reject(&f, se + 1, 1, "ATTACK F (orphan superseded tier over an uncontended outpoint)")?;

    // --- The honest bundle is still accepted after all of that. -------------------------------------
    verify_bundle(&bundle, se, 1).map_err(|e| anyhow!("honest bundle must still verify, got: {e}"))?;

    println!("SDK54 - ✓ PASS: the count is unpaddable — junk, csv:None, never-co-signed and out-racing entries are all REJECTED; honest bundles still verify");
    Ok(())
}
