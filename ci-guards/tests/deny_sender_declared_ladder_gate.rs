//! **[D40.2 / A.4] The JS clients' laddered gate must key on COORDINATOR-served evidence, not on
//! three fields the sender fills in.**
//!
//! Neither JS client can verify a TES-R ladder, so both refuse laddered coins and fall through to the
//! un-laddered census `num_sigs == backup_transactions.length`. That census is sound for an
//! un-laddered coin and worthless for a laddered one — a laddered coin's tiers each consume a
//! co-sign slot, so exact equality can only hold if `num_sigs` is under-reported.
//!
//! The gate deciding which world you are in read `protocol_version`, `tesr_ladder` and
//! `child_tesr_bundle`. **All three are written by the sender.** A sender declaring version 0 with
//! both fields omitted fell straight through to a bare equality against an integer these clients
//! never authenticate — a plain HTTP-response edit, no seed, no database write. That made these the
//! CHEAPEST route in the whole trust model, and the corpus had them filed as *exempt* ("they refuse
//! laddered coins outright").
//!
//! It is also the identical shape the Rust receiver already had to close with
//! `MIN_PREPAY_PROTOCOL_VERSION`: a version floor a sender can duck by declaring a lower version.
//!
//! # What replaced it
//!
//! `statechainInfo` is served by the coordinator and keyed by statechain id, so it is not
//! sender-controlled. An `sig_count_attestation` field means the enclave signs `utexo/sig_count/v2`
//! over `(statechain_id, num_sigs, sig_budget, nonce)` — i.e. this deployment runs laddered coins and
//! the count is only as good as a signature THESE CLIENTS CANNOT VERIFY. So they refuse.
//!
//! **This does not make them conformant.** It makes them fail closed for a reason they can check.
//! The real fix is porting the attestation verification; until then they are non-conformant receivers
//! by design rather than by accident, and the trust model says so.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

const CLIENTS: [&str; 2] = [
    "clients/libs/nodejs/transfer_receive.js",
    "clients/libs/web/transfer_receive.js",
];

/// THE STRUCTURAL GATE, in both clients.
#[test]
fn both_js_clients_gate_on_coordinator_served_evidence() {
    for c in CLIENTS {
        let src = read(c);
        assert!(
            src.contains("statechainInfo.sig_count_attestation"),
            "{c} does not gate on the coordinator-served attestation field. Without it the only gate \
             is three SENDER-declared fields, and a sender declaring protocol_version 0 with \
             tesr_ladder and child_tesr_bundle omitted falls through to a bare equality against an \
             unauthenticated num_sigs — the cheapest route in the trust model"
        );
        // It must REFUSE on presence, not merely notice it.
        let at = src.find("statechainInfo.sig_count_attestation").unwrap();
        let window = &src[at..src.len().min(at + 800)];
        assert!(
            window.contains("throw new Error("),
            "{c} reads the attestation field but does not refuse on it:\n\n{window}"
        );
    }
}

/// THE ORDER. The structural refusal must come BEFORE the census it protects — a check that runs
/// after the thing it guards is decoration.
#[test]
fn the_structural_gate_precedes_the_flat_census() {
    for c in CLIENTS {
        let src = read(c);
        let gate = src
            .find("statechainInfo.sig_count_attestation")
            .unwrap_or_else(|| panic!("{c}: gate is gone"));
        let census = src
            .find("num_sigs != transferMsg.backup_transactions.length")
            .unwrap_or_else(|| panic!("{c}: the flat census is gone — this guard has lost its subject"));
        assert!(
            gate < census,
            "{c}: the attestation refusal runs AFTER the flat census it exists to protect. The \
             census would already have accepted the coin."
        );
    }
}

/// NON-VACUITY: the field name must be the real wire name. `StatechainInfoResponsePayload` carries no
/// `serde(rename)` on it, so the JSON key is the Rust field name — if that ever gains a rename, this
/// gate silently reads `undefined` and stops refusing anything.
#[test]
fn the_wire_field_name_is_still_what_the_js_reads() {
    let lib = read("lib/src/transfer/receiver.rs");
    let at = lib
        .find("pub sig_count_attestation: Option<String>")
        .expect("the attestation field is gone from the served payload");
    // Look back a few lines for a rename attribute on this field.
    let before = &lib[at.saturating_sub(200)..at];
    assert!(
        !before.contains("serde(rename"),
        "`sig_count_attestation` has acquired a serde(rename), so the JSON key is no longer the \
         field name — both JS clients now read `undefined` and their structural gate has silently \
         stopped refusing anything:\n\n{before}"
    );
}
