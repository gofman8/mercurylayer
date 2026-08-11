//! The keyless tower's stated INCAPABILITY must stay stated. [D31]
//!
//! D19 asked for something unusual: write down what a keyless tower **cannot** do, as a normative
//! property, so an implementer reading only the spec cannot conclude otherwise. D31 answered it — a
//! keyless tower can broadcast pre-signed tiers at their committed fee and **cannot fee-bump them**,
//! so during a fee spike the defence is the OWNER being online.
//!
//! That sentence is the kind of thing a later editing pass deletes as "negative framing", because it
//! reads like an admission rather than a feature. It is load-bearing: a reader who assumes delegated
//! keyless watching implies spike-time rescue will build on a guarantee that does not exist. The
//! measured gap is not marginal — rescuing the 240-sat P2A anchor needed a **180 330-sat child**,
//! about 900× its value, so no disinterested third party supplies one either.
//!
//! This guard does not check prose style. It checks that the two places an implementer actually
//! reads — the protocol spec and the watchtower module they would `use` — still carry the limit, and
//! that the funded variant is still described as OPTIONAL rather than assumed.

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

/// An implementer reads the module they import, not only the spec.
#[test]
fn the_watchtower_module_carries_the_same_limit() {
    let src = flat(&read("clients/libs/rust-sdk/src/watchtower.rs"));
    assert!(
        src.contains("cannot fee-bump"),
        "watchtower.rs documents a KEYLESS tower; it must say that such a tower cannot fee-bump, or \
         its trust discussion reads as a capability discussion"
    );
    assert!(
        src.contains("900"),
        "the module should keep the measured ratio (~900x the anchor's value) — it is what turns \
         'anyone-can-spend' from an apparent rescue into a permission nobody exercises"
    );
}

/// The trust model's B4 row claims keyless delegation covers offline periods. That is true except in
/// exactly the case this decision is about.
#[test]
fn the_trust_model_qualifies_its_offline_coverage_claim() {
    let tm = flat(&read("docs/utexo/TRUST-MODEL.md"));
    let at = tm.find("keyless delegation to n towers covers offline periods").expect(
        "TRUST-MODEL.md B4 must still make its offline-coverage claim — if this moved, re-point the \
         guard rather than deleting it",
    );
    let window = &tm[at..(at + 400).min(tm.len())];
    assert!(
        window.contains("except during a fee spike") && window.contains("cannot fee-bump"),
        "B4's offline-coverage claim must carry the fee-spike exception INLINE. A qualification \
         stated only elsewhere does not reach the reader of this row, and this row is the one people \
         quote."
    );
}
