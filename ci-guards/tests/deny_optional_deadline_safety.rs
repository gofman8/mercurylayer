//! **[D40 / A.2] The deadline defence is UNCONDITIONAL. Routine re-anchoring is not.**
//!
//! A whole laddered coin carries exactly one absolute clock: `min(L_k)` over its flat backup chain,
//! and that chain is held by its PRIOR OWNERS. Every tier below `F` is *relative*-timelocked, so
//! none of them can out-race a transaction that is simply valid now. When `L_k` passes, any
//! ancestor's matured rung spends `F` and takes the coin.
//!
//! # How it came to be optional
//!
//! `auto_refresh_due` did two unrelated jobs under one name:
//!
//! * **routine background re-anchoring** — an ECONOMICS choice. B4 folds the re-anchor cost into
//!   `transfer` and pays it on demand, so a running wallet must not silently shrink a balance in the
//!   background. Correctly opt-in, correctly off by default.
//! * **the deadline defence** — a SAFETY property.
//!
//! Both sat behind `auto_refresh && background_auto_refresh`. Turning the economics off turned the
//! safety off with it, and the comment at the call site asserted the gap was covered elsewhere —
//! *"deadline safety for idle wallets is the `auto_exit` pass below"* — which is false.
//! `auto_exit_due` protects sub-coins and materialises carriers; the whole-coin clock had **no
//! scheduled defender at all** on a default wallet.
//!
//! # And why the fallback is load-bearing
//!
//! Re-anchoring is COOPERATIVE — one fresh SE co-signature. Under D40.1 the party most interested in
//! this deadline passing is the operator, i.e. the same party being asked to sign. **A defence its
//! adversary can decline is not a defence.** So the pass falls back to severing from `F`: broadcast
//! the already-co-signed trigger, which is un-timelocked and therefore wins against every retained
//! rung by being valid first, with no SE and no counterparty.

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

/// The body of the item starting at `sig`, bounded by the next COLUMN-4 `pub`/`fn` item (these are
/// inherent-impl methods, so they are indented). Never a fixed byte count: a window that falls back
/// to one is long enough to look like a body and short enough to miss what is at the end of it.
fn method_body<'a>(code: &'a str, sig: &str) -> &'a str {
    let at = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let rest = &code[at + sig.len()..];
    let end = ["\n    pub async fn ", "\n    pub fn ", "\n    async fn ", "\n    fn ", "\n    pub(crate) async fn "]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
    let body = &code[at..at + sig.len() + end];
    assert!(body.len() > 300, "`{sig}` scanned to {} bytes — not a method body", body.len());
    body
}

/// THE SCHEDULING. Whatever the config says, a deadline pass runs every poll.
#[test]
fn the_background_loop_always_runs_a_deadline_pass() {
    let code = code_only(&read("clients/libs/rust-sdk/src/wallet.rs"));
    let body = method_body(&code, "pub fn start_background(&self)");

    assert!(
        body.contains("deadline_safety_due("),
        "`start_background` no longer schedules a deadline pass. The whole-coin `L_k` clock then has \
         NO defender on a default wallet — `auto_exit_due` protects sub-coins and carriers, not \
         this:\n\n{body}"
    );
    // The economics flag may still gate the ROUTINE pass — but it must not be the only arm.
    let at = body.find("auto_refresh && ").expect("the routine-refresh gate is gone");
    let arm = &body[at..];
    assert!(
        arm.contains("} else {") && arm[..arm.find("\n                }\n").unwrap_or(arm.len()).min(arm.len())].len() > 0,
        "the routine-refresh gate has no `else` arm, so a wallet that has not opted into background \
         maintenance runs no deadline pass at all — which is exactly the shape being fixed:\n\n{arm}"
    );
    let else_at = arm.find("} else {").unwrap();
    assert!(
        arm[else_at..].contains("deadline_safety_due("),
        "the `else` arm of the routine-refresh gate does not run the deadline pass:\n\n{}",
        &arm[else_at..arm.len().min(else_at + 600)]
    );
}

/// THE TWO REMEDIES, IN ORDER, AND THE FALLBACK BETWEEN THEM.
#[test]
fn the_cooperative_route_falls_back_to_severing() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    let body = method_body(&code, "pub async fn deadline_safety_due(");

    assert!(
        body.contains("self.auto_refresh_due(margin_blocks)"),
        "the cooperative re-anchor is no longer tried first. It is the cheap remedy and it keeps the \
         coin off-chain; severing costs the coin its off-chain life:\n\n{body}"
    );
    assert!(
        body.contains("self.unilateral_exit("),
        "there is no unilateral fallback. Re-anchoring needs one fresh SE co-signature, and under \
         D40.1 the adversary IS the party asked to sign — a defence that can be declined by its \
         adversary is not a defence:\n\n{body}"
    );
    // Blindness must not sever. A plain trigger over an unknown carrier destroys its allocation.
    assert!(
        body.contains("BLIND") && body.contains("return Err("),
        "an unreadable carrier set no longer stops the pass. Severing blindly broadcasts a PLAIN \
         trigger, which over a token carrier destroys the allocation — a worse outcome than the \
         deadline this pass exists to beat:\n\n{body}"
    );
    // The still-due set must be RE-READ, not inferred from the failure list: a coin the cooperative
    // route moved is no longer at risk under its old id.
    assert!(
        body.contains("coin_near_final(c, tip, margin_blocks)"),
        "the second pass no longer re-derives what is still near its deadline. Inferring it from the \
         first pass's failures would sever coins that were successfully re-anchored under a new \
         id:\n\n{body}"
    );
}

/// NON-VACUITY: the predicate the whole pass rests on must still be keyed on the coin's own
/// `locktime`, which is `L_k` — the clock the prior owners hold.
#[test]
fn the_deadline_predicate_still_reads_the_flat_chains_clock() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    // A two-line free function, so `method_body`'s floor does not apply — take the declaration and
    // the line under it, which is the whole thing.
    let at = code.find("fn coin_near_final(").expect("`coin_near_final` is gone");
    let body = &code[at..code.len().min(at + 240)];
    assert!(
        body.contains("c.locktime") && body.contains("saturating_sub(tip)"),
        "`coin_near_final` no longer measures headroom as `locktime - tip`. That quantity IS the \
         deadline; anything else defends a different clock:\n\n{body}"
    );
}
