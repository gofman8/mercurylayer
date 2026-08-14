//! **A coloured spine batch must not build PLAIN legs under its coloured `SP`.** [S4b]
//!
//! S4 built the coloured split state (`build_colored_spine_batch_sp`) and the entry point
//! (`spine_batch_split_colored`), and `spine_batch_split_ex` correctly refuses the two mismatched
//! lanes: an uncoloured `SP` over a coloured tip, and a coloured `SP` over a plain tip. The refusal
//! it did not have is the one for the case where both agree.
//!
//! Below `SP`, every leg is still constructed by `establish_child_journalled` /
//! `establish_spine_tip_journalled` — the PLAIN builders — and each persisted record is written with
//! `rgb: None`. That is an UNCOLOURED tier over `SP.out[j]`, which after the batch is where the
//! allocation lives. It is the identical construction the "coloured tip, uncoloured SP" arm exists
//! to prevent, one level further down, and its consequence is worse than a refusal: the allocation
//! is BURNED rather than declined.
//!
//! It was latent, not live — `spine_batch_split_colored` has no callers in the repo — which is
//! exactly when this is cheapest to close and easiest to forget. The refusal is a no-op for every
//! path that exists today.
//!
//! **The refusal is now PERMANENT, not a placeholder.** When this guard was written the coloured
//! legs did not exist anywhere and the refusal stood in for them. They exist now
//! (`build_colored_spine_batch` + `cosign_colored_spine_batch`), and they are a SEPARATE pair rather
//! than a coloured mode of this driver — because a coloured leg is a pre-built, pre-coloured
//! transaction that the co-sign signs, not something `establish_*_journalled` can mint from the SE.
//! So this refusal keeps its job for good: it is the sign at the fork saying the plain driver is the
//! wrong road, and the assertions below also check it NAMES the right one.
//!
//! ---
//!
//! **[A7] WHY THIS GUARD WAS REWRITTEN: it pinned the DESCRIPTION of the refusal, not the refusal.**
//!
//! The original assertion was `body.contains("a COLOURED spine batch must not be driven through the
//! PLAIN batch driver")` — the text of the error message — evaluated against the RAW source of the
//! function, comments included. Nothing anywhere in the file pinned that the arm *returns*. An
//! adversarial review applied the obvious mutation to the real tree:
//!
//! ```ignore
//!     (true, true) => {
//!         println!(                       // was: return Err(anyhow::anyhow!(
//!             "a COLOURED spine batch must not be driven through the PLAIN batch driver. …"
//!         );
//!     }
//! ```
//!
//! …and the guard stayed GREEN — 1 passed. The message was still there, so the pin was satisfied,
//! while the function now PRINTS a warning about burning the allocation and then goes on to burn it:
//! `_ => {}` falls through, the plain legs are built, `rgb: None` is persisted over `SP.out[j]`. A
//! description survives every mutation that keeps the prose and changes the behaviour, and the whole
//! point of this refusal is behaviour. Worse, matching against un-stripped source meant the long
//! rationale comment sitting inside the arm could satisfy the pin all by itself, so even DELETING
//! the `println!` would not have failed it.
//!
//! What is pinned now, and why each piece:
//!
//! * the `(true, true)` arm, brace-matched, **contains `return Err(`** — a `println!`, a `log::warn!`,
//!   a `debug_assert!` or a silent fall-through all fail it. This is the property the guard names.
//! * so do the two mismatch arms. `(true, false)` is the arm that keeps a coloured tip off the plain
//!   path at all; demoting IT to a print is the same defect one lane over, and it is live rather than
//!   latent.
//! * the refusal names the coloured pair **in code** (the message string), read from source with all
//!   comment lines stripped — a rationale comment naming it no longer counts.
//! * the fork is ORDERED before anything is spent, signed or built: the positions of
//!   `set_spend_budget` / `cosign_tier` / `establish_*_journalled` are compared against the position
//!   of the end of the match. A refusal that runs after the first leg is built is not a refusal.
//! * every scan window terminates on a real closing brace found by matching, never on a byte count.
//!   The old window was `src[at..].find("\n/// ").unwrap_or(9000)`: had that doc-comment terminator
//!   ever moved, the window would have run 9000 bytes into whatever followed and the assertions
//!   below it would have been satisfiable by an unrelated function's text.
//! * and the checks are a pure function of the source string, so `guard_catches_the_mutations_it_was
//!   _written_for` can apply each mutation to the REAL tesr.rs in memory and assert the guard fails.
//!   That test is the reason this file is allowed to claim it enforces anything.

use std::path::PathBuf;

fn tesr_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("clients/libs/rust/src/tesr.rs");
    std::fs::read_to_string(&p).expect("tesr.rs is readable")
}

/// Drop whole-line `//` and `///` comments. The refusal this guard is about is DESCRIBED at length
/// in the comment that sits directly inside the arm — every identifier and most of the message text
/// appears there too — so a guard that reads raw source is pinning the commentary, not the code.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The block that opens at the first `{` at or after `from`, through ITS matching `}`.
///
/// This is the window discipline the rewrite exists to establish: the window ends on a real brace
/// that the scanner found, so it can never spill into the next arm or the next function. Strings are
/// tracked (the refusal messages are full of prose) and trailing `//` comments are skipped.
fn block_at(code: &str, from: usize, what: &str) -> (usize, usize) {
    let open = from
        + code[from..]
            .find('{')
            .unwrap_or_else(|| panic!("no `{{` after {what} — this guard is reading the wrong thing"));
    let b = code.as_bytes();
    let (mut i, mut depth, mut in_str, mut esc) = (open, 0i32, false, false);
    while i < b.len() {
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
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return (from, i + 1);
            }
        }
        i += 1;
    }
    panic!("unbalanced braces scanning {what} — this guard cannot trust its own window");
}

fn slice<'a>(code: &'a str, marker: &str, what: &str) -> &'a str {
    let at = code
        .find(marker)
        .unwrap_or_else(|| panic!("`{marker}` is gone from tesr.rs — re-point this guard ({what})"));
    let (s, e) = block_at(code, at, what);
    &code[s..e]
}

/// Everything this guard enforces, as a pure function of the file's text, so the non-vacuity test
/// below can feed it a MUTATED copy of the real source and watch it fail. Returns one tag per
/// broken property; empty means the invariant holds.
fn violations(raw: &str) -> Vec<String> {
    let code = code_only(raw);
    let mut v = Vec::new();

    // ---- WINDOW ---------------------------------------------------------------------------------
    let body = slice(&code, "\nasync fn spine_batch_split_ex(", "spine_batch_split_ex");
    // The window is brace-matched, so this cannot silently be the next function's text — but say so
    // out loud, because that is precisely the failure the old `unwrap_or(9000)` window invited.
    assert!(
        !body.contains("pub async fn renew_auto("),
        "the scan window overshot `spine_batch_split_ex` into the following function"
    );
    assert!(
        body.len() > 4000,
        "`spine_batch_split_ex` scanned to {} bytes — that is not the function body",
        body.len()
    );

    let lane_fork = slice(body, "match (tip.is_colored(), colored_sp.is_some())", "the lane fork");

    // ---- THE REFUSALS THEMSELVES, ARM BY ARM ----------------------------------------------------
    //
    // `return Err(` — never the message text. The proven mutation is a `println!` carrying the
    // identical words; it satisfies any prose pin and satisfies none of these.
    for (pat, why) in [
        (
            "(true, true)",
            "a COLOURED tip with a COLOURED `SP` now falls THROUGH the lane fork. Every leg below \
             `SP` is built by `establish_child_journalled` / `establish_spine_tip_journalled` and \
             persisted with `rgb: None` — an UNCOLOURED tier over `SP.out[j]`, the outpoint the \
             allocation is booked at, which DESTROYS it rather than refusing. Printing a warning \
             about that and then doing it is worse than saying nothing",
        ),
        (
            "(true, false)",
            "a COLOURED spine tip handed an UNCOLOURED `SP` no longer RETURNS. This is the live \
             half of the same defect: the uncoloured split state is built over the funding outpoint \
             the allocation sits on and burns it",
        ),
        (
            "(false, true)",
            "a PLAIN spine tip handed a COLOURED `SP` no longer RETURNS. Falling through co-signs \
             an irreversible SE slot onto a transaction carrying RGB transitions for an allocation \
             this tip does not hold",
        ),
    ] {
        let arm = slice(lane_fork, pat, pat);
        if !arm.contains("a COLOURED spine batch must not be driven through") {
            v.push(format!("REFUSAL_{pat}: {why}:\n\n{arm}"));
        }
    }

    // ---- THE REFUSAL MUST NAME THE ROAD THAT IS CORRECT -----------------------------------------
    //
    // Preserved from the original guard, now read from STRIPPED source: the long rationale comment
    // inside this arm names both functions, so on raw text this assertion passed on commentary.
    let both = slice(lane_fork, "(true, true)", "(true, true)");
    if !(both.contains("build_colored_spine_batch") && both.contains("cosign_colored_spine_batch")) {
        v.push(format!(
            "NAMES_COLOURED_PAIR: the refusal no longer names the coloured pair in the message it \
             actually returns, so the next reader concludes the coloured batch does not exist — \
             which is what the ORIGINAL refusal said, correctly at the time, and is no longer \
             true:\n\n{both}"
        ));
    }
    // …and the pair it points at is really there, public, in this file.
    for sig in [
        "pub fn build_colored_spine_batch(",
        "pub async fn cosign_colored_spine_batch(",
    ] {
        if !code.contains(sig) {
            v.push(format!(
                "COLOURED_PAIR_EXISTS: the refusal points at `{sig}`, which no longer exists — a \
                 refusal naming a road that is not there sends the reader back to the plain driver"
            ));
        }
    }

    // ---- ORDERING: THE FORK COMPLETES BEFORE ANYTHING IS SPENT, SIGNED OR BUILT -----------------
    //
    // An ordering property is pinned by comparing POSITIONS, not by asserting both things are
    // present. A refusal that runs after the first leg is established has already terminalized the
    // slot it was supposed to protect, and the batch journal is half-written.
    let fork_end = body
        .find("match (tip.is_colored(), colored_sp.is_some())")
        .map(|at| at + lane_fork.len())
        .expect("the lane fork was located above");
    for after in [
        "set_spend_budget(",
        "cosign_tier(",
        "establish_spine_tip_journalled(",
        "establish_child_journalled(",
    ] {
        let at = body.find(after).unwrap_or_else(|| {
            panic!(
                "`{after}` is gone from `spine_batch_split_ex` — this guard's ordering check has \
                 nothing left to order against and must be re-pointed"
            )
        });
        if at < fork_end {
            v.push(format!(
                "FORK_BEFORE_LEGS: `{after}` appears at byte {at}, BEFORE the lane fork ends at \
                 {fork_end}. The coloured/plain decision must be made while nothing has been spent, \
                 co-signed or journalled — a refusal that fires after the first leg is built has \
                 already committed the SE slot and written a plain tier over the allocation"
            ));
        }
    }

    v
}

/// THE INVARIANT.
#[test]
fn the_plain_batch_driver_refuses_a_coloured_batch_and_names_the_right_one() {
    let v = violations(&tesr_src());
    assert!(v.is_empty(), "{}", v.join("\n\n────────\n\n"));
}

/// **NON-VACUITY — and specifically the mutation the old guard SURVIVED.**
///
/// Each case is applied to the real `tesr.rs` in memory and must be reported. `println!` carrying
/// the original message is case one: it is the exact edit an adversarial review made to the working
/// tree, and the previous version of this file passed on it.
#[test]
fn guard_catches_the_mutations_it_was_written_for() {
    let src = tesr_src();

    /// Replace the first occurrence of `from` that follows `anchor`, once.
    fn mutate(src: &str, anchor: &str, from: &str, to: &str) -> String {
        let a = src.find(anchor).unwrap_or_else(|| panic!("anchor `{anchor}` not found"));
        let at = a + src[a..].find(from).unwrap_or_else(|| panic!("`{from}` not found after `{anchor}`"));
        format!("{}{}{}", &src[..at], to, &src[at + from.len()..])
    }

    let refusal = "return Err(anyhow::anyhow!(";
    let cases: Vec<(&str, String, &str)> = vec![
        // THE PROVEN MUTATION: identical text, no return. The old guard: 1 passed.
        (
            "the_true_true_refusal_becomes_a_println",
            mutate(&src, "(true, true) => {", refusal, "println!("),
            "REFUSAL_(true, true)",
        ),
        (
            "the_coloured_tip_refusal_becomes_a_println",
            mutate(&src, "(true, false) => {", refusal, "println!("),
            "REFUSAL_(true, false)",
        ),
        (
            "the_coloured_sp_refusal_becomes_a_println",
            mutate(&src, "(false, true) => {", refusal, "println!("),
            "REFUSAL_(false, true)",
        ),
        // The refusal stops naming the road that IS correct.
        (
            "the_refusal_stops_naming_the_coloured_pair",
            mutate(&src, "(true, true) => {", "cosign_colored_spine_batch", "the coloured lane"),
            "NAMES_COLOURED_PAIR",
        ),
        // The pair it points at is withdrawn from the public surface.
        (
            "the_coloured_pair_is_no_longer_public",
            mutate(&src, "\n/// **[S4b] The COLOURED spine batch", "pub fn build_colored_spine_batch(", "fn build_colored_spine_batch("),
            "COLOURED_PAIR_EXISTS",
        ),
        // ORDERING: a leg established before the lane fork is reached.
        (
            "a_leg_is_built_before_the_lane_fork",
            mutate(
                &src,
                "\nasync fn spine_batch_split_ex(",
                "    match (tip.is_colored(), colored_sp.is_some()) {",
                "    let _ = establish_child_journalled(cc, wallet_name, tip_coin, &mut journal, 0).await?;\n    match (tip.is_colored(), colored_sp.is_some()) {",
            ),
            "FORK_BEFORE_LEGS",
        ),
    ];

    for (tag, mutated, expect) in cases {
        assert_ne!(mutated, src, "mutation `{tag}` changed nothing — it is not testing the guard");
        let v = violations(&mutated);
        assert!(
            v.iter().any(|m| m.starts_with(expect)),
            "mutation `{tag}` was NOT caught (expected a `{expect}` violation), got: {v:#?}"
        );
    }
}

/// …and the guard must not cry wolf: the unmutated source is the same input every case above starts
/// from, so if it did not pass cleanly the cases would prove nothing.
#[test]
fn the_untouched_source_reports_nothing() {
    assert!(violations(&tesr_src()).is_empty());
}
