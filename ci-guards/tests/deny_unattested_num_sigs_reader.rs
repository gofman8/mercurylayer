//! **[D40.2 / Stage 3] `num_sigs` may only enter this client through the attested reader.**
//!
//! `num_sigs` is the right-hand side of the receiver's anti-theft census
//! (`se_num_sigs == flat_backups + tiers + superseded`, exact equality). Unattested, a coordinator
//! that under-reports it by `k` hides `k` co-signed rival states and the census still balances
//! exactly — which is the one thing the census exists to make impossible.
//!
//! `get_statechain_info` is the only function that verifies the enclave's `utexo/sig_count/v2`
//! signature over `(statechain_id, num_sigs, sig_budget, nonce)` against the CHAIN-ANCHORED enclave
//! key, and refuses an unattested or half-stated response rather than defaulting it.
//!
//! **Today that single-reader property is a convention.** Nothing stops a new call site issuing its
//! own `GET /info/statechain/<id>` and reading the number raw — and the D8 hole was exactly that
//! shape, with the count travelling as a bare JSON integer nobody checked. A.1 made it worse-shaped
//! to lose: terminality is now DERIVED from the attested budget, so a second unattested reader would
//! reintroduce the hole one field over.
//!
//! So the convention becomes a check.

use std::path::PathBuf;

/// **[D64] Strip whole-line AND TRAILING `//` comments.**
///
/// Every stripper in this crate filtered only `l.trim_start().starts_with("//")`, so a TRAILING
/// comment survived — and a trailing comment is enough to defeat any substring pin in the file. An
/// adversarial pass proved it on the real tree with the mutation a guard printed in its own header:
///
/// ```ignore
/// println!( // was: return Err(anyhow::anyhow!(
///     "a COLOURED spine batch must not be driven through the PLAIN batch driver. …"
/// );
/// ```
///
/// The refusal is gone, the message is intact, and `arm.contains("return Err(")` is satisfied by the
/// ANNOTATION. The guard stayed green.
///
/// `//` inside a string literal must not be treated as a comment (URLs, `"http://…"`, and the
/// `"//"` in this very doc), so the scan tracks `"` and escapes.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let b = line.as_bytes();
        let (mut i, mut in_str, mut esc, mut cut) = (0usize, false, false, line.len());
        while i + 1 <= b.len() {
            let c = b[i];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
            } else if c == b'"' {
                in_str = true;
            } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
                cut = i;
                break;
            }
            i += 1;
        }
        out.push_str(line[..cut].trim_end());
        out.push('\n');
    }
    out
}


fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    strip_comments(src)
}

/// ONE FETCHER. Any other site building the `/info/statechain` path is fetching the count without
/// the attestation.
#[test]
fn only_the_attested_reader_fetches_statechain_info() {
    const CRATES: [&str; 2] = ["clients/libs/rust/src", "clients/libs/rust-sdk/src"];
    let mut hits: Vec<String> = Vec::new();
    for dir in CRATES {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(dir);
        for entry in std::fs::read_dir(&base).expect("crate src is readable") {
            let path = entry.expect("dir entry").path();
            if path.extension().map_or(true, |e| e != "rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let src = code_only(&std::fs::read_to_string(&path).expect("readable"));
            // The path literal the fetcher builds. `utils.rs` owns it; nobody else may.
            if src.contains("info/statechain/") && name != "utils.rs" {
                hits.push(format!("{dir}/{name}"));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "these files build the `/info/statechain` path themselves instead of going through \
         `utils::get_statechain_info`, which is the ONLY site that verifies the enclave's \
         utexo/sig_count/v2 signature over the count and budget. An unattested reader lets a \
         coordinator under-report `num_sigs` by k, hiding k co-signed rival states while the \
         exact-equality census still balances: {hits:?}"
    );
}

/// NON-VACUITY. The reader must still actually verify — otherwise the census above is a census over
/// a function that checks nothing.
#[test]
fn the_single_reader_still_verifies_and_still_refuses() {
    let code = code_only(&read("clients/libs/rust/src/utils.rs"));
    assert!(
        code.contains("verify_sig_count_attestation("),
        "`get_statechain_info` no longer verifies the enclave signature — the single-reader property \
         would then be guarding nothing"
    );
    // An unattested response must be refused, not accepted-with-a-warning.
    assert!(
        code.contains("NO enclave \\\n") || code.contains("with NO enclave"),
        "the unattested branch no longer refuses by name"
    );
    // …and the verifying key must be the PINNED enclave identity.
    //
    // **[D69] This assertion used to read `code.contains("&response.enclave_public_key")`** — the
    // coin's own chain-anchored server key. Sound for a coin that is ON CHAIN, and a depth-≥2
    // in-ladder-split ancestor's funding output is deliberately un-broadcast, so for those ancestors
    // the "anchor" was a field of the same response as the signature. `TRUST-MODEL` B11.
    assert!(
        code.contains("TesrParams::attestation_identity("),
        "`get_statechain_info` no longer resolves the PINNED enclave attestation identity. Every \
         other candidate for the verifying key is served by the coordinator in the same response as \
         the signature, which proves nothing — and for a deep in-ladder-split ancestor there is no \
         chain anchor to fall back to, by design ([D69], TRUST-MODEL B11)."
    );
}

/// **[D69] THE PIN MAY NOT COME FROM THE RESPONSE.** The whole point is that the verifying key is
/// independent of the party being checked; a call site that recovers "convenience" by passing a
/// served field re-opens B11 in one line, and it would look entirely reasonable in review.
///
/// This is an ABSENCE assertion over the argument list — the shape [D64] says a source scan may
/// make. It cannot prove the resolver's OUTPUT is what flows in (that would be a binding claim);
/// `the_single_reader_still_verifies_and_still_refuses` above pins the resolver's presence, and the
/// live suite exercises the path in both directions.
#[test]
fn no_call_site_verifies_against_a_key_the_coordinator_served() {
    const CRATES: [&str; 2] = ["clients/libs/rust/src", "clients/libs/rust-sdk/src"];
    // Served fields. Any of these reaching the verifier means the coordinator chose its own checker.
    const SERVED: [&str; 4] = [
        "response.enclave_public_key",
        "response.sig_count_attestation_pubkey",
        "info.enclave_public_key",
        "info.sig_count_attestation_pubkey",
    ];
    let mut hits: Vec<String> = Vec::new();
    for dir in CRATES {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(dir);
        for entry in std::fs::read_dir(&base).expect("crate src is readable") {
            let path = entry.expect("dir entry").path();
            if path.extension().map_or(true, |e| e != "rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let src = code_only(&std::fs::read_to_string(&path).expect("readable"));
            // The window is the call expression: from the callee to the closing `);`. A served field
            // anywhere else in the file (logging it, storing it) is not this defect.
            let mut rest = src.as_str();
            while let Some(at) = rest.find("verify_sig_count_attestation(") {
                let tail = &rest[at..];
                let end = tail.find(");").map(|e| e + 2).unwrap_or(tail.len());
                let call = &tail[..end];
                for served in SERVED {
                    if call.contains(served) {
                        hits.push(format!("{dir}/{name}: passes `{served}`"));
                    }
                }
                rest = &tail[end..];
            }
        }
    }
    assert!(
        hits.is_empty(),
        "these call sites verify the enclave's sig-count attestation against a key the COORDINATOR \
         SERVED in the same response as the signature. That is not a check — the signer picks the \
         verifier. Pass the pinned identity from `TesrParams::attestation_identity` ([D69], \
         TRUST-MODEL B11):\n{}",
        hits.join("\n")
    );
}

/// NON-VACUITY, by construction rather than by assertion: the detector is run against the code as
/// it stood BEFORE [D69], which is the defect this guard exists to keep out.
#[test]
fn the_detector_catches_the_pre_d69_call_site() {
    let historical = "\
        let ok = verify_sig_count_attestation(\n\
            &statechain_id,\n\
            response.num_sigs,\n\
            budget,\n\
            &nonce_hex,\n\
            &att,\n\
            &att_pubkey,\n\
            &response.enclave_public_key,\n\
        );\n";
    let src = code_only(historical);
    let at = src.find("verify_sig_count_attestation(").expect("callee is found");
    let tail = &src[at..];
    let end = tail.find(");").map(|e| e + 2).unwrap_or(tail.len());
    assert!(
        tail[..end].contains("response.enclave_public_key"),
        "the window no longer captures the argument list, so the rule above is a census of nothing"
    );
}

/// THE CONSUMER THAT MADE THIS WORTH PINNING. A.1 derives terminality from the attested budget, so a
/// second unattested reader would reintroduce the D8 hole one field over.
#[test]
fn terminality_is_derived_from_the_attested_payload() {
    let code = code_only(&read("clients/libs/rust/src/tesr.rs"));
    assert!(
        code.contains("async fn attested_terminal("),
        "`attested_terminal` is gone — if terminality went back to an unattested endpoint, this \
         guard is pinning the wrong property and must be re-derived rather than deleted"
    );
}
