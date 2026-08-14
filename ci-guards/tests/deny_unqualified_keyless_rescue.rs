//! The keyless tower's stated INCAPABILITY must stay stated. [D31]
//!
//! D19 asked for something unusual: write down what a keyless tower **cannot** do, as a normative
//! property, so an implementer reading only the spec cannot conclude otherwise. D31 answered it — a
//! keyless tower can broadcast pre-signed tiers at their committed fee and **cannot fee-bump them**,
//! so during a fee spike the defence is the OWNER being online.
//!
//! That sentence is the kind of thing a later editing pass deletes as "negative framing", because it
//! reads like an admission rather than a feature. It is load-bearing: a reader who assumes delegated
//! keyless watching implies spike-time rescue will build on a guarantee that does not exist. And the
//! limit alone is not the whole property — the second half is that the anchor's *anyone-can-spend*
//! script does not quietly supply a rescuer either. Without that half a reader concludes "some
//! stranger will bump it", which is the same wrong conclusion by a different route.
//!
//! This guard does not check prose style. It checks that the two places an implementer actually
//! reads — the protocol spec and the watchtower module they would `use` — still carry the limit, and
//! that the funded variant is still described as OPTIONAL rather than assumed.
//!
//! # Why this guard was rewritten (an adversarial review applied the mutation and watched it pass)
//!
//! The watchtower half of this guard used to end with `src.contains("900")` over the WHOLE file,
//! with a rationale saying it kept "the measured ratio (~900x the anchor's value)". It kept nothing.
//! `clients/libs/rust-sdk/src/watchtower.rs` carries an unrelated test constant — a `900_000`
//! deadline height in the `leaf_watch_entry` unit tests — so the conjunct was satisfied by four
//! digits that have nothing to do with fee-bumping. Deleting the entire no-rescuer paragraph from
//! the D31 section left the guard **green**. A pin that a bare integer elsewhere in the file
//! satisfies is a pin on a DESCRIPTION of the property, not on the property.
//!
//! Three things changed, and they are the general shape of the fix:
//!
//!   1. **The claim is now scoped.** Every assertion about the module runs against the D31 section
//!      only, cut out between the section heading and the module doc's link to [`watch_pass`] — a
//!      symbol this guard separately proves is really defined in the file, so the window cannot
//!      quietly run off the end of the doc and into the test module (which is exactly how `900_000`
//!      got to answer for `900×`). No `unwrap_or(len)`, no fixed byte count, no slice-to-EOF.
//!   2. **The digits must appear as a QUANTITY, not as a substring.** A ratio has to read as a
//!      ratio (`900×`) and be backed by the measurement it came from (the `180 330`-sat child).
//!   3. **…and the corrected, non-ratio form is accepted too.** `PROTOCOL.md` has since retracted
//!      the `~900×` figure as three stacked arithmetic errors and replaced it with a *structural*
//!      reason: an anchor-only child's change can never clear `CHILD_CHANGE_DUST = 330` out of a
//!      `P2A_VALUE = 240` anchor, so it cannot be built at any fee rate. That is a stronger
//!      statement of the same property, so the guard must not punish a module that adopts it. What
//!      it refuses is a D31 section with NO reason at all — because "anyone-can-spend" with the
//!      reason removed reads as a rescue.
//!
//! Every check below is exercised by a mutation test that plants the historical defect and asserts
//! this guard rejects it. A guard nobody has ever seen fail is a guard nobody knows the strength of.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(1).expect("repo root").to_path_buf()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Normalise whitespace so a reflow (or a `//!` prefix) does not fail the guard. The point is that
/// the STATEMENT survives, not that its line breaks do.
fn flat(s: &str) -> String {
    let stripped: String =
        s.lines().map(|l| l.trim_start().trim_start_matches("//!").trim()).collect::<Vec<_>>().join(" ");
    stripped.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Cut a scan window out of `hay`, bounded at BOTH ends by text that must really be there.
///
/// The failure this exists to prevent is the quiet one: a window that starts at a real marker and
/// then runs to the end of the file (or for a fixed number of bytes) will happily let text from an
/// unrelated function — or from a unit test three hundred lines later — satisfy an assertion about
/// the section you meant. So a missing `end` is an ERROR here, never "take what is left".
///
/// Both markers are returned as violations rather than panics so the mutation tests below can
/// assert on them, and so a section that gets RENAMED produces a message telling the next editor to
/// re-point the guard instead of deleting it.
fn window<'a>(hay: &'a str, start: &str, end: &str, what: &str) -> Result<&'a str, String> {
    let at = hay.find(start).ok_or_else(|| {
        format!("{what}: the section marker `{start}` is gone. If it was renamed, RE-POINT this guard; if it was deleted, that is the defect the guard is for.")
    })?;
    let rest = &hay[at..];
    let len = rest.find(end).ok_or_else(|| {
        format!("{what}: the window terminator `{end}` no longer follows `{start}`. This guard refuses to scan an unbounded window — re-point the terminator at a real symbol that still exists.")
    })?;
    Ok(&rest[..len])
}

// ---------------------------------------------------------------------------------------------
// PROTOCOL.md
// ---------------------------------------------------------------------------------------------

#[test]
fn the_protocol_spec_states_that_a_keyless_tower_cannot_fee_bump() {
    let spec = flat(&read("docs/utexo/PROTOCOL.md"));
    assert!(
        spec.contains("cannot fee-bump"),
        "PROTOCOL.md must state plainly that a keyless tower CANNOT fee-bump (D19/D31). Without it, \
         'delegable, keyless watching' reads as implying spike-time rescue, which the protocol does \
         not provide."
    );
    assert!(
        spec.contains("the defence is the owner being online")
            || spec.contains("the guarantee during a fee spike is \u{201c}the owner is online\u{201d}")
            || spec.contains("the guarantee during a fee spike is \"the owner is online\""),
        "PROTOCOL.md must name the consequence, not just the limit: during a fee spike the defence \
         falls back to the OWNER being online — which is precisely the condition a watchtower \
         otherwise removes, so leaving it implicit is what misleads."
    );
}

/// The funded tower is a deployment OPTION under D31, not the design. §5.13 previously read "towers
/// hold a small funded fee wallet", which states the opposite — that every tower has one.
#[test]
fn the_funded_tower_is_offered_not_assumed() {
    let spec = read("docs/utexo/PROTOCOL.md");
    let flat_spec = flat(&spec);
    assert!(
        !flat_spec.contains("towers hold a small funded fee wallet"),
        "PROTOCOL.md must not assert that towers HOLD a fee wallet — under D31 that is the optional \
         variant, and stating it as fact re-creates the assumption D31 removed"
    );
    assert!(
        flat_spec.contains("an operator may run a tower with a small hot fee wallet")
            || flat_spec.contains("optional: the funded-tower variant"),
        "…but the option must still be described, so the choice is informed rather than ad hoc"
    );
    // The bounded exposure is the reason the option is offerable at all. If that qualifier goes, the
    // option starts reading as "trust the tower", which is a much larger claim.
    assert!(
        flat_spec.contains("no coin keys"),
        "the funded variant's exposure must stay bounded IN TEXT: it holds no coin keys, so a \
         compromise costs the operator's float and cannot touch a user's coin"
    );
}

// ---------------------------------------------------------------------------------------------
// watchtower.rs — the module an implementer imports
// ---------------------------------------------------------------------------------------------

const WATCHTOWER: &str = "clients/libs/rust-sdk/src/watchtower.rs";

/// Start of the normative D31 section in the module doc.
const D31_HEADING: &str = "[d31] what a keyless tower cannot do";
/// End of it. `watch_pass` is not prose: it is a `pub fn` in this very file, and
/// [`the_window_terminator_is_a_real_symbol`] proves that on every run. Bounding here is what keeps
/// the scan from reaching the `#[cfg(test)]` module, where `900_000` lives.
const D31_TERMINATOR: &str = "[`watch_pass`]";

/// The whole watchtower property, as a function of the source text so a mutation can be fed to it.
///
/// Returns one string per violation; empty means the module still states its incapability.
fn keyless_limit_violations(src: &str) -> Vec<String> {
    let flat_src = flat(src);
    let win = match window(&flat_src, D31_HEADING, D31_TERMINATOR, "watchtower.rs D31 section") {
        Ok(w) => w,
        Err(e) => return vec![e],
    };
    let mut bad = Vec::new();

    // (1) The capability limit itself, scoped. File-wide this was already true; scoped, it also
    //     survives the section being gutted while the phrase lingers somewhere unrelated.
    if !win.contains("cannot fee-bump") {
        bad.push(
            "watchtower.rs documents a KEYLESS tower; its D31 section must say that such a tower \
             CANNOT fee-bump, or the section reads as a trust discussion where it is a capability \
             discussion"
                .to_string(),
        );
    }

    // (2) The second half: anyone-can-spend is a permission, not a rescuer. Both words are
    //     load-bearing and neither implies the other — "permission" alone is the sentence a reader
    //     misreads as a rescue; "incentive" alone is not about the anchor at all.
    if !(win.contains("permission") && win.contains("incentive")) {
        bad.push(
            "the D31 section must state that the anchor being ANYONE-CAN-SPEND is a permission and \
             not an incentive. Dropping it leaves 'anyone may spend the anchor' standing on its \
             own, which is read as 'somebody will', i.e. exactly the spike-time rescue D31 denies"
                .to_string(),
        );
    }

    // (3) …and the reason must be quantified, in one of the two forms the repo has used.
    //
    //     Ratio form: the figure has to read as a RATIO and be backed by the measurement it was
    //     computed from. A bare `900` is what the old guard accepted and is worth nothing — the
    //     mutation test below shows the file keeps a `900` after the paragraph is deleted.
    let ratio_form = (win.contains("900\u{d7}") || win.contains("900x"))
        && (win.contains("180 330") || win.contains("180,330") || win.contains("180330"));
    //     Structural form: PROTOCOL.md's correction — an anchor-only child cannot produce a legal
    //     change output at ANY fee rate, because the change must clear CHILD_CHANGE_DUST = 330 out
    //     of a P2A_VALUE = 240 anchor. Both are real constants in this tree.
    let structural_form = win.contains("child_change_dust") && win.contains("p2a_value");
    if !(ratio_form || structural_form) {
        bad.push(
            "the D31 section states that no third party rescues the anchor but no longer says WHY. \
             Keep one of the two forms: the measured ratio (`900×`, backed by the 180 330-sat \
             child), or — better, and what PROTOCOL.md now carries after retracting that ratio — \
             the structural reason (`CHILD_CHANGE_DUST` = 330 cannot come out of a `P2A_VALUE` = \
             240 anchor, so the child is unbuildable at any fee rate)"
                .to_string(),
        );
    }
    bad
}

/// An implementer reads the module they import, not only the spec.
#[test]
fn the_watchtower_module_carries_the_same_limit() {
    let violations = keyless_limit_violations(&read(WATCHTOWER));
    assert!(violations.is_empty(), "{}", violations.join("\n\n"));
}

/// The terminator has to be a REAL symbol, or bounding the window is theatre: prose can be reflowed
/// away, and a window whose end marker vanished would otherwise have to fall back to "the rest of
/// the file". `watch_pass` is a `pub fn` right here, and the module doc links to it.
#[test]
fn the_window_terminator_is_a_real_symbol() {
    let raw = read(WATCHTOWER);
    assert!(
        raw.contains("pub fn watch_pass("),
        "the D31 scan window terminates at the doc link to `watch_pass`, which must stay a real \
         public item of this module — otherwise the window has no dependable end and this guard \
         would be scanning to EOF"
    );
}

/// The concrete regression: the window must not be able to reach the unit tests. This is the exact
/// distance the old `src.contains(\"900\")` was covering when it reported success.
#[test]
fn the_d31_window_cannot_reach_the_test_module() {
    let raw = read(WATCHTOWER);
    let flat_src = flat(&raw);
    let win = window(&flat_src, D31_HEADING, D31_TERMINATOR, "watchtower.rs D31 section").unwrap();
    assert!(
        raw.contains("900_000"),
        "this test is only meaningful while the unrelated `900_000` test constant is still in the \
         file; if it is gone, the demonstration is stale but the bounding is still correct"
    );
    assert!(
        !win.contains("900_000"),
        "the D31 window reaches the `#[cfg(test)]` module. That is how a deadline constant came to \
         satisfy a claim about a fee ratio."
    );
    assert!(
        win.len() < flat_src.len() / 2,
        "the D31 window is most of the file — it is not a window"
    );
}

/// Non-vacuity, and the whole reason this file was rewritten. Each case is a real mutation applied
/// to the REAL module text; each asserts the mutation actually changed something (so the fixture
/// cannot rot into a no-op) and that this guard rejects the result.
#[test]
fn guard_rejects_the_mutations_it_was_written_for() {
    let real = read(WATCHTOWER);
    assert!(
        keyless_limit_violations(&real).is_empty(),
        "fixtures below are derived from the real file; it must be clean first"
    );

    // M1 — THE MUTATION THE OLD GUARD SURVIVED. Delete the no-rescuer reason from the D31 section,
    // leaving the rest of the module (and the `900_000` unit-test constant) untouched.
    let m1: String = real
        .lines()
        .filter(|l| {
            !(l.trim_start().starts_with("//!")
                && (l.contains("900\u{d7}") || l.contains("180 330") || l.contains("permission")))
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(m1, real, "M1 did not apply — re-point it at the current wording");
    assert!(
        m1.contains("900"),
        "M1 is only the historical defect if a bare `900` SURVIVES it — that is what the old \
         `src.contains(\"900\")` was matching, and why it stayed green"
    );
    let v = keyless_limit_violations(&m1);
    assert!(
        v.iter().any(|m| m.contains("no longer says WHY")),
        "the deleted no-rescuer reason was not reported. Violations: {v:?}"
    );

    // M2 — delete the capability limit from the module doc while leaving everything else.
    let m2: String = real
        .lines()
        .filter(|l| !(l.trim_start().starts_with("//!") && l.contains("cannot fee-bump")))
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(m2, real, "M2 did not apply — re-point it at the current wording");
    let v = keyless_limit_violations(&m2);
    assert!(
        v.iter().any(|m| m.contains("CANNOT fee-bump")),
        "the deleted fee-bump limit was not reported. Violations: {v:?}"
    );

    // M3 — the section is deleted outright (the "negative framing" edit). The guard must say so,
    // not silently scan an empty window.
    let m3 = real.replace("[D31] What a keyless tower CANNOT do", "Notes on watchtowers");
    assert_ne!(m3, real, "M3 did not apply — re-point it at the current heading");
    let v = keyless_limit_violations(&m3);
    assert!(
        v.iter().any(|m| m.contains("section marker")),
        "a missing D31 heading must be reported, not treated as nothing to check. Violations: {v:?}"
    );

    // M4 — the reason is kept but demoted out of the section (moved below `watch_pass`, e.g. into
    // a function's doc). Scoping is the only thing that catches this; a file-wide `contains` cannot.
    let m4 = real
        .replace("//! measured to need a **180 330-sat child** — roughly **900×** its value — so \"anyone *may* spend", "//! measured. So \"anyone *may* spend")
        + "\n/// Historical note: a **180 330-sat child**, roughly **900×** the anchor. permission\n";
    assert_ne!(m4, real, "M4 did not apply — re-point it at the current wording");
    let v = keyless_limit_violations(&m4);
    assert!(
        v.iter().any(|m| m.contains("no longer says WHY")),
        "the reason was moved OUT of the D31 section and the guard accepted it — the window is not \
         doing its job. Violations: {v:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// TRUST-MODEL.md — the B4 row
// ---------------------------------------------------------------------------------------------

/// Start of B4's keyless-delegation coverage claim.
const B4_CLAIM: &str = "keyless delegation to n towers covers";
/// End of it. `FeeBumpConfig` is a real `pub struct` (see [`the_b4_terminator_is_a_real_symbol`]),
/// it is the last thing B4's D31 qualification names, and the row continues afterwards with the
/// unrelated D46/D51 carrier discussion — so this is a genuine boundary, not a guess.
const B4_TERMINATOR: &str = "feebumpconfig";

fn b4_violations(src: &str) -> Vec<String> {
    let tm = flat(src);
    let win = match window(&tm, B4_CLAIM, B4_TERMINATOR, "TRUST-MODEL.md B4") {
        Ok(w) => w,
        Err(e) => return vec![e],
    };
    let mut bad = Vec::new();
    if !win.contains("cannot fee-bump") {
        bad.push(
            "B4's coverage claim must carry the D31 fee-bump exception INLINE. A qualification \
             stated only elsewhere does not reach the reader of this row, and this row is the one \
             people quote."
                .to_string(),
        );
    }
    if !(win.contains("reactive") || win.contains("fee spike")) {
        bad.push(
            "B4 must say WHAT the keyless delegate does not cover — either the fee-spike case or \
             (the stronger, current form) that it covers the reactive clock only. An unqualified \
             \"covers\" is the sentence this guard exists to prevent."
                .to_string(),
        );
    }
    bad
}

/// The trust model's B4 row claims keyless delegation covers offline periods. That is true except in
/// exactly the case this decision is about.
///
/// RE-POINTED (not deleted, per this guard's own instruction). The D40/A.2 documentation pass
/// rewrote B4 to say something STRONGER: keyless delegation covers the *reactive* clock ONLY,
/// because a laddered watch entry exports `deadline_block: u32::MAX`. The old phrasing ("covers
/// offline periods except during a fee spike") conceded less. What must not be lost is the inline
/// qualification, which is the half a reader quotes.
#[test]
fn the_trust_model_qualifies_its_offline_coverage_claim() {
    let violations = b4_violations(&read("docs/utexo/TRUST-MODEL.md"));
    assert!(violations.is_empty(), "{}", violations.join("\n\n"));
}

/// B4's window used to be `&tm[at..(at + 700).min(tm.len())]` — a fixed byte count. That is the same
/// defect as scanning to EOF wearing a smaller number: it is unrelated to where the claim ends, so
/// it can truncate the qualification (a false alarm) or, in a shorter row, spill into the next
/// sentence and let its words answer for B4's. It now ends on a real symbol instead.
#[test]
fn the_b4_terminator_is_a_real_symbol() {
    let cfg = read("clients/libs/rust-sdk/src/config.rs");
    assert!(
        cfg.contains("pub struct FeeBumpConfig"),
        "B4's scan window terminates at `FeeBumpConfig`, which must stay a real type in the SDK — \
         if it is renamed, re-point B4_TERMINATOR at the new name rather than reverting to a byte \
         count"
    );
}

/// Non-vacuity for B4, plus proof the window is no longer length-based.
#[test]
fn b4_guard_rejects_its_mutations_and_tolerates_reflow() {
    let real = read("docs/utexo/TRUST-MODEL.md");
    assert!(b4_violations(&real).is_empty(), "B4 must be clean before mutating it");

    // M1 — the qualification is dropped from the row.
    let m1 = real.replace("**cannot fee-bump them**", "may need help");
    assert_ne!(m1, real, "M1 did not apply — re-point it at the current wording");
    let v = b4_violations(&m1);
    assert!(
        v.iter().any(|m| m.contains("INLINE")),
        "an unqualified B4 coverage claim was accepted. Violations: {v:?}"
    );

    // M2 — the row grows. A fixed 700-byte window would have started failing here even though the
    // property is intact; terminating on a symbol does not care how much prose is inserted.
    let filler = "and this is an inserted clause of the kind an editing pass adds. ".repeat(20);
    let m2 = real.replacen(
        "Keyless delegation to N towers covers",
        &format!("Keyless delegation to N towers covers {filler}"),
        1,
    );
    assert!(m2.len() > real.len() + 700, "M2 must insert more than the old fixed window");
    assert!(
        b4_violations(&m2).is_empty(),
        "inserting prose inside B4 broke the guard — the window is length-based again"
    );

    // M3 — the claim is renamed away. Must be reported as a re-point, never silently skipped.
    let m3 = real.replace("Keyless delegation to N towers covers", "Delegation covers");
    assert_ne!(m3, real, "M3 did not apply");
    let v = b4_violations(&m3);
    assert!(
        v.iter().any(|m| m.contains("section marker")),
        "a vanished B4 claim must be reported. Violations: {v:?}"
    );
}
