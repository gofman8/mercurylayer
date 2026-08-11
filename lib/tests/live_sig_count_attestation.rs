//! A REAL attestation, fetched from the running lockbox and checked by the shipping verifier.
//!
//! The unit tests in `receiver.rs` generate their own signatures, so they agree with themselves by
//! construction — they all passed while the live endpoint's output could not even be parsed, because
//! the lockbox prefixes every key with `0x` and the tests built their hex without it. That class of
//! bug is only visible against the real thing, which is what this file is for.
//!
//! It FETCHES rather than embedding a captured vector. A hard-coded signature goes stale the moment
//! the coin is co-signed again or the preimage version changes, and a stale vector in a security test
//! is worse than none: it gets "fixed" by editing the expectation. Fetching means the test either
//! exercises the live SE or says plainly that it did not.

use std::process::Command;

const DEFAULT_LOCKBOX: &str = "http://127.0.0.1:18080";
/// A 32-byte challenge. Fixed rather than random so a failure is reproducible; the nonce's job here
/// is to be *bound into the signature*, not to be unpredictable to the SE.
const NONCE: &str = "abababababababababababababababababababababababababababababababab";

fn get(url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-s", "--show-error", "--max-time", "5", url])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let body = String::from_utf8(out.stdout).map_err(|e| format!("not UTF-8: {e}"))?;
    if body.trim().is_empty() {
        return Err("empty response".to_string());
    }
    Ok(body)
}

fn json_str(body: &str, key: &str) -> Option<String> {
    let at = body.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = body[at..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

fn json_num(body: &str, key: &str) -> Option<u32> {
    let at = body.find(&format!("\"{key}\""))? + key.len() + 2;
    body[at..]
        .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

fn json_bool(body: &str, key: &str) -> Option<bool> {
    let at = body.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = body[at..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Pick any coin the lockbox knows about. The test is about the ATTESTATION, not about a particular
/// coin, so the caller may name one via `LIVE_STATECHAIN_ID`; otherwise we try the id recorded when
/// this test was first written and skip if it is gone.
fn statechain_id() -> String {
    std::env::var("LIVE_STATECHAIN_ID")
        .unwrap_or_else(|_| "5eebee706045497daf9b8972c9aa4857".to_string())
}

#[test]
fn the_live_se_attestation_verifies_and_covers_both_count_and_budget() {
    let base = std::env::var("LOCKBOX_URL").unwrap_or_else(|_| DEFAULT_LOCKBOX.to_string());
    let sid = statechain_id();

    let body = match get(&format!("{base}/signature_count/{sid}?nonce={NONCE}")) {
        Ok(b) => b,
        Err(why) => {
            eprintln!(
                "SKIP: no lockbox answered at {base} ({why}). This test only proves something \
                 against a running SE; set LOCKBOX_URL / LIVE_STATECHAIN_ID to point it elsewhere."
            );
            return;
        }
    };

    let (Some(sig), Some(pk)) = (json_str(&body, "attestation"), json_str(&body, "attestation_pubkey"))
    else {
        eprintln!(
            "SKIP: the lockbox at {base} did not attest statechain id {sid} — it may not know this \
             coin. Response: {body}"
        );
        return;
    };

    let count = json_num(&body, "sig_count").unwrap_or_else(|| panic!("no sig_count in {body}"));
    // [D8(i)] `has_sig_budget` is the field that distinguishes "no budget" from "budget 0". An SE
    // that does not report it is one that predates this change, and that must FAIL rather than be
    // read as the permissive case — the whole point is that terminality can be checked.
    let has_budget = json_bool(&body, "has_sig_budget").unwrap_or_else(|| {
        panic!(
            "the live SE reported no `has_sig_budget` for {sid}. It predates D8(i), so terminality \
             cannot be verified against it. Response: {body}"
        )
    });
    let budget = if has_budget { Some(json_num(&body, "sig_budget").expect("sig_budget")) } else { None };

    // The anchored key is a 33-byte COMPRESSED pubkey (that is what `enclave_public_key` is, and
    // what `validate_tx0_output_pubkey` binds to the chain); the attestation carries its 32-byte
    // x-only half. Passing the x-only where the compressed one belongs fails to PARSE, which the
    // verifier reports as the same "invalid" as a bad signature — so it looked exactly like a
    // preimage mismatch when this test was first written. Re-add the prefix.
    //
    // `02` regardless of the real parity is deliberate and sound HERE: the check is
    // `anchored.x_only_public_key() == claimed_xonly`, which discards parity, and the unit test
    // `a_coordinator_signing_with_its_own_key_is_refused` covers the binding itself. What this test
    // is for is the LIVE signature over the LIVE preimage.
    let anchored = format!("02{}", pk.trim_start_matches("0x"));
    let ok = mercurylib::transfer::receiver::verify_sig_count_attestation(
        &sid, count, budget, NONCE, &sig, &pk, &anchored,
    );
    assert!(
        ok.is_ok(),
        "the LIVE SE attestation over count={count} budget={budget:?} must verify: {ok:?}\n{body}"
    );
    eprintln!("live SE attested {sid}: sig_count={count} budget={budget:?} — verified");

    // Every neighbouring claim must fail. An attestation that vouched for more than the one
    // (count, budget) pair it was made over would be worth nothing.
    let neighbours = [
        (count.wrapping_add(1), budget),
        (count.wrapping_sub(1), budget),
        (count, Some(budget.unwrap_or(0).wrapping_add(1))),
        (count, if budget.is_some() { None } else { Some(0) }),
    ];
    for (c, b) in neighbours {
        if (c, b) == (count, budget) {
            continue;
        }
        assert!(
            mercurylib::transfer::receiver::verify_sig_count_attestation(
                &sid, c, b, NONCE, &sig, &pk, &anchored
            )
            .is_err(),
            "the live attestation must NOT vouch for count={c} budget={b:?}"
        );
    }
}
