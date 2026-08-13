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

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
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
    assert!(
        !b.contains(".filter(|c| !crate::wallet::is_token_carrier(c, &carriers))"),
        "`deadline_safety_due` filters token carriers OUT of its unilateral route again. That leaves \
         the carrier lane with ZERO automatic deadline coverage — the one lane where the loss is an \
         ASSET — resting on `auto_exit_due` alone. The forced action here is the coin's OWN \
         pre-signed `T`, which does not move the allocation:\n\n{b}"
    );
    // …and it still SEVERES rather than re-anchoring. A carrier re-anchored is a carrier destroyed.
    assert!(
        b.contains("unilateral_exit("),
        "the forced action is no longer `unilateral_exit`. If this route ever re-anchors instead, it \
         destroys exactly the allocations it was extended to protect:\n\n{b}"
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
