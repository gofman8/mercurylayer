//! **[REQ-76 / REQ-80 / REQ-81] The three prohibitions the round left behind.**
//!
//! The discharge round is deleted. What survives it are three rules about what may NEVER come back,
//! and a prohibition is exactly the kind of claim a source scan settles correctly: §0.2's evidence
//! rule says a scan establishes **presence, absence and ordering** — never reachability or
//! behaviour — and "this text does not exist" is an absence. So these are not weak substitutes for
//! behavioural tests; they are the right instrument for the shape of requirement being enforced.
//!
//! What each one is actually defending:
//!
//! * **REQ-76** — no path may require an operator to hold capital. The round's float traced to ONE
//!   sentence: a funded successor root had to be CONFIRMED before a holder could migrate onto it.
//!   The prohibition is against that ordering returning under another name.
//! * **REQ-80** — "no operator liquidity" may never be stated without the mechanism that makes it
//!   true, and never extended to the Lightning legs, which consume ordinary channel liquidity in
//!   both directions and always did. An unqualified claim is marketing, and this document spent a
//!   whole section learning that.
//! * **REQ-81** — a close is triggered by a root owner's decision, never by a calendar. The moment
//!   an epoch or a deadline can trigger one, "absentee" becomes a category again and holders can be
//!   discharged for failing to be awake.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("could not read {}: {e}", p.display()))
}

/// Lines of a document with its fenced code blocks and block quotes removed.
///
/// Quotes and examples legitimately QUOTE the prohibited shapes — this file's own spec section does,
/// to record what was deleted. A guard that could not tell a rule from a quotation of the rule would
/// be unusable, and would be silenced rather than fixed.
fn prose_lines(doc: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (i, raw) in doc.lines().enumerate() {
        let t = raw.trim_start();
        if t.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || t.starts_with('>') || t.starts_with("//") {
            continue;
        }
        out.push((i + 1, raw.to_string()));
    }
    out
}

/// **[REQ-80] The zero-liquidity claim may not travel alone.**
///
/// Within the paragraph that makes it, the claim must name at least one of the mechanisms that make
/// it true. The paragraph is the unit deliberately: a mechanism three sections away does not help a
/// reader who quotes the sentence.
#[test]
fn the_zero_liquidity_claim_always_carries_its_mechanism() {
    let doc = read("docs/utexo/spec/SPEC.md");
    // The mechanisms, in the words the document actually uses.
    const MECHANISMS: &[&str] = &[
        "out of `F` itself",
        "out of `F`",
        "renewal moves no value",
        "moves no value",
        "fronts nothing",
        "no successor root",
        "there is no window",
        "closer fronts",
    ];
    let mut naked = Vec::new();
    for para in doc.split("\n\n") {
        // A TABLE ROW naming a section is not making the claim — the coverage table has an
        // "Operator liquidity — ZERO" cell, and demanding a mechanism inside a table cell would be
        // demanding prose where a pointer belongs. The first guard run fired on exactly that and on
        // an empty chunk, which is the guard being wrong rather than the document.
        let prose: String = para
            .lines()
            .filter(|l| !l.trim_start().starts_with('|') && !l.trim_start().starts_with('>'))
            .collect::<Vec<_>>()
            .join("\n");
        if prose.trim().is_empty() {
            continue;
        }
        let para = prose.as_str();
        let lower = para.to_lowercase();
        let claims_zero = (lower.contains("no operator liquidity")
            || lower.contains("operator liquidity — zero")
            || lower.contains("requirement is zero")
            || lower.contains("liquidity is zero"))
            // A paragraph SAYING the claim must carry a mechanism is not itself making the claim.
            && !lower.contains("must always carry")
            && !lower.contains("must be accompanied");
        if !claims_zero {
            continue;
        }
        if !MECHANISMS.iter().any(|m| para.contains(m)) {
            // Report the sentence that MADE the claim, not the paragraph's first line — which can
            // be blank and tells a reader nothing about what to fix.
            let hit = para
                .lines()
                .find(|l| {
                    let x = l.to_lowercase();
                    x.contains("no operator liquidity")
                        || x.contains("requirement is zero")
                        || x.contains("liquidity is zero")
                        || x.contains("operator liquidity — zero")
                })
                .unwrap_or("")
                .trim()
                .to_string();
            naked.push(hit);
        }
    }
    assert!(
        naked.is_empty(),
        "[REQ-80] a zero-liquidity claim was made without naming the mechanism that makes it true. \
         Unqualified, it is a marketing claim rather than a property — and this document already \
         published a 9%-of-TVL float, so a reader has every reason to want the reason. Offending \
         paragraph(s): {naked:#?}"
    );
}

/// **[REQ-80] and it may never be extended to the Lightning legs.**
#[test]
fn the_zero_claim_is_never_extended_to_lightning() {
    let doc = read("docs/utexo/spec/SPEC.md");
    for (n, line) in prose_lines(&doc) {
        let l = line.to_lowercase();
        if (l.contains("lightning") || l.contains("§8"))
            && (l.contains("no liquidity") || l.contains("zero liquidity"))
            // The rule itself says exactly this, in order to forbid it.
            && !l.contains("must not")
            && !l.contains("never")
        {
            panic!(
                "[REQ-80] line {n} appears to extend the zero-liquidity claim to the Lightning legs, \
                 which consume ordinary channel liquidity in both directions and always did: {line}"
            );
        }
    }
}

/// **[REQ-81] Nothing may SCHEDULE a close.**
///
/// Scans the CODE, not the document: the prohibition is about what the system does.
///
/// **This guard was narrowed after it fired on the owner's own caller, and the narrowing is the
/// point.** It used to refuse the string `collapse_grant` anywhere on a client path. That was a
/// serviceable proxy while no client could close a tree at all — and it became wrong the moment one
/// could, because it forbade the very thing REQ-81 requires to EXIST: an owner-initiated close.
/// A guard that cannot tell "the owner asked" from "a calendar asked" would be satisfied only by
/// having no close at all, which is not the requirement.
///
/// So it now checks the requirement itself: **no function whose name marks it as automatic,
/// scheduled or periodic may mention a close.** The owner's entry point may; a background pass may
/// not. That is the line REQ-81 actually draws — "absentee" must never become a category, and it
/// becomes one exactly when something other than a decision can trigger a discharge.
#[test]
fn no_scheduled_trigger_reaches_a_close() {
    const SOURCES: &[&str] = &[
        "clients/libs/rust-sdk/src/wallet.rs",
        "clients/libs/rust-sdk/src/tokens.rs",
        "clients/libs/rust/src/tesr.rs",
        "clients/libs/rust/src/rgb.rs",
    ];
    // What marks a function as something other than a decision. `auto`/`renew`/`rollover` are this
    // repo's own words for its background maintenance pass; the rest are the shapes a scheduler
    // arrives under.
    const SCHEDULED: &[&str] = &[
        "_auto", "auto_", "background", "maintenance", "periodic", "_tick", "sweep_", "on_timer",
        "cron", "deadline_pass", "watchtower_",
    ];
    // The close, in every form a caller could name it.
    const CLOSE: &[&str] = &["collapse_grant", "request_collapse", "collapse_first"];

    for rel in SOURCES {
        let src = read(rel);
        let mut current_fn: Option<(usize, String)> = None;
        let mut depth: i32 = 0;
        for (n, line) in src.lines().enumerate() {
            let t = line.trim_start();
            let is_comment = t.starts_with("//");
            // Track which function body we are inside, by brace depth. Crude, and sufficient: a
            // scheduled function's body is what matters and Rust nests it in braces like any other.
            if !is_comment && depth == 0 {
                if let Some(idx) = t.find("fn ") {
                    let name: String = t[idx + 3..]
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    current_fn = Some((n + 1, name));
                }
            }
            if !is_comment {
                depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
                if depth <= 0 {
                    depth = 0;
                    if let Some((_, ref name)) = current_fn {
                        // A one-line body closes immediately; keep the name until the next `fn`.
                        let _ = name;
                    }
                }
            }
            if is_comment {
                continue;
            }
            let Some((decl_line, ref fname)) = current_fn else { continue };
            let lower = fname.to_lowercase();
            if !SCHEDULED.iter().any(|m| lower.contains(m)) {
                continue;
            }
            if let Some(hit) = CLOSE.iter().find(|c| line.contains(**c)) {
                panic!(
                    "[REQ-81] {rel}:{} names `{hit}` inside `{fname}` (declared at line {decl_line}), \
                     whose name marks it as automatic or scheduled. A close is the root owner's \
                     DECISION. The moment a calendar, an epoch or a maintenance pass can trigger one, \
                     'absentee' becomes a category again and a holder can be discharged for failing \
                     to be awake: {line}",
                    n + 1
                );
            }
        }
    }
}

/// **[REQ-81] …and the owner's close must actually EXIST.**
///
/// The companion to the guard above, and the reason that one had to be narrowed rather than
/// silenced. REQ-81 is not "no close" — it is "a close, triggered only by its owner". A repository
/// that satisfied the prohibition by having no owner-initiated close would satisfy it the way an
/// empty room satisfies a fire code.
#[test]
fn the_owner_initiated_close_exists() {
    let src = read("clients/libs/rust/src/tesr.rs");
    assert!(
        src.contains("pub async fn request_collapse("),
        "[REQ-81] no owner-initiated close exists on the client. The SE has had a working \
         `collapse_grant` since #169, and for as long as nothing called it the route's ACCEPT path \
         had never run once — measured only in refusal."
    );
}

/// **[REQ-76] No path may require a funded successor output before a holder can act.**
///
/// The float traced to exactly this ordering. The guard is deliberately narrow — it looks for the
/// successor root being required to be CONFIRMED, which is the sentence that produced 9% of TVL —
/// because a broad grep for "successor" would fire on REQ-64b's retry hint, which is a different and
/// permitted thing.
#[test]
fn nothing_requires_a_confirmed_successor_before_a_holder_may_act() {
    let doc = read("docs/utexo/spec/SPEC.md");
    // PARAGRAPH-scoped, not line-scoped. The historical framing ("the round is deleted", "traced
    // to") routinely sits on a neighbouring line, and a line-scoped guard reported the summary
    // bullet that explains the requirement was WITHDRAWN as if it were reintroducing it. A guard
    // that cannot tell a rule from its own obituary gets silenced rather than fixed.
    for para in doc.split("\n\n") {
        let l = para.to_lowercase();
        if l.trim_start().starts_with('>') {
            continue;
        }
        let requires_confirmed_successor = l.contains("confirmed")
            && (l.contains("successor root") || l.contains("f_b"))
            && (l.contains("must") || l.contains("required") || l.contains("before"));
        let is_historical = l.contains("deleted")
            || l.contains("retired")
            || l.contains("traced to")
            || l.contains("used to")
            || l.contains("had to be")
            || l.contains("withdrawn")
            || l.contains("no longer")
            || l.contains("the round");
        assert!(
            !(requires_confirmed_successor && !is_historical),
            "[REQ-76] a paragraph states that a CONFIRMED successor root is required before a holder \
             can act, with no framing marking it as history. That is the single ordering the whole \
             operator float traced to; if it is being reintroduced, §5.5's zero-liquidity claim is \
             false. Paragraph: {}",
            para.lines().take(3).collect::<Vec<_>>().join(" ")
        );
    }
}
