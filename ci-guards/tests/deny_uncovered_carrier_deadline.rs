//! **[D46 / decision 4] The carrier lane must not be the one lane with no automatic deadline
//! coverage.**
//!
//! `deadline_safety_due` has two routes. The COOPERATIVE one (`auto_refresh_due`) excludes token
//! carriers for a real reason: a plain re-anchor spends the carrier's funding outpoint into a fresh
//! aggregate and destroys its RGB allocation. That exclusion is correct and must stay.
//!
//! The UNILATERAL route excluded them too — and there the exclusion had no such justification. It
//! left an RGB carrier's `min(L_k)` resting on `auto_exit_due` alone, in the one lane where the loss
//! is an ASSET rather than sats.
//!
//! The forced action is `unilateral_exit`, which broadcasts the coin's own pre-signed `T`. That is a
//! tier of the coin's own ladder carrying the coin's own state — it does not re-aggregate and does
//! not move the allocation, which is exactly why it is safe on a carrier where a re-anchor is not.
//!
//! `sdk86` measured the calendar this protects: the flat backup chain's absolute locktime is finite,
//! mining moves the tip toward it, and each whole-coin hop spends `interval` of it. INV-27's "idle
//! coins never age" is true of the CSV side only.

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

fn body(code: &str, sig: &str) -> String {
    let at = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let rest = &code[at + sig.len()..];
    let end = ["\n    pub async fn ", "\n    pub fn ", "\n    async fn ", "\n    fn "]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
    let b = code[at..at + sig.len() + end].to_string();
    assert!(b.len() > 500, "`{sig}` scanned to {} bytes — that is not a function body", b.len());
    b
}

/// THE FIX: the unilateral route does NOT filter carriers out.
#[test]
fn the_unilateral_deadline_route_covers_carriers() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    let b = body(&code, "pub async fn deadline_safety_due(");
    // [D51] The ORIGINAL form of this assertion pinned the absence of ONE literal spelling of the
    // filter. The pre-spec review named it correctly as the description-pin shape: an
    // `if is_token_carrier(c, &carriers) { continue }`, or the same closure with a different
    // binding, re-introduces the exclusion and the guard still passes. It now names every spelling
    // that would exclude carriers from the set this route acts on, and the check is scoped to the
    // `still_due` binding rather than the whole function — `auto_refresh_due`'s own (correct)
    // exclusion is called from in here and must not trip it.
    let still_due = b
        .split_once("let still_due:")
        .and_then(|(_, rest)| rest.split_once(".collect();"))
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| {
            panic!("the `still_due` binding is gone from `deadline_safety_due` — this guard is \
                    reading the wrong thing and must be re-pointed before it can mean anything:\n\n{b}")
        });
    assert!(
        !still_due.contains("is_token_carrier"),
        "`deadline_safety_due` filters token carriers OUT of the set its unilateral route acts on. \
         The forced action here is the coin's OWN pre-signed `T`, which carries the coin's own state \
         and does not move the allocation, so a carrier belongs in this set — a carrier that cannot \
         take it is refused BY `unilateral_exit`, on the merits, and [D51] reports that refusal. \
         Excluding it here hides the coin instead:\n\n{still_due}"
    );
    // …and it still SEVERES rather than re-anchoring. A carrier re-anchored is a carrier destroyed.
    // [D67] Either spelling of the sever. `sever_from_f` IS `unilateral_exit` on one coin, and the
    // pass now routes through the NAMED remedy so the doc's "it is also what `deadline_safety_due`
    // falls back to" is true of the symbol too. What must never appear here is a RE-ANCHOR.
    assert!(
        b.contains("sever_from_f(") || b.contains("unilateral_exit("),
        "the forced action is neither `sever_from_f` nor `unilateral_exit`. If this route ever \
         re-anchors instead, it destroys exactly the allocations it was extended to protect:\n\n{b}"
    );
}

/// **[D51] AND THE PASS MAY NOT RETURN `Ok` OVER A COIN IT DID NOT DEFEND.**
///
/// This is the half that was missing while the guard above was green. Carriers WERE included in the
/// set — the assertion above held — and the `unilateral_exit` refusal for a non-coloured carrier then
/// landed on a bare `_ => continue`, so the pass reported success over an undefended coin while the
/// operator line promised those same carriers "will be SEVERED". Inclusion without reporting is not
/// coverage.
#[test]
fn the_deadline_pass_reports_every_coin_it_could_not_defend() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    let b = body(&code, "pub async fn deadline_safety_due(");
    assert!(
        b.contains("undefended"),
        "`deadline_safety_due` no longer collects the coins it failed to defend. Without that list \
         a refusal from `unilateral_exit` — the DEFAULT outcome for every carrier, since \
         `colored_ladder` ships false — is indistinguishable from a quiet pass:\n\n{b}"
    );
    assert!(
        b.contains("Err(e) => undefended.push"),
        "the `Err` arm no longer records its reason. `_ => continue` over an Err about a coin inside \
         its deadline margin is the silent-degradation shape this repo has been bitten by three \
         times — the failure looks like idle:\n\n{b}"
    );
    assert!(
        b.contains("ExitDeadlineApproaching"),
        "the undefended coins are no longer announced on the event channel, which is the only \
         signal a caller that discards the return value (the background loop does exactly that) can \
         ever see:\n\n{b}"
    );
    assert!(
        b.contains("return Err(anyhow!(") && b.contains("could NOT defend them"),
        "the pass no longer RETURNS an error when it left a coin undefended. `Ok` from this pass is \
         read as \"this wallet is protected\" — by `note_watchtower_ok` and by every direct caller — \
         so returning it over an undefended coin is a false green on a safety pass:\n\n{b}"
    );
}

/// THE EXCLUSION THAT MUST SURVIVE. The cooperative route's carrier filter is not the same mistake
/// and removing it would be a real one.
#[test]
fn the_cooperative_route_still_excludes_carriers() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    let b = body(&code, "pub async fn auto_refresh_due(");
    assert!(
        b.contains("is_token_carrier("),
        "`auto_refresh_due` no longer excludes token carriers. A plain re-anchor spends the \
         carrier's outpoint into a fresh aggregate and DESTROYS its RGB allocation — this exclusion \
         is correct and is not the one D46 removed:\n\n{b}"
    );
}

/// NON-VACUITY: the BLIND propagation must survive too. If the carrier set is unreadable, neither
/// remedy can tell which coins it would destroy, so the pass refuses rather than guessing — and now
/// that carriers are IN the unilateral route, guessing would be worse than before.
#[test]
fn a_blind_wallet_still_refuses_rather_than_guessing() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    let b = body(&code, "pub async fn deadline_safety_due(");
    assert!(
        b.contains("BLIND"),
        "the BLIND propagation is gone. With carriers now IN the unilateral route, a wallet that \
         cannot tell a carrier from a plain coin must refuse — not sever blindly:\n\n{b}"
    );
}
