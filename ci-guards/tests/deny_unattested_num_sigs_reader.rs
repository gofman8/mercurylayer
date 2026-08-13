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

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
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
    // …and the verifying key must be the CHAIN-ANCHORED one, not the served attestation pubkey —
    // verifying against a key the coordinator also supplies proves nothing.
    assert!(
        code.contains("&response.enclave_public_key"),
        "the attestation is no longer verified against the chain-anchored `enclave_public_key`. \
         Verifying against the SERVED attestation pubkey instead would accept a coordinator signing \
         with a key of its own, which is precisely the attack."
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
