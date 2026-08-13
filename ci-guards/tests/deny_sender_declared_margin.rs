//! **[D14] The supersession margin must be keyed on the LIVE rival's STRUCTURAL kind.**
//!
//! A disclosed superseded tier is accepted only if it is certain to LOSE the maturity race against
//! the live tier over the same outpoint. "Certain" is decided by relay and confirmation, so the
//! separation must be at least this design's own propagation budget — `δ` for a state, `δE` for an
//! extension. The shipped rule was strict inequality: a ONE-BLOCK lead, which is not a budget.
//!
//! # The part that is easy to get wrong, and would look done
//!
//! There are two places to read "which kind is this":
//!
//! * `sup.kind` — **which conveyed list the superseded tier arrived in**
//!   (`for (kind, list) in [("state", sup_states), ("extension", sup_exts)]`). That is a field the
//!   SENDER fills in.
//! * the LIVE rival's kind — position parity in the tree, the segment's own shape. Structural.
//!
//! Keying the margin on the first is a margin the adversary selects: on any preset where `δE < δ`,
//! file a superseded STATE under `superseded_extensions` and buy the smaller number. **Regtest is
//! exactly that preset (δ = 6, δE = 3).** Mainnet is immune only because `δ == δE == 36` — the same
//! coincidence that motivated the per-kind correction in the first place, which is why a
//! mainnet-only test would have proved nothing.
//!
//! The existing CSV-*band* check tolerates the same mis-declaration harmlessly, because
//! `[e_floor, e0]` is a strict subset of `[d_floor, d0]` — mis-declaring only TIGHTENS it. A margin
//! moves the other way. So "the code already branches on kind over there" is not evidence.

use std::path::PathBuf;

fn tesr() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("clients/libs/rust/src/tesr.rs");
    std::fs::read_to_string(&p).expect("tesr.rs is readable")
}

/// Whole-line `//` comments removed. Every assertion below runs on this, because the last several
/// pins in this repo that went green over broken code matched a sentence rather than a construction.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// THE LAW: a margin, and it is read off the LIVE rival.
#[test]
fn the_margin_is_the_propagation_budget_and_comes_from_the_live_rival() {
    let code = code_only(&tesr());

    assert!(
        code.contains("let margin = live.kind.margin(p);"),
        "the supersession margin is no longer taken from the LIVE rival's kind. If it is taken from \
         `sup.kind` — which conveyed list the tier arrived in — the sender chooses their own margin, \
         and on regtest (δ=6, δE=3) that halves it."
    );
    assert!(
        code.contains("if sup.csv < live.csv.saturating_add(margin) {"),
        "the relational check is no longer `sup.csv >= live.csv + margin`. A strict inequality \
         (`sup.csv > live.csv`) is a ONE-BLOCK lead, which is not a propagation budget."
    );
    // The old rule must be gone, not merely shadowed — leaving it would let a later reader restore it
    // as "the simpler equivalent".
    assert!(
        !code.contains("if sup.csv <= live.csv {"),
        "the pre-D14 one-block rule is still present in the source"
    );
}

/// THE MAPPING: state → δ, extension → δE. Reversed, this is silently wrong on regtest and silently
/// fine on mainnet, which is the worst possible failure signature.
#[test]
fn the_two_budgets_are_not_swapped() {
    let code = code_only(&tesr());
    let at = code
        .find("fn margin(self, p: &mercurylib::tesr::TesrParams) -> u32 {")
        .expect("`RivalKind::margin` is gone");
    let body = &code[at..at + code[at..].find("\n    }").unwrap_or(400)];
    assert!(
        body.contains("RivalKind::Extension => p.delta_e"),
        "an EXTENSION must clear δE:\n\n{body}"
    );
    assert!(
        body.contains("RivalKind::State => p.delta"),
        "a STATE must clear δ:\n\n{body}"
    );
}

/// STRUCTURAL AT EVERY CONSTRUCTION POINT. `LiveRival::read` takes the kind from its caller, so the
/// rule is only as good as the five call sites. Each must derive it from tree position — parity, or
/// the segment's own shape — and none may read a conveyed field.
#[test]
fn every_live_rival_declares_a_structural_kind() {
    let code = code_only(&tesr());

    // Five CALL sites (the definition is `fn read(`, not `LiveRival::read(`).
    let sites = code.matches("LiveRival::read(").count();
    assert_eq!(
        sites, 5,
        "expected exactly five `LiveRival::read(` call sites, found {sites}. A new one is a new \
         place that must declare a STRUCTURAL kind; a missing one means this census has lost part \
         of its subject."
    );
    // Root ladder: position parity, the same rule the CSV-band check uses.
    assert!(
        code.contains("if i % 2 == 1 { RivalKind::Extension } else { RivalKind::State }"),
        "the root ladder no longer derives the kind from POSITION PARITY over \
         [trigger, ext0, state0, ext1, state1, …]"
    );
    // Ancestor segment: a two-tier segment's funding-outpoint tier is the extension; a spine
    // segment's is the lone state `SP`. The discriminator is whether an extension was parsed at all.
    assert!(
        code.contains("if ext_parsed.is_some() { RivalKind::Extension } else { RivalKind::State }"),
        "the ancestor segment no longer derives the kind from the segment's own SHAPE. A spine \
         segment has one tier and it is a state; a two-tier segment's funding tier is an extension."
    );
    // …and nothing anywhere keys a margin off the conveyed list.
    assert!(
        !code.contains("sup.kind.margin("),
        "a margin is being computed from a superseded tier's DECLARED kind — the sender then picks \
         their own margin, and on regtest (δ=6, δE=3) that halves it"
    );
    assert!(
        !code.contains("RivalKind::from_str") && !code.contains("RivalKind::parse"),
        "`RivalKind` has acquired a parser. It is a STRUCTURAL fact read off tree position; anything \
         that turns a string into one is reading a sender-declared field."
    );
}

/// THE PRESET LANDMINE. The law is satisfiable on the spine only while `d_floor >= δ`, and this is a
/// property of the SCHEDULES rather than of any code path — so it fails at claim time, on the payee's
/// side, with the sender already committed. The unit census lives in `tesr.rs`; this pins that it
/// exists at all, because a schedule tweak is exactly the change that would delete it as unrelated.
#[test]
fn the_preset_census_for_the_margin_exists() {
    let src = tesr();
    assert!(
        src.contains("fn every_preset_can_satisfy_its_own_supersession_margin()"),
        "the preset census for the D14 margin is gone. Both shipped presets clear it today — mainnet \
         144 >= 36, regtest 6 >= 6 with NO slack — so nothing fails until someone tunes a number, \
         and then every honest CATS bundle is refused."
    );
    assert!(
        src.contains("p.d_floor >= p.delta") && src.contains("p.e_floor >= p.delta_e"),
        "the preset census no longer asserts both floors against both budgets"
    );
}
