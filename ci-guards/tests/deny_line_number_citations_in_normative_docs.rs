//! **[Stage 0.3] A NORMATIVE document may not cite code by LINE NUMBER.**
//!
//! Line numbers rot silently, and a rotted citation is worse than none: it reads as verified. The
//! Stage-0.3 sweep found every one of `ADMISSION-INPUTS.md`'s six citations pointing somewhere else
//! — `tesr.rs:3288`, `:3233`, `:3232`, `:3220` and `:3031` had all drifted into
//! `cosign_colored_renewal` and `cosign_colored_receiver_state`, functions with no relationship to
//! anything the surrounding prose claimed, and `receiver.rs:684` had landed in
//! `verify_if_locktime_is_reasonable_tx_version_and_output_size`. Two of `PROTOCOL.md`'s and one of
//! `CHILDREN.md`'s were wrong the same way.
//!
//! None of that was noticed by anyone, over months, because a `file.rs:1234` in a document looks
//! exactly as authoritative when it is wrong.
//!
//! # Why only the normative set
//!
//! Audits, sweeps and scoping documents cite by line **correctly**: they are DATED OBSERVATIONS of
//! what stood at a position at a moment. Re-symbolising `AUDIT-2026-07-30.md` would falsify the
//! record of what the audit actually looked at. The rule is therefore scoped to the documents that
//! become the specification, where a citation is a claim about the code as it is NOW.
//!
//! A symbol is not immune to rot either — a renamed function breaks it — but it breaks LOUDLY: the
//! name is greppable, so a reader can check it in one command and a reviewer can see it is gone.

use std::path::PathBuf;

/// **[D60] The normative set is DERIVED FROM `README.md`, not hand-listed.**
///
/// It used to be a hand-list of seven. `docs/utexo/README.md` labels **ten** documents
/// `*Normative.*`, and the three the list omitted — `GRANULARITY-SPEC.md`, `SUBECONOMIC-FINALITY.md`
/// and `INVALIDATION-SPEC.md` — held **281** line citations between them. Two sampled at random were
/// both rotted: *"`token_carrier_outpoints` is empty (tokens.rs:364-385)"* (the symbol is ~1 000
/// lines away; 362-368 is a doc block) and *"`plan()` is greedy largest-first (select.rs:89-112)"*
/// (`pub fn plan(` is at `:101`; line 89 is inside a different function's subset-sum loop).
///
/// A hand-list is a second place to remember something, and it silently omitted a third of the
/// corpus it claimed to police. Reading the README means **adding a doc to the normative set enrols
/// it automatically** — the census cannot drift from the label any more.
fn normative_docs() -> Vec<String> {
    let readme = read("docs/utexo/README.md");
    let mut out = Vec::new();
    // A README entry is a markdown link followed, within its bullet, by the literal `*Normative`.
    // Bullets can wrap, so scan per-bullet rather than per-line.
    for bullet in readme.split("\n- ") {
        if !bullet.contains("*Normative") {
            continue;
        }
        if let Some(open) = bullet.find("](") {
            let rest = &bullet[open + 2..];
            if let Some(close) = rest.find(')') {
                let target = &rest[..close];
                if target.ends_with(".md") && !target.contains('/') {
                    out.push(format!("docs/utexo/{target}"));
                }
            }
        }
    }
    // DECISIONS.md is normative by use — everything else cites it as the authority — but the README
    // files it under records rather than specs, so it is added explicitly.
    out.push("docs/utexo/DECISIONS.md".to_string());
    out.sort();
    out.dedup();
    assert!(
        out.len() >= 8,
        "the README-derived normative set collapsed to {} entries. That is the failure mode this \
         function exists to prevent — a census that quietly stops censusing. Either the README's \
         `*Normative.*` labels moved, or its bullet format changed: {out:?}",
        out.len()
    );
    out
}

/// `DECISIONS.md` and `LIGHTNING.md` are records of decisions and of an external fork's API surface
/// respectively; both legitimately quote a position AT THE TIME. They are held to the weaker rule:
/// no NEW line citations, measured against the count when this guard was written.
const GRANDFATHERED: [(&str, usize); 5] = [
    ("docs/utexo/DECISIONS.md", 31),
    ("docs/utexo/LIGHTNING.md", 16),
    // [D60] The three the hand-list omitted. These are RATCHETS, not exemptions: the counts are what
    // was there when the census was widened to see them, and they may only go DOWN. Fixing 281
    // citations in one pass would be 281 chances to introduce a new wrong one, so they are frozen
    // where they stand and re-symbolised as each document is next edited for its own reasons.
    ("docs/utexo/GRANULARITY-SPEC.md", 134),
    ("docs/utexo/SUBECONOMIC-FINALITY.md", 83),
    ("docs/utexo/INVALIDATION-SPEC.md", 64),
];

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// Count `something.rs:123` / `something.js:12-34` style citations.
fn line_citations(src: &str) -> Vec<String> {
    const EXTS: [&str; 6] = [".rs:", ".kt:", ".js:", ".ts:", ".sql:", ".py:"];
    let mut out = Vec::new();
    for line in src.lines() {
        for ext in EXTS {
            let mut from = 0;
            while let Some(i) = line[from..].find(ext) {
                let at = from + i + ext.len();
                // A digit immediately after the colon is a line number. `foo.rs: the` is prose.
                if line[at..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    let start = line[..from + i].rfind(char::is_whitespace).map_or(0, |s| s + 1);
                    let end = line[at..]
                        .find(|c: char| !c.is_ascii_digit() && c != '-')
                        .map_or(line.len(), |e| at + e);
                    out.push(line[start..end].to_string());
                }
                from = at;
            }
        }
    }
    out
}

/// THE RULE, for the documents held to it strictly.
#[test]
fn no_normative_document_cites_code_by_line_number() {
    let mut offenders: Vec<String> = Vec::new();
    for doc in normative_docs() {
        if GRANDFATHERED.iter().any(|(g, _)| *g == doc) {
            continue;
        }
        for c in line_citations(&read(&doc)) {
            offenders.push(format!("{doc}: {c}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "these normative documents cite code by LINE NUMBER. Line numbers rot silently and a rotted \
         citation reads as verified — the Stage-0.3 sweep found ALL SIX of ADMISSION-INPUTS.md's \
         pointing into unrelated functions. Cite the SYMBOL and the file instead: a symbol breaks \
         loudly, because a reader can grep it.\n\n{}",
        offenders.join("\n")
    );
}

/// THE WEAKER RULE for the grandfathered set: no NEW line citations. A decision record may legitimately
/// quote a position as it stood when the decision was taken; it may not accumulate more.
#[test]
fn the_grandfathered_records_do_not_accumulate_more() {
    for (doc, allowed) in GRANDFATHERED {
        let n = line_citations(&read(doc)).len();
        assert!(
            n <= allowed,
            "{doc} now carries {n} line-number citations, up from the {allowed} grandfathered when \
             this guard was written. A decision record may quote a position AS IT STOOD; new prose \
             must cite by symbol. If a decision genuinely needs a new positional quote, raise the \
             number here deliberately and say why."
        );
    }
}

/// NON-VACUITY. The detector must actually detect — otherwise the census above is a census of
/// nothing, which is the failure mode every source-scanning test in this repo has hit at least once.
#[test]
fn the_detector_finds_a_line_citation_and_ignores_prose() {
    assert_eq!(
        line_citations("see `lib/src/tesr.rs:210` for the params"),
        // The closing backtick is not part of the citation; the detector stops at the digits.
        vec!["`lib/src/tesr.rs:210"],
        "the detector no longer finds a plain line citation"
    );
    assert_eq!(
        line_citations("see (`receiver.rs:768-779`) and `tesr.rs:5210`.").len(),
        2,
        "the detector no longer finds ranges or multiple citations on one line"
    );
    assert!(
        line_citations("`enforce_split_depth_cap_shaped` in `clients/libs/rust/src/tesr.rs`")
            .is_empty(),
        "the detector flags a SYMBOL citation — the very form this guard is pushing people toward"
    );
    assert!(
        line_citations("in tesr.rs: the ladder is built here").is_empty(),
        "the detector flags prose after a filename — `foo.rs: the` is a sentence, not a citation"
    );
}

/// **[D60] NON-VACUITY FOR THE DERIVATION ITSELF.**
///
/// The two rules above both pass with a BROKEN derivation: the strict rule skips whatever is
/// grandfathered, and the ratchet reads `GRANDFATHERED` directly rather than the derived set. So a
/// `normative_docs()` that returned only `DECISIONS.md` would leave this file green — which is the
/// exact shape of defect that put 281 citations outside the census in the first place.
///
/// This test pins the derivation's OUTPUT: the three documents the old hand-list omitted must be in
/// it, by name, and so must the seven it had.
#[test]
fn the_derivation_actually_finds_every_normative_document() {
    let docs = normative_docs();
    for must in [
        // the three the hand-list omitted — the whole point of D60
        "docs/utexo/GRANULARITY-SPEC.md",
        "docs/utexo/SUBECONOMIC-FINALITY.md",
        "docs/utexo/INVALIDATION-SPEC.md",
        // and the ones it already had, so widening cannot silently narrow
        "docs/utexo/SPEC.md",
        "docs/utexo/PROTOCOL.md",
        "docs/utexo/CHILDREN.md",
        "docs/utexo/LIGHTNING.md",
        "docs/utexo/TRUST-MODEL.md",
        "docs/utexo/ADMISSION-INPUTS.md",
        "docs/utexo/DECISIONS.md",
    ] {
        assert!(
            docs.iter().any(|d| d == must),
            "`normative_docs()` no longer yields {must}. It is derived from `docs/utexo/README.md`'s \
             `*Normative.*` labels, so either the label was removed from that entry or the README's \
             bullet format changed and the parser stopped seeing it. A census that quietly stops \
             censusing is the defect this guard exists to prevent.\n\nderived: {docs:?}"
        );
    }
}

/// And the grandfathered ratchets must name documents that are actually IN the census — otherwise a
/// ratchet is a promise about a file nobody checks.
#[test]
fn every_grandfathered_document_is_in_the_derived_set() {
    let docs = normative_docs();
    for (g, _) in GRANDFATHERED {
        assert!(
            docs.iter().any(|d| d == g),
            "{g} carries a grandfathered citation budget but is NOT in the derived normative set, so \
             the budget polices nothing.\n\nderived: {docs:?}"
        );
    }
}
