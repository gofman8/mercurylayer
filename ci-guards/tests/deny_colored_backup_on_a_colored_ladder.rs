//! **[D35 / RGB-1] A coloured ladder's flat backups must be plain, and BOTH acceptance paths must
//! say so.** [P1 corrected]
//!
//! The defect this guards was not a missing check — it was a check that served two lanes with
//! opposite requirements and therefore enforced the union of their shapes:
//!
//! * `verify_if_locktime_is_reasonable_tx_version_and_output_size` admits `op_return_outputs <= 1`
//!   on every flat backup, because on the un-laddered carrier lane the coloured backup IS the exit
//!   material. Correct there, and it must stay.
//! * On a COLOURED ladder a flat backup is a HOP backup over `F` that any ancestor may still hold.
//!   An opret on one commits to an assignment nothing verifies, so a prior owner's 112-vB spend of
//!   `F` stops being griefing (they gain nothing) and becomes capture (they take the allocation) —
//!   which also breaks the "voiding the tree is unprofitable" premise the economics lean on.
//!
//! The pre-correction predicate keyed on "a ladder is present" and asserted PLAIN *in its prose*
//! without ever reading `is_colored()`. So it missed the coloured lane's real defect and refused the
//! coloured lane's legitimate shape — a coloured ladder whose carrier envelope rides on a flat row,
//! which is exactly what an uncolourable legacy piece becomes once `accept_ladder` colours it. It
//! refused the rescue lane at its final hop, and the refusal message told the user the ladder was
//! PLAIN while the ladder was coloured.
//!
//! **What is pinned here is the CONSTRUCTION, not the description.** Every assertion below reads
//! comment-stripped source, because the last four times a pin like this went green while the code
//! was wrong, it matched a sentence I had written about the code. A refusal message is not evidence
//! that the refusal happens.

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


fn repo(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// Source with every whole-line `//` comment removed. Doc comments are prose about the code; a pin
/// that can be satisfied by prose pins nothing.
fn code_only(src: &str) -> String {
    strip_comments(src)
}

fn fn_body(code: &str, sig: &str) -> String {
    let at = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let rest = &code[at..];
    // Up to the next item at column 0 — enough to contain one function.
    let end = rest[1..].find("\npub fn ").map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// THE RULE ITSELF, read off the code. Both arms must exist and they must be keyed on the lane —
/// an implementation that refuses an opret unconditionally would break the un-laddered carrier, and
/// one that never reads `is_colored()` is the pre-correction shape all over again.
#[test]
fn the_lane_rule_reads_the_lane_and_reads_the_backup_outputs() {
    let code = code_only(&repo("clients/libs/rust/src/tesr.rs"));
    let body = fn_body(&code, "pub fn verify_flat_backup_lane(");

    assert!(
        body.contains("bundle.is_colored()"),
        "`verify_flat_backup_lane` does not read the LANE. Keyed on anything else it enforces the \
         union of two lanes' shapes, which is the defect it replaced:\n\n{body}"
    );
    assert!(
        body.contains("is_op_return()"),
        "`verify_flat_backup_lane` never inspects the backup transactions' OUTPUTS. The theft \
         primitive is an opret in the backup TX, not the `rgb_consignment` metadata field — a \
         predicate that only reads the field is blind to it:\n\n{body}"
    );
    assert!(
        body.contains("deserialize(&raw)") && body.contains("does not parse"),
        "the predicate must PARSE every backup and refuse the ones it cannot read. A backup that \
         fails to deserialize must not be skipped as `nothing to see`:\n\n{body}"
    );
    // The plain arm keeps the older, still-correct refusal — the envelope on a plain ladder.
    assert!(
        body.contains("!colored && b.rgb_consignment.is_some()"),
        "the PLAIN arm no longer refuses an RGB consignment. A plain ladder's tiers carry no \
         transition, so its exit BURNS the allocation — that half of the old rule was right and \
         must survive the correction:\n\n{body}"
    );
}

/// BOTH acceptance paths. R′ is the claim path AND the SSP's pre-payment census; the pre-pay one is
/// the path that authorises an irreversible Lightning leg, so a rule enforced on only one of them
/// leaves the payer holding the coin an ancestor can take.
#[test]
fn both_acceptance_paths_call_it() {
    let code = code_only(&repo("clients/libs/rust/src/transfer_receiver.rs"));
    let calls = code.matches("verify_flat_backup_lane(").count();
    assert!(
        calls >= 2,
        "`verify_flat_backup_lane` has {calls} call site(s) in transfer_receiver.rs — R′ is the \
         claim path AND `prepay_flat_census`, and both must run it"
    );
    assert!(
        code.contains("crate::tesr::verify_flat_backup_lane(&bundle, &transfer_msg.backup_transactions)"),
        "the CLAIM path must run the lane rule over the conveyed backup vector"
    );
    assert!(
        code.contains("crate::tesr::verify_flat_backup_lane(&bundle, backup_group)"),
        "the PRE-PAY census must run the lane rule over the group it just structurally validated"
    );
}

/// THE UNION RULE IS GONE, not merely bypassed. Leaving the old predicate in place would let a
/// later reader restore the call and re-break the coloured lane's legitimate shape.
#[test]
fn the_union_keyed_predicate_no_longer_exists() {
    let src = repo("clients/libs/rust/src/transfer_receiver.rs");
    assert!(
        !src.contains("first_rgb_envelope_on_a_laddered_message"),
        "the pre-correction predicate is still present. It keys on `a ladder is present` and \
         refuses the coloured lane's legitimate carrier-envelope shape — the last hop of the \
         uncolourable-carrier rescue (sdk78)"
    );
}

/// THE PERMISSION THAT MUST STAY. `op_return_outputs <= 1` in mercurylib is what lets the
/// un-laddered carrier lane convey its coloured backup at all. Tightening it there would close the
/// legacy token lane while looking like a hardening.
#[test]
fn the_unladdered_carrier_lane_keeps_its_permission() {
    let code = code_only(&repo("lib/src/transfer/receiver.rs"));
    assert!(
        code.contains("if op_return_outputs > 1 {"),
        "the flat-backup shape check no longer admits exactly one OP_RETURN. On the un-laddered \
         carrier lane the coloured backup IS the exit material; the lane-specific refusal belongs \
         in `verify_flat_backup_lane`, not here"
    );
}
