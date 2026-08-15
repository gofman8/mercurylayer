//! **Every published value floor must be the one the code derives, at the rate the code ships.**
//!
//! # The defect this exists to catch
//!
//! `min_child_value` and `min_spine_tip_value` take the fee rate as an ARGUMENT. So a document can
//! print a perfectly correct evaluation of them — at a rate the code does not ship — and read as
//! current forever. That is what happened: four of the six normative documents published floors
//! evaluated at 2 sat/vB while `TesrParams::mainnet()` ships 3.0, and one of them printed an
//! arithmetic slip on top (`1306`, where the 2.0 evaluation is `1310`).
//!
//! A prose sweep cannot catch this, because nothing about `1310 sat at 2 sat/vB` looks stale — it is
//! internally consistent, correctly attributed, and wrong only in that nobody runs at that rate.
//! The only defence is to DERIVE the number here, from the same constants the code derives it from,
//! and require the documents to agree.
//!
//! # Why this is a legitimate source scan
//!
//! It asserts the presence of a literal in prose and the absence of superseded literals — never
//! reachability or behaviour. The arithmetic is reproduced from constants read out of the source, so
//! if a constant moves, the expected figure moves with it and the documents are re-checked against
//! the new value automatically.

use std::fs;

const NORMATIVE: [&str; 6] = [
    "docs/utexo/spec/SPEC.md",
    "docs/utexo/spec/PROTOCOL.md",
    "docs/utexo/spec/CHILDREN.md",
    "docs/utexo/spec/LIGHTNING.md",
    "docs/utexo/spec/TRUST-MODEL.md",
    "docs/utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md",
];

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// Pull `name: <number>` out of a source file — the same literals `lib/src/tesr.rs` derives from.
fn num_after(src: &str, key: &str) -> f64 {
    let i = src.find(key).unwrap_or_else(|| panic!("`{key}` not found — the constant was renamed"));
    let rest = &src[i + key.len()..];
    let s: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '_')
        .filter(|c| *c != '_')
        .collect();
    s.parse().unwrap_or_else(|_| panic!("`{key}` did not parse as a number: {s:?}"))
}

struct Floors {
    rate: f64,
    child: u64,
    spine_tip: u64,
}

/// Reproduce `min_child_value` / `min_spine_tip_value` from the constants the code holds.
fn derive() -> Floors {
    let tesr = read("lib/src/tesr.rs");
    // The MAINNET preset is the shipped one; take the rate from that constructor specifically so a
    // regtest-only change cannot move what the documents are held to.
    let mainnet = {
        let i = tesr.find("pub fn mainnet()").expect("`mainnet()` preset");
        let rest = &tesr[i..];
        let end = rest.find("\n    }").unwrap_or(rest.len());
        rest[..end].to_string()
    };
    let rate = num_after(&mainnet, "committed_fee_rate:");
    let tier = num_after(&tesr, "pub const TIER_VBYTES: u64 =") as u64;
    let p2a = num_after(&tesr, "pub const P2A_VALUE: u64 =") as u64;
    let dust = num_after(&tesr, "pub const DUST_LIMIT: u64 =") as u64;

    let committed = (tier as f64 * rate) as u64;
    Floors {
        rate,
        // (committed_fee + P2A) * 2 + dust  -- two rungs, then the final state must clear dust
        child: (committed + p2a) * 2 + dust,
        // one cap rung only: the change leg gets no extension
        spine_tip: committed + p2a + dust,
    }
}

/// Render a figure the way documents write it: bare, and with a thin-space thousands separator.
fn spellings(n: u64) -> Vec<String> {
    let bare = n.to_string();
    let mut out = vec![bare.clone()];
    if n >= 1000 {
        let (a, b) = bare.split_at(bare.len() - 3);
        out.push(format!("{a} {b}"));
        out.push(format!("{a},{b}"));
        out.push(format!("{a}\u{202f}{b}"));
    }
    out
}

fn mentions_any(hay: &str, needles: &[String]) -> bool {
    needles.iter().any(|n| hay.contains(n.as_str()))
}

/// A document that NAMES a floor must publish the figure the code derives for it.
#[test]
fn every_published_floor_matches_what_the_code_derives() {
    let f = derive();
    let mut bad = Vec::new();
    for doc in NORMATIVE {
        let src = read(doc);
        for (sym, val) in [("min_child_value", f.child), ("min_spine_tip_value", f.spine_tip)] {
            if src.contains(sym) && !mentions_any(&src, &spellings(val)) {
                bad.push(format!("{doc}: names `{sym}` but never publishes {val}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "these documents name a value floor without publishing the figure the code derives at the \
         SHIPPED rate of {} sat/vB (min_child_value = {}, min_spine_tip_value = {}).\n\n{}\n\n\
         These floors take the rate as an ARGUMENT, so an evaluation at some other rate is \
         internally consistent and reads as current forever. Publish the shipped evaluation.",
        f.rate,
        f.child,
        f.spine_tip,
        bad.join("\n")
    );
}

/// No document may publish an evaluation at a rate the code does not ship.
#[test]
fn no_document_publishes_a_floor_evaluated_at_an_unshipped_rate() {
    let f = derive();
    let tier = num_after(&read("lib/src/tesr.rs"), "pub const TIER_VBYTES: u64 =") as u64;
    let p2a = num_after(&read("lib/src/tesr.rs"), "pub const P2A_VALUE: u64 =") as u64;
    let dust = num_after(&read("lib/src/tesr.rs"), "pub const DUST_LIMIT: u64 =") as u64;

    // Every plausible OTHER integer rate, evaluated the same way. If one of these appears next to a
    // floor symbol, the document is quoting a rate nobody runs.
    let mut stale: Vec<(u64, f64)> = Vec::new();
    for r in [1u64, 2, 4, 5] {
        if (r as f64 - f.rate).abs() < 0.01 {
            continue;
        }
        let c = tier * r;
        stale.push(((c + p2a) * 2 + dust, r as f64));
        stale.push((c + p2a + dust, r as f64));
    }

    let mut bad = Vec::new();
    for doc in NORMATIVE {
        let src = read(doc);
        for (val, rate) in &stale {
            if mentions_any(&src, &spellings(*val)) {
                bad.push(format!("{doc}: publishes {val}, which is the floor at {rate} sat/vB"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "these documents publish a value floor evaluated at a rate the code does not ship (shipped \
         is {} sat/vB):\n\n{}\n\nRe-evaluate at the shipped rate. A floor is not a constant — it is \
         a function of the rate, and printing yesterday's rate is how a document goes stale while \
         staying self-consistent.",
        f.rate,
        bad.join("\n")
    );
}

/// NON-VACUITY: the derivation must actually produce the numbers the shipped code produces.
#[test]
fn the_derivation_reproduces_the_shipped_floors() {
    let f = derive();
    assert!(
        (f.rate - 3.0).abs() < 0.01,
        "the shipped committed fee rate read {} — if that is a deliberate change, update this \
         assertion and expect every normative document to be re-checked against the new floors",
        f.rate
    );
    assert_eq!(f.child, 1_560, "min_child_value at the shipped rate");
    assert_eq!(f.spine_tip, 945, "min_spine_tip_value at the shipped rate");
}
