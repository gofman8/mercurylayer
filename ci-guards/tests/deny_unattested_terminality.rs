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
    let end = ["\npub async fn ", "\npub fn ", "\nasync fn ", "\nfn ", "\npub struct ", "\nimpl "]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
    let body = &code[at..at + sig.len() + end];
    assert!(body.len() > 400, "`{sig}`'s body scanned to {} bytes — that is not a function", body.len());
    body
}

fn tesr() -> String {
    code_only(&read("clients/libs/rust/src/tesr.rs"))
}

/// THE DERIVATION, read off the code: terminality is a function of the ATTESTED fields.
#[test]
fn terminality_is_derived_from_the_attested_budget() {
    let code = tesr();
    let body = item_body(&code, "async fn attested_terminal(");

    assert!(
        body.contains("info.num_sigs >= budget"),
        "terminality is no longer `num_sigs >= budget` over the attested payload:\n\n{body}"
    );
    assert!(
        body.contains("(Some(true), Some(budget))") && body.contains("(Some(false), _) => false"),
        "the two attested shapes are no longer distinguished. `Some(false)` is the enclave saying \
         `no budget`; it is not the same as a missing field:\n\n{body}"
    );
    // "Cannot say" must never resolve to "not terminal" — that is the permissive direction.
    assert!(
        body.contains("return Err(") && body.contains("cannot say"),
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
    let body = item_body(&code, "async fn attested_terminal(");

    assert!(
        body.contains("coordinator_says != terminal"),
        "the coordinator's answer is no longer compared against the attested derivation. Dropping \
         the comparison loses the only signal that one of the two stores was written behind the \
         other's back:\n\n{body}"
    );
    assert!(
        body.contains("get_spend_budget(cc, statechain_id)"),
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
