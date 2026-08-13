//! **[D40.2] Terminality must come from the ENCLAVE'S SIGNATURE, not from the coordinator's word.**
//!
//! The attestation existed and was verified. It was then read by nothing.
//!
//! `get_statechain_info` verifies `utexo/sig_count/v2` over `(statechain_id, num_sigs, sig_budget,
//! nonce)` against the chain-anchored enclave key, and refuses a `has_sig_budget: None` rather than
//! defaulting it. `verify_conveyed_child` fetched that payload — and then took terminality from
//! `GET /statechain/spend_budget`, which the coordinator computes entirely from its own Postgres. A
//! repo-wide grep for `has_sig_budget` in `clients/` returned **one** site: the verification itself.
//!
//! So the receiver's most load-bearing acceptance input was an unauthenticated integer, on both the
//! claim path and the SSP's pre-pay census, and one `terminal: true` retires a whole parent tree at
//! once. That is the same shape as D8's original `sig_count` hole, one field over: the work of
//! attesting was done and the consumer was never repointed.
//!
//! **This does not close CO-1** — the enclave key material and the counter it attests are held by
//! the same party the receiver is being protected from. It closes the gap between what the enclave
//! signed and what the client read.

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

/// The body of the item starting at `sig`, bounded by the next COLUMN-0 item.
///
/// Comment lines are already gone, so a `\n/// ` delimiter would never match and every scan would
/// silently fall back to a fixed byte count — long enough to look like a body, short enough to miss
/// the call sites at the end of a long function. That is how a source-scanning test lies. Bound it
/// by something that actually exists in stripped source.
fn item_body<'a>(code: &'a str, sig: &str) -> &'a str {
    let at = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let rest = &code[at + sig.len()..];
    // `pub(crate) fn` must be in this list: without it, `terminal_from_attested`'s window runs on
    // past `cross_check_terminality` and every assertion below could be satisfied by the WRONG
    // function's text — a scan window that overshoots is the same lie as one that includes itself.
    let end = ["\npub async fn ", "\npub(crate) fn ", "\npub fn ", "\nasync fn ", "\nfn ", "\npub struct ", "\nimpl "]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
    let body = &code[at..at + sig.len() + end];
    assert!(body.len() > 300, "`{sig}`'s body scanned to {} bytes — that is not a function", body.len());
    body
}

fn tesr() -> String {
    code_only(&read("clients/libs/rust/src/tesr.rs"))
}

/// THE DERIVATION, read off the code: terminality is a function of the ATTESTED fields.
///
/// [Stage 3] The decision moved out of `attested_terminal` into `terminal_from_attested` so a test
/// could drive a malicious coordinator through it without a network. The guard follows it, and
/// additionally pins that `attested_terminal` still CALLS the extracted pieces — a split whose
/// halves are correct but unwired is the same hole with more code.
#[test]
fn terminality_is_derived_from_the_attested_budget() {
    let code = tesr();
    let wiring = item_body(&code, "async fn attested_terminal(");
    assert!(
        wiring.contains("terminal_from_attested(") && wiring.contains("cross_check_terminality("),
        "`attested_terminal` no longer calls both halves of the rule. Extracting them and then not \
         calling one is the original hole with extra steps:\n\n{wiring}"
    );
    let body = item_body(&code, "pub(crate) fn terminal_from_attested(");

    assert!(
        body.contains("num_sigs >= budget"),
        "terminality is no longer `num_sigs >= budget` over the attested payload:\n\n{body}"
    );
    assert!(
        body.contains("(Some(true), Some(budget))") && body.contains("(Some(false), _) => Ok(false)"),
        "the two attested shapes are no longer distinguished. `Some(false)` is the enclave saying \
         `no budget`; it is not the same as a missing field:\n\n{body}"
    );
    // "Cannot say" must never resolve to "not terminal" — that is the permissive direction.
    // Pinned as the ARM producing an Err, not as the keyword `return`: the same refusal written as
    // a match-arm expression is the same rule, and a guard that fails on the rewrite is pinning
    // spelling. The BEHAVIOUR is exercised by
    // `tesr::malicious_coordinator_terminality_tests::an_omitted_budget_is_refused_not_defaulted`,
    // which drives all three unanswered shapes through the real function.
    assert!(
        body.contains("_ => Err(") && body.contains("cannot say"),
        "a missing attested budget no longer REFUSES. Reading silence as `not terminal` is the \
         permissive direction, and terminality is what stops a parent being spent out from under \
         the child being claimed:\n\n{body}"
    );
}

/// THE DEMOTION. The coordinator's answer may still be consulted — as a cross-check that refuses on
/// disagreement, never as the source.
#[test]
fn the_coordinator_answer_is_a_cross_check_that_refuses() {
    let code = tesr();
    // The fetch stays in the wiring; the comparison moved into `cross_check_terminality`.
    let wiring = item_body(&code, "async fn attested_terminal(");
    let body = item_body(&code, "pub(crate) fn cross_check_terminality(");

    assert!(
        body.contains("coordinator_says != attested"),
        "the coordinator's answer is no longer compared against the attested derivation. Dropping \
         the comparison loses the only signal that one of the two stores was written behind the \
         other's back:\n\n{body}"
    );
    assert!(
        wiring.contains("get_spend_budget(cc, statechain_id)"),
        "the cross-check no longer fetches the coordinator's record at all:\n\n{body}"
    );
}

/// NO ACCEPTANCE PATH MAY STILL TAKE TERMINALITY FROM THE UNATTESTED ENDPOINT.
///
/// `get_spend_budget` has legitimate non-acceptance callers — a sender setting its own budget, a
/// local diagnostic — so this is a census of the sites that FEED A VERIFIER, not a ban on the
/// function.
#[test]
fn no_verifier_reads_terminality_from_the_unattested_endpoint() {
    let code = tesr();

    // The acceptance path is `verify_conveyed_child`. Inside it, terminality must come from the
    // attested derivation for the parent AND for every intermediate ancestor segment.
    let body = item_body(&code, "pub async fn verify_conveyed_child(");

    assert!(
        !body.contains("get_spend_budget("),
        "`verify_conveyed_child` reads `/statechain/spend_budget` directly again. That endpoint is \
         computed from the coordinator's own Postgres and carries no enclave signature; one \
         `terminal: true` from it retires a whole parent tree:\n\n{body}"
    );
    assert!(
        body.matches("attested_terminal(").count() >= 2,
        "the parent AND every intermediate ancestor segment must take terminality from the attested \
         derivation — an ancestor left on the unattested read is the same hole one level up:\n\n{body}"
    );
}

/// **[D54] …AND THE CENSUS COVERS EVERY FILE THAT REACHES A VERIFIER, NOT ONE FUNCTION.**
///
/// The test above reads `verify_conveyed_child`, in `tesr.rs`, and nothing else. Its own module
/// header says the hole was live "on both the claim path and the SSP's pre-pay census" — and
/// `verify_terminal_parents`, in a DIFFERENT FILE, was still reading
///
/// ```ignore
/// let terminal = v.get("terminal").and_then(|t| t.as_bool()).unwrap_or(false);
/// ```
///
/// straight off `GET /statechain/spend_budget/{id}`, with two verifier call sites: `claim()` and the
/// SSP pre-payment gate that authorises an irreversible Lightning leg. That is the lane a DEFAULT
/// wallet uses — `colored_ladder` ships false, so branch-funded sub-coins are what wallets receive.
///
/// **A guard scoped to one function certified a property of the whole receiver.** It stayed green
/// for as long as the hole existed, and it named the very lane it could not see.
///
/// So this census is over FILES: no file that reaches a verifier may parse a `terminal` field out of
/// an HTTP body. The attested derivation and its deliberate cross-check both live behind
/// `attested_terminal`, which is the one place allowed to consult the coordinator's copy.
#[test]
fn no_file_that_reaches_a_verifier_parses_terminality_out_of_an_http_body() {
    const REACHES_A_VERIFIER: [&str; 3] = [
        "clients/libs/rust/src/transfer_receiver.rs",
        "clients/libs/rust/src/tesr.rs",
        "clients/libs/rust-sdk/src/tokens.rs",
    ];
    // The shape of the defect: pulling `terminal` out of a deserialised response.
    const RAW_READS: [&str; 3] = [
        "\"terminal\").and_then",
        "get(\"terminal\")",
        "/statechain/spend_budget/",
    ];
    let mut offenders = Vec::new();
    for f in REACHES_A_VERIFIER {
        let code = code_only(&read(f));
        for needle in RAW_READS {
            if code.contains(needle) {
                offenders.push(format!("{f}: contains `{needle}`"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a file on a verifier path reads terminality out of an HTTP body again. That endpoint is \
         the coordinator's own Postgres with no enclave signature, and one `terminal: true` from it \
         retires a whole parent tree — accepted by `claim()` AND paid out over by the SSP pre-pay \
         gate. Route it through `attested_terminal`, which derives terminality from the enclave's \
         signed `num_sigs`/`sig_budget` and keeps the coordinator's answer as a CROSS-CHECK \
         only.\n\n{}",
        offenders.join("\n")
    );
}

/// NON-VACUITY for the file census. A scan over a list of files is the exact shape that goes
/// vacuous when a path is renamed — the file simply stops existing and the loop finds nothing.
#[test]
fn the_file_census_reads_real_files_and_would_catch_the_original_defect() {
    for f in [
        "clients/libs/rust/src/transfer_receiver.rs",
        "clients/libs/rust/src/tesr.rs",
        "clients/libs/rust-sdk/src/tokens.rs",
    ] {
        let code = code_only(&read(f));
        assert!(code.len() > 10_000, "{f} scanned to {} bytes — that is not the file", code.len());
    }
    // The literal the defect was written in, as it stood before D54.
    let defect = r#"let terminal = v.get("terminal").and_then(|t| t.as_bool()).unwrap_or(false);"#;
    assert!(
        defect.contains(r##"get("terminal")"##),
        "the detector no longer recognises the shape it exists to catch"
    );
}

/// NON-VACUITY: the attested fields the derivation depends on must still be verified upstream, and
/// the refusal for a missing one must still be there. Without this, the census above could pass over
/// a payload nobody checked.
#[test]
fn the_attestation_is_still_verified_upstream() {
    let code = code_only(&read("clients/libs/rust/src/utils.rs"));
    assert!(
        code.contains("verify_sig_count_attestation("),
        "`get_statechain_info` no longer verifies the enclave's signature over the count and budget \
         — the derivation downstream would then be arithmetic over unauthenticated numbers"
    );
    assert!(
        code.contains("response.has_sig_budget") && code.contains("Some(false) => None"),
        "the attested-budget shape is no longer distinguished at the point of verification"
    );
}
