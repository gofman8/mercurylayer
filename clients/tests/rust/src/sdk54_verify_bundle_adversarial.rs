//! E2E (SDK_E2E=54) — **ADVERSARIAL: `verify_bundle`'s anti-theft count cannot be padded [S1]**.
//!
//! Every prior TES-R E2E (sdk47/49/50/52) exercised an HONEST sender, so they were all green while the
//! linchpin was exploitable. This test attacks it directly, against a REAL ladder co-signed by the live
//! SE — the one the DEPOSIT established at first mempool sight of F (T, X_0, S_0), loaded from disk,
//! never re-established.
//!
//! The count `expected = tiers + superseded_states + superseded_extensions` — with NO flat term: a
//! laddered coin has no `tx1`, its three deposit co-signs are all tiers, and the receiver passes
//! `flat_backups = 0` — is what stops a sender from hiding a co-signed low-CSV state that pays
//! themselves. If a sender can inflate `expected` by ONE, they can hold a hidden state, get the
//! receiver to accept, then broadcast it and take the coin back. Before the fix, `superseded_*` were
//! only `.len()`-counted — never parsed, ladder-linked or signature-checked — and the CSV race-check
//! skipped `csv: None`. Each attack below made `expected` match an inflated `num_sigs` and was
//! **ACCEPTED**; all must now be **REJECTED**, while the honest bundle still verifies. Two attacks
//! target the retired flat term itself: a flat term of 1 budgeted against a laddered coin (the old
//! deposit-`tx1` baseline — exactly one free census slot) and a flat backup CONVEYED beside the
//! ladder (ATTACK H, `verify_flat_backup_lane`), which the receiver must refuse by name.
//!
//! Run with SDK_E2E=54 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs};

use anyhow::{anyhow, Result};
use mercuryrustlib::tesr::{verify_bundle, TesrTier};

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";

/// Assert a tampered bundle is refused, and surface WHY (a reject for the wrong reason is not a pass).
///
/// `se` is the SE's reported `num_sigs`; the third argument is `verify_bundle`'s `flat_backups` term —
/// ZERO for every laddered coin (no `tx1` is co-signed at deposit and none at any hop; the receiver
/// passes 0). It is a parameter here only so the retired baseline can be shown NOT to balance. `expect` is a substring of
/// the NAMED error the attack targets: a rejection carrying any other message means the check under
/// test never ran, so the test fails rather than reporting a safety it did not observe.
fn must_reject(
    b: &mercuryrustlib::tesr::TesrBundle,
    se: u32,
    flat_backups: u32,
    attack: &str,
    expect: &str,
) -> Result<()> {
    match verify_bundle(b, se, flat_backups) {
        Ok(()) => Err(anyhow!("SECURITY: {attack} was ACCEPTED — the count is still paddable")),
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains(expect) {
                return Err(anyhow!(
                    "{attack} was rejected for the WRONG reason — expected an error containing {expect:?}, got: {msg}"
                ));
            }
            println!("SDK54 - {attack} correctly REJECTED: {msg}");
            Ok(())
        }
    }
}

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk54");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // --- A REAL ladder, co-signed by the live SE: the one the DEPOSIT established at sight. --------
    // Loaded, not re-established — a second ladder over F would be three more irreversible co-signs
    // the census could never account for.
    let mut alice = deposit_coin(&cc, "sdk54_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk54_alice", &sid)
        .await?
        .ok_or(anyhow!("the deposit must have been laddered at first sight — it has no other exit material"))?;
    let se = mercuryrustlib::utils::get_statechain_info(&sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs;
    assert_eq!(se, 3, "a deposited coin's enclave count is exactly its three tiers (T, X_0, S_0) — no tx1");

    // Control: the honest bundle verifies with the flat term 0 (the three co-signs are the tiers).
    verify_bundle(&bundle, se, 0).map_err(|e| anyhow!("honest bundle must verify, got: {e}"))?;
    println!("SDK54 - control: honest bundle verifies (num_sigs={se}, flat term 0)");

    // --- ATTACK 0: the RETIRED flat term. A verifier that still budgets one deposit `tx1` hands every
    // coin one free census slot: at the honest count it refuses every honest coin, and at count+1 it
    // launders exactly one hidden state. Neither may balance.
    must_reject(&bundle, se, 1, "ATTACK 0 (a flat term of 1 — the retired deposit tx1 — at the honest count)", "num_sigs mismatch")?;

    // --- ATTACK A: pad with a junk entry to absorb ONE hidden co-signed state. ----------------------
    // The sender's real num_sigs is se+1 (one hidden low-CSV state paying themselves). They pad one
    // empty TesrTier so `expected` becomes se+1 and the count "matches".
    let mut a = bundle.clone();
    a.superseded_states.push(TesrTier {
        txid: String::new(),
        signed_tx: String::new(),
        out_value: 0,
        csv: None,
        payload_vout: 0,
    });
    must_reject(&a, se + 1, 0, "ATTACK A (junk padding: empty signed_tx + csv:None)", "not a transaction")?;

    // --- ATTACK B: a REAL LIVE tier replayed as a superseded state (with csv: None). -----------------
    // Historically this probed the `csv: None` skip in the maturity race-check; since [C-2] the tier is
    // refused one step earlier, by the one-co-sign-one-slot dedup, because its txid is ALSO a live tier.
    // That is the correct (and stricter) outcome, so this attack now asserts the DEDUP by name — the
    // `csv: None` and maturity-race properties it used to cover are kept alive by B' and D' below,
    // which use a rival the dedup cannot see.
    let mut b = bundle.clone();
    let real = bundle.current().extension.clone();
    b.superseded_states.push(TesrTier { csv: None, ..real });
    must_reject(
        &b,
        se + 1,
        0,
        "ATTACK B (a LIVE tier replayed as superseded — [C-2] dedup)",
        "is disclosed more than once",
    )?;

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
            payload_vout: src.payload_vout,
        };
        c.superseded_states.push(forged);
    }
    must_reject(&c, se + 1, 0, "ATTACK C (well-formed but never-co-signed tier)", "is not co-signed by A")?;

    // --- ATTACK D: the LIVE state replayed as a superseded one. -------------------------------------
    // Same story as B: since [C-2] this is caught by the dedup rather than the maturity race. The race
    // property itself is exercised by D' (a genuinely co-signed RIVAL state with a distinct txid).
    let mut d = bundle.clone();
    let cur_state = bundle.current().state.clone();
    d.superseded_states.push(cur_state); // same CSV as current ⟹ not strictly above
    must_reject(
        &d,
        se + 1,
        0,
        "ATTACK D (the LIVE state replayed as superseded — [C-2] dedup)",
        "is disclosed more than once",
    )?;

    // --- ATTACK E [S-1]: the LIVE extension replayed as a superseded one. ----------------------------
    // The [S-1] property (extensions are race-checked, not merely bounds-checked) is now carried by E',
    // which co-signs a REAL rival extension over T's payload output; replaying the live one is caught
    // first by the [C-2] dedup, which is what this asserts.
    let mut e = bundle.clone();
    e.superseded_extensions.push(bundle.current().extension.clone());
    must_reject(
        &e,
        se + 1,
        0,
        "ATTACK E (the LIVE extension replayed as superseded — [C-2] dedup)",
        "is disclosed more than once",
    )?;

    // --- ATTACK F [S-2]: the LIVE trigger replayed as a superseded tier. -----------------------------
    // Again dedup-first since [C-2]. The ORPHAN property ("contends with nothing in the exit chain") is
    // exercised for real by F' below, with a co-signed rival rooted directly at F.
    let mut f = bundle.clone();
    f.superseded_states.push(bundle.trigger.clone());
    must_reject(
        &f,
        se + 1,
        0,
        "ATTACK F (the LIVE trigger replayed as superseded — [C-2] dedup)",
        "is disclosed more than once",
    )?;

    // --- The honest bundle is still accepted after all of that. -------------------------------------
    verify_bundle(&bundle, se, 0).map_err(|e| anyhow!("honest bundle must still verify, got: {e}"))?;

    // --- TRANSITIVE-DEATH path (renew supersedes BOTH the extension and the state). ------------------
    // After a renew the old state spends the OLD (superseded) extension's out[0], which no LIVE tier
    // spends. It is safely dead — its parent lost its own race for T:0 and can never confirm — so
    // verify_bundle must ACCEPT it. (Before the transitive-death fix this bricked every renewed coin.)
    let mut renewed = bundle.clone();
    mercuryrustlib::tesr::renew_auto(&cc, &mut alice, &mut renewed).await?;
    let se_renew = mercuryrustlib::utils::get_statechain_info(&sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs;
    verify_bundle(&renewed, se_renew, 0)
        .map_err(|e| anyhow!("renewed bundle must verify (transitive-death ACCEPT path), got: {e}"))?;
    println!("SDK54 - control: renewed bundle verifies — superseded state over a dead extension ACCEPTED (num_sigs={se_renew})");

    // --- ATTACK G: a superseded STATE whose parent extension is NOT disclosed as a dead tier. --------
    // The renewed bundle's superseded state is only safe because its parent superseded extension is
    // disclosed AND provably dead. Drop that parent extension: the state now roots in NOTHING dead, so
    // it must be refused (it could otherwise pad the count while remaining broadcastable if its parent
    // could somehow confirm). We pass the count that MATCHES the reduced disclosure (se_renew − 1) so the
    // rejection is the STRUCTURAL orphan/linkage check, not a count mismatch.
    let mut g = renewed.clone();
    g.superseded_extensions.pop(); // remove the dead parent; the superseded state now has no dead root
    must_reject(
        &g,
        se_renew - 1,
        0,
        "ATTACK G (superseded state with no disclosed dead parent)",
        "spends an outpoint outside this ladder",
    )?;

    // --- ATTACK H: a flat backup CONVEYED beside the ladder (`verify_flat_backup_lane`). ------------
    // The census has no flat term, so the one place a sender could still smuggle a co-sign into a
    // receiver's arithmetic is the transfer message's `backup_transactions` vector — the vector the
    // old `flat_backups = 1` baseline was read from. One entry there is a co-sign the census cannot
    // account for and, matured, a spend of F a prior owner keeps. Every laddered conveyance (claim
    // path and pre-pay path) runs `verify_flat_backup_lane` on that vector: it must refuse a
    // non-empty one BY NAME and admit the empty vector the sender actually conveys.
    {
        let smuggled = mercurylib::wallet::BackupTx {
            tx_n: 1,
            tx: bundle.trigger.signed_tx.clone(),
            client_public_nonce: String::new(),
            server_public_nonce: String::new(),
            client_public_key: String::new(),
            server_public_key: String::new(),
            blinding_factor: String::new(),
            rgb_consignment: None,
            rgb_blinding: None,
        };
        match mercuryrustlib::tesr::verify_flat_backup_lane(&bundle, &[smuggled]) {
            Ok(()) => return Err(anyhow!("SECURITY: ATTACK H (a flat backup conveyed beside the ladder) was ACCEPTED")),
            Err(e) => {
                let msg = e.to_string();
                let expect = "refusing a plain ladder conveyed with 1 flat backup transaction(s)";
                if !msg.contains(expect) {
                    return Err(anyhow!(
                        "ATTACK H was rejected for the WRONG reason — expected an error containing {expect:?}, got: {msg}"
                    ));
                }
                println!("SDK54 - ATTACK H (a flat backup conveyed beside the ladder) correctly REJECTED: {msg}");
            }
        }
        mercuryrustlib::tesr::verify_flat_backup_lane(&bundle, &[])
            .map_err(|e| anyhow!("the EMPTY backup vector every laddered conveyance carries must be admitted: {e}"))?;
    }

    // ================= GENUINE RIVALS: the checks the [C-2] dedup would otherwise hide =================
    //
    // B/D/E/F above all pad with a tier that is ALSO a live tier, so since [C-2] they are refused by the
    // one-co-sign-one-slot dedup before the per-prevout maturity race, the mandatory-CSV check and the
    // orphan check ever run. Left there, this suite would report those three properties as covered while
    // never executing them. So the rivals below are built the way a real attacker would: the SE is BLIND
    // and co-signs whatever a non-terminal owner hands it, so each rival is a genuine, fully co-signed
    // tier of this ladder with a DISTINCT txid. Dedup cannot see it; the signature check passes; the
    // count passed in is the one that MAKES it balance. Only the property under test can reject it.
    //
    // These run LAST on purpose: each co-sign advances the SE's counter, which would otherwise break the
    // real-count controls above.
    let attacker_addr = {
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        // A valid regtest P2TR the attacker controls (fixed generator-derived x-only).
        let x = XOnlyPublicKey::from_slice(&hex::decode(
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        )?)?;
        Address::p2tr(&Secp256k1::new(), x, None, Network::Regtest).to_string()
    };
    let live_ext = bundle.current().extension.clone();
    let live_state_csv = bundle.current().state.csv.ok_or(anyhow!("live state has no CSV"))?;
    let live_ext_csv = live_ext.csv.ok_or(anyhow!("live extension has no CSV"))?;

    // R1 — a RIVAL STATE over the live extension's payload output, at the SAME CSV as the live state,
    //      paying the attacker. It does not lose the maturity race, so counting it would let the
    //      attacker keep a broadcastable state that ties the owner's.
    let rival_state = {
        let t = mercurylib::tesr::build_state_from(
            &live_ext.txid,
            live_ext.payload_vout,
            live_ext.out_value,
            &attacker_addr,
            NETWORK,
            live_state_csv,
            bundle.fee_rate,
        )?;
        let signed = mercuryrustlib::tesr::cosign_tier(
            &cc,
            &mut alice,
            t.tx_hex.clone(),
            live_ext.out_value,
            NETWORK,
        )
        .await?;
        TesrTier {
            txid: t.txid,
            signed_tx: signed,
            out_value: t.out_value,
            csv: Some(live_state_csv),
            payload_vout: t.payload_vout,
        }
    };

    // ATTACK B' [the MANDATORY-CSV check, restored] — the same genuine rival, declared with `csv: None`.
    // A missing CSV must be a rejection in its own right; it may never be treated as "unraceable".
    {
        let mut b2 = bundle.clone();
        b2.superseded_states.push(TesrTier { csv: None, ..rival_state.clone() });
        must_reject(
            &b2,
            se + 1,
            0,
            "ATTACK B' (a genuinely co-signed rival state declared with csv:None)",
            "no CSV declared",
        )?;
    }

    // ATTACK D' [the PER-PREVOUT MATURITY RACE, restored] — the rival, honestly declared. It parses, is
    // ladder-linked, is co-signed by A, its declared CSV matches its tx and sits inside the schedule
    // bounds, and the census balances at se+1. The maturity race against the LIVE state over the same
    // outpoint is the only thing that can refuse it.
    {
        let mut d2 = bundle.clone();
        d2.superseded_states.push(rival_state.clone());
        must_reject(
            &d2,
            se + 1,
            0,
            "ATTACK D' (a genuinely co-signed rival state that TIES the live state's CSV)",
            "race",
        )?;
    }

    // ATTACK E' [S-1, restored] — the same, one rung up: a genuinely co-signed rival EXTENSION over the
    // trigger's payload output at the live extension's CSV. Extensions must be race-checked exactly like
    // states, or X_evil matures alongside the honest extension and its child state pays the attacker.
    //
    // It pays the ATTACKER, not `A`. That is not cosmetic: a txid does not commit to the witness, so a
    // rival built with the same prevout, the same nSequence and the same output would be BIT-IDENTICAL
    // to the live extension and the [C-2] dedup would (correctly) catch it before the race check —
    // exactly the short-circuit this battery exists to route around. Superseded tiers carry no payee
    // check, so an attacker-paying extension is both a distinct tx and the more honest threat model.
    {
        let t = mercurylib::tesr::build_extension(
            &bundle.trigger.txid,
            bundle.trigger.out_value,
            &attacker_addr,
            NETWORK,
            live_ext_csv,
            bundle.fee_rate,
        )?;
        let signed = mercuryrustlib::tesr::cosign_tier(
            &cc,
            &mut alice,
            t.tx_hex.clone(),
            bundle.trigger.out_value,
            NETWORK,
        )
        .await?;
        let mut e2 = bundle.clone();
        e2.superseded_extensions.push(TesrTier {
            txid: t.txid,
            signed_tx: signed,
            out_value: t.out_value,
            csv: Some(live_ext_csv),
            payload_vout: t.payload_vout,
        });
        must_reject(
            &e2,
            se + 1,
            0,
            "ATTACK E' (a genuinely co-signed rival EXTENSION that ties the live extension's CSV)",
            "race",
        )?;
    }

    // ATTACK F' [S-2, restored] — a genuinely co-signed tier rooted directly at F, under a CSV. F is in
    // the ladder (so the linkage check passes) but the exit chain's race map is seeded from the tiers
    // BELOW the trigger, so nothing live contends with F: this rival is never out-raced and is not
    // transitively dead. It is a live threat branch — a timelocked rival trigger — and must be refused
    // as an orphan rather than counted.
    {
        let t = mercurylib::tesr::build_extension_from(
            &bundle.f_txid,
            bundle.f_vout,
            bundle.f_value,
            &bundle.agg_address,
            NETWORK,
            live_ext_csv,
            bundle.fee_rate,
        )?;
        let signed =
            mercuryrustlib::tesr::cosign_tier(&cc, &mut alice, t.tx_hex.clone(), bundle.f_value, NETWORK)
                .await?;
        let mut f2 = bundle.clone();
        f2.superseded_extensions.push(TesrTier {
            txid: t.txid,
            signed_tx: signed,
            out_value: t.out_value,
            csv: Some(live_ext_csv),
            payload_vout: t.payload_vout,
        });
        must_reject(
            &f2,
            se + 1,
            0,
            "ATTACK F' (a genuinely co-signed rival rooted at F — contends with no live tier)",
            "orphan/threat branch",
        )?;
    }

    // --- The honest bundle is STILL accepted at its own count after the genuine-rival battery. --------
    verify_bundle(&bundle, se, 0)
        .map_err(|e| anyhow!("honest bundle must still verify after the genuine rivals, got: {e}"))?;
    println!("SDK54 - control: the honest bundle still verifies after the genuine-rival battery");

    println!("SDK54 - ✓ PASS: the count is unpaddable with the flat term 0 — the retired deposit-tx1 term, a conveyed flat backup, junk, duplicate-disclosure, never-co-signed and parentless entries are REJECTED, and so are GENUINELY CO-SIGNED rivals: csv:None, a state tying the live CSV, an extension tying the live CSV, and a timelocked rival rooted at F. Honest and renewed bundles verify.");
    Ok(())
}
