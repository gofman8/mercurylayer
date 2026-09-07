//! **[D53] A document may not publish the superseded depth cap without saying it is superseded.**
//!
//! The mainnet split-depth cap was published as **10 / 23 transactions** in five documents, carried
//! into `DECISIONS.md` as a measured figure, and independently "re-derived" twice — including once
//! by me, in the session that later found it wrong. Every derivation agreed because every derivation
//! read the same premise: the BARE latency rule `exit_wait_blocks <= epoch`.
//!
//! The rule the cap is actually measured by adds `exit_slack_margin`:
//! `exit_wait_blocks(chain) + exit_slack_margin(chain) <= initlock`. Under it the caps are **8 / 19**
//! (mainnet, `lockheight_init = 10 000`) and **54 / 111** (regtest, `lockheight_init = 1 000`);
//! depths 9 and 10 need more headroom than the window can ever offer, so the build side was minting
//! children no receiver could adopt — after terminalizing the parent.
//!
//! # [ONE COIN SHAPE] The window is a CONSTANT, not a calendar
//!
//! When D53 was written the right-hand side of that rule was the epoch REMAINING on the parent's
//! flat backup chain — `epoch_deadline − tip`, at most `initlock` — and the receiver admitted a
//! conveyed child through `check_exit_headroom_with_margin` over that remaining window. A laddered
//! coin no longer carries a flat backup (none is co-signed at deposit or at any hop), so it has no
//! epoch deadline and no "remaining window". `enforce_split_depth_cap` (through
//! `split_cap_decision`) now measures the leaf's REAL exit walk — length AND latency, margin
//! included — against `initlock` itself (`epoch_blocks = info.initlock`), the same constant the
//! receiver's `enforce_exit_chain_length` reads, and `check_exit_headroom_with_margin` has no live
//! caller. The NUMBERS did not move: the maximum window was always `initlock`, and the caps were
//! always the depths that fit it. What moved is their derivation — 8 / 19 and 54 / 111 are the caps
//! at every tip, not merely at a freshly re-anchored one.
//!
//! # What this guard is, and what it is not
//!
//! It is NOT a re-derivation. The arithmetic lives in `mercurylib` (`max_split_depth`), and
//! `the_build_side_never_admits_what_the_receive_side_refuses` is what holds the build side and the
//! receive side together; this crate deliberately has no dependencies, so a guard here cannot
//! recompute a cap and must not pretend to.
//!
//! It is a **staleness tripwire**: a document may state the old numbers only in the company of a
//! marker saying they are superseded. That is enough to stop the specific failure that happened —
//! a reader lifting `23` out of a scoping document into the specification — without freezing prose
//! that legitimately records what the old baseline was.
//!
//! If the schedule changes and 19/111 become stale in turn, this guard goes stale with them. That is
//! why it names D53 rather than the numbers alone: the next person to move the cap has to come here.

use std::path::PathBuf;

/// Documents that feed the specification. A dated audit may quote whatever it observed.
// Every normative document. A guard that reads a file which no longer exists fails for the wrong
// reason and teaches a reader to ignore it, so this list holds exactly what is on disk.
const CHECKED: [&str; 6] = [
    "docs/utexo/spec/SPEC.md",
    "docs/utexo/spec/PROTOCOL.md",
    "docs/utexo/spec/CHILDREN.md",
    "docs/utexo/spec/LIGHTNING.md",
    "docs/utexo/spec/TRUST-MODEL.md",
    "docs/utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md",
];

/// Spellings of the superseded cap. Each is specific enough that an unrelated `23` cannot trip it.
const STALE: [&str; 6] = [
    "max_split_depth = 10",
    "max_split_depth = 68",
    "23 transactions",
    "139 transactions",
    "depth 10 / 23",
    "depth cap **10**",
];

/// The marker that makes a stale number legible as history rather than as fact.
const SUPERSEDED_MARKER: &str = "D53";

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// THE RULE. A document that states the old cap must also carry the D53 marker.
#[test]
fn a_document_stating_the_superseded_depth_cap_must_say_it_is_superseded() {
    let mut offenders: Vec<String> = Vec::new();
    for doc in CHECKED {
        let src = read(doc);
        let hits: Vec<&str> = STALE.iter().copied().filter(|s| src.contains(s)).collect();
        if !hits.is_empty() && !src.contains(SUPERSEDED_MARKER) {
            offenders.push(format!("{doc}: states {hits:?} with no D53 marker"));
        }
    }
    assert!(
        offenders.is_empty(),
        "these documents publish the SUPERSEDED split-depth cap as if it were current. The shipped \
         caps are depth 8 / 19 transactions (mainnet) and depth 54 / 111 (regtest) — the old 10/23 \
         and 68/139 were measured against the bare latency rule, not the margin rule \
         (`exit_wait_blocks + exit_slack_margin <= initlock`, the FIXED exit window a laddered coin \
         is measured against), and depths 9 and 10 were unadoptable at every tip. Either correct \
         the number or mark it as superseded by citing D53.\n\n{}",
        offenders.join("\n")
    );
}

/// The four scoping documents that carried the number are bannered rather than rewritten — their
/// old figures are the "before" half of a labelled before/after and deleting them would falsify the
/// record. The present-state form of that protection: the document must publish the CURRENT cap and
/// never the superseded one.
///
/// The banner it used to pin was a before/after record — the exact shape the clean documents in
/// `current/` exist to be free of. Deleting the assertion with it would have been wrong: the hazard
/// is real (this document publishes per-depth cost figures, and a reader who lifts a stale one into
/// the spec is how the superseded number reached five documents). So the check is kept and restated
/// against what the document SAYS rather than what it confesses.
#[test]
fn the_cost_document_publishes_the_current_depth_cap_and_not_the_superseded_one() {
    let doc = "docs/utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md";
    let src = read(doc);
    assert!(
        src.contains("depth-8") || src.contains("max_split_depth = 8") || src.contains("depth 8"),
        "{doc} publishes per-depth cost figures but never names the mainnet cap of 8. A reader \
         cannot tell which depths are reachable, which is how a figure measured at an unreachable \
         depth gets lifted into the specification."
    );
    for stale in STALE {
        assert!(
            !src.contains(stale),
            "{doc} states the superseded cap (`{stale}`). Its cost figures are per-depth, so a \
             superseded cap here is not a stale sentence — it is a price list for depths the \
             admission rule refuses."
        );
    }
}

/// NON-VACUITY. A guard that cannot fail is a census of nothing — the failure mode this repo has hit
/// with every source-scanning test at least once.
#[test]
fn the_detector_would_catch_a_bare_restatement() {
    let stale_doc = "The mainnet cap is 23 transactions and that is final.";
    assert!(
        STALE.iter().any(|s| stale_doc.contains(s)),
        "the detector no longer recognises the superseded cap"
    );
    assert!(
        !stale_doc.contains(SUPERSEDED_MARKER),
        "…and a bare restatement carries no marker, so the rule above would fire on it"
    );
    let corrected = "The cap was 23 transactions before D53 corrected it to 19.";
    assert!(
        corrected.contains(SUPERSEDED_MARKER),
        "a properly-marked historical statement must pass"
    );
}
