//! **[D40.2 / A.4] The JS clients' laddered gate must key on COORDINATOR-served evidence, not on
//! three fields the sender fills in.**
//!
//! Neither JS client can verify a TES-R ladder, so both refuse laddered coins and fall through to the
//! un-laddered census `num_sigs == backup_transactions.length`. That census is sound for an
//! un-laddered coin and worthless for a laddered one — a laddered coin's tiers each consume a
//! co-sign slot, so exact equality can only hold if `num_sigs` is under-reported.
//!
//! The gate deciding which world you are in read `protocol_version`, `tesr_ladder` and
//! `child_tesr_bundle`. **All three are written by the sender.** A sender declaring version 0 with
//! both fields omitted fell straight through to a bare equality against an integer these clients
//! never authenticate — a plain HTTP-response edit, no seed, no database write. That made these the
//! CHEAPEST route in the whole trust model, and the corpus had them filed as *exempt* ("they refuse
//! laddered coins outright").
//!
//! It is also the identical shape the Rust receiver already had to close with
//! `MIN_PREPAY_PROTOCOL_VERSION`: a version floor a sender can duck by declaring a lower version.
//!
//! # What replaced it
//!
//! `statechainInfo` is served by the coordinator and keyed by statechain id, so it is not
//! sender-controlled. An `sig_count_attestation` field means the enclave signs `utexo/sig_count/v2`
//! over `(statechain_id, num_sigs, sig_budget, nonce)` — i.e. this deployment runs laddered coins and
//! the count is only as good as a signature THESE CLIENTS CANNOT VERIFY. So they refuse.
//!
//! **This does not make them conformant.** It makes them fail closed for a reason they can check.
//! The real fix is porting the attestation verification; until then they are non-conformant receivers
//! by design rather than by accident, and the trust model says so.
//!
//! # Why this guard was rewritten (2026-08-13): it pinned the DESCRIPTION, not the property
//!
//! The whole gate rests on one unstated assumption — **the JSON key the JS reads is spelled exactly
//! like the Rust field**. `StatechainInfoResponsePayload` carries no `serde(rename_all)`, so it is,
//! and the old third test claimed to check it. It did not. It looked at the **200 bytes immediately
//! before** `pub sig_count_attestation` for the text `serde(rename`, while the thing that decides the
//! key for every field — the struct's own attribute block — sits ~1.6 kB further up. The window
//! could not see what it named. The property happened to be TRUE, which is precisely why nobody
//! noticed the window was blind; an adversarial review applied the mutation to the real tree and
//! watched the guard stay green.
//!
//! **The mutation the old guard survived:** add `#[serde(rename_all = "camelCase")]` to
//! `StatechainInfoResponsePayload`. Every wire key becomes `sigCountAttestation` / `numSigs`; both JS
//! clients then read `undefined`, the structural refusal never fires, `num_sigs` compares `undefined`
//! against a length and the census throws for the wrong reason — or, with a receiver that treats a
//! missing count as absent, accepts. Old guard: green. It is also survived by a per-field
//! `#[serde(rename = …)]` written at the TOP of the field's attribute block, above its ~900-byte doc
//! comment: still attached to the field, still 200+ bytes out of the window's reach.
//!
//! The same fixed-window habit sat in the JS half. `&src[at..at + 800]` was asked to prove that
//! reading the attestation field *refuses*. Today the next statement's `throw` starts at +870 — 70
//! bytes outside. Delete one line of the intervening comment and the window swallows the NEXT
//! block's `throw`, so the gate could be downgraded to a `console.error` with the identical message
//! and the guard would still pass on someone else's refusal.
//!
//! # What this file pins now
//!
//! Every scan window below terminates on a **real symbol**: the struct's attribute block is walked
//! back to the previous item's closing brace; its body is brace-matched to its own `}`; the gate's
//! refusal is brace-matched to the end of the `if` block that the gate's own condition opens.
//! Nothing is a byte count, nothing runs to EOF. Comments and string-literal bodies are blanked
//! before any search, so no assertion here can be satisfied by prose *about* the defect — this file's
//! own mutants are proof, and `receiver.rs` really does contain `#[serde(rename = …)]` (on
//! `TransferReceiverError`), so a file-wide grep would have been a false positive and a
//! struct-scoped one is a real bounds test.
//!
//! And the wire keys are no longer a hard-coded singleton: they are **extracted from the JS**. Every
//! `statechainInfo.<key>` either client reads must exist as an un-renamed field of the served struct.
//! A client that starts reading a fourth key is pinned the moment it does.

use std::ops::Range;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

const CLIENTS: [&str; 2] = [
    "clients/libs/nodejs/transfer_receive.js",
    "clients/libs/web/transfer_receive.js",
];

const PAYLOAD: &str = "lib/src/transfer/receiver.rs";
const STRUCT: &str = "StatechainInfoResponsePayload";
const GATE: &str = "statechainInfo.sig_count_attestation";
const CENSUS: &str = "num_sigs != transferMsg.backup_transactions.length";

// ---------------------------------------------------------------------------------------------
// Lexing. Nothing below searches raw source.
// ---------------------------------------------------------------------------------------------

/// Blank out comments and the *bodies* of string literals, preserving byte offsets exactly (every
/// removed byte becomes a space; newlines survive). Delimiters are kept, so `throw new Error(` and
/// `#[serde(rename = "")]` still read as themselves while their payload text cannot satisfy any
/// assertion.
///
/// Two reasons this is load-bearing rather than tidiness:
///
/// 1. A guard that greps un-stripped source pins a DESCRIPTION. Both JS clients carry a comment
///    block naming `num_sigs == backups.length` and the sender-declared fields verbatim; `receiver.rs`
///    documents `serde(default)` in prose. Delete the code, keep the comment, and an un-stripped
///    guard never notices.
/// 2. Brace matching is only sound once braces inside literals are gone.
///
/// `quotes` is caller-supplied because Rust's `'` is a lifetime far more often than a char literal.
fn strip(src: &str, quotes: &[u8]) -> String {
    fn blank(out: &mut [u8], from: usize, to: usize) {
        for b in &mut out[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    let b = src.as_bytes();
    let n = b.len();
    let mut out = b.to_vec();
    let mut i = 0usize;
    while i < n {
        if b[i] == b'/' && i + 1 < n && b[i + 1] == b'/' {
            let mut j = i;
            while j < n && b[j] != b'\n' {
                j += 1;
            }
            blank(&mut out, i, j);
            i = j;
        } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut j = i + 2;
            let mut depth = 1usize;
            while j < n && depth > 0 {
                if b[j] == b'/' && j + 1 < n && b[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && j + 1 < n && b[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            let end = j.min(n);
            blank(&mut out, i, end);
            i = end;
        } else if quotes.contains(&b[i]) {
            let q = b[i];
            let start = i + 1;
            let mut j = start;
            while j < n {
                if b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if b[j] == q {
                    break;
                }
                // An unterminated literal is a lexing accident, not a string: stop at the line end
                // rather than eating the rest of the file. (Template literals may span lines.)
                if q != b'`' && b[j] == b'\n' {
                    break;
                }
                j += 1;
            }
            let end = j.min(n);
            blank(&mut out, start, end);
            i = end + 1;
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("whole comments/literals were blanked, so char boundaries survive")
}

fn strip_rust(src: &str) -> String {
    strip(src, b"\"")
}

fn strip_js(src: &str) -> String {
    strip(src, b"\"'`")
}

/// Brace-match from an opening `{` to ITS `}`. Returns `None` only on genuinely unbalanced input —
/// which is a hard failure, never a silently-truncated window.
fn match_brace(src: &str, open: usize) -> Option<usize> {
    let b = src.as_bytes();
    assert_eq!(b[open], b'{', "match_brace must start on a brace");
    let mut depth = 0i32;
    for (i, ch) in b.iter().enumerate().skip(open) {
        match ch {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The Rust side: the served payload struct, bounded by real symbols.
// ---------------------------------------------------------------------------------------------

struct StructSpan {
    /// The struct's own attribute block: `[after the previous item, the `pub struct` keyword)`.
    attrs: Range<usize>,
    /// `{ … }`, brace-matched.
    body: Range<usize>,
    /// The real symbol the backward walk stopped on — recorded so the bound is auditable.
    terminator: String,
}

struct Field {
    name: String,
    /// The attribute lines that attach to this field — ALL of them, in declaration order,
    /// however far above the field they were written.
    attrs: String,
    at: usize,
}

/// Locate `pub struct <name>` and both of its real boundaries.
///
/// Upward: walk whole lines back over attribute lines and (comment-blanked) empty lines and stop on
/// the first line that is neither — the previous item's closing `}` in the tree today. That
/// terminator is a real symbol, it is returned, and the walk cannot cross into the previous item
/// because that item's own body ends in a non-attribute line.
///
/// Downward: brace-match. Never a byte count, never EOF.
fn struct_span(src: &str, name: &str) -> Result<StructSpan, String> {
    let decl = format!("pub struct {name}");
    let header = match src.find(&decl) {
        Some(h) => h,
        None => return Err(format!("`{decl}` is gone from {PAYLOAD} — this guard has lost its subject")),
    };
    if src[header + 1..].contains(&decl) {
        return Err(format!("`{decl}` is declared more than once; the bounds below would be ambiguous"));
    }
    // Exact name, not a prefix of a longer one.
    let after = src[header + decl.len()..].trim_start();
    if !(after.starts_with('{') || after.starts_with('<') || after.starts_with('(')) {
        return Err(format!("`{decl}` is a prefix of a different item — refusing to guess its bounds"));
    }

    let line_start = src[..header].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let mut attrs_start = line_start;
    let mut terminator = None;
    while attrs_start > 0 {
        let prev_end = attrs_start - 1; // the '\n' closing the previous line
        let prev_start = src[..prev_end].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line = src[prev_start..prev_end].trim();
        if line.is_empty() || line.starts_with("#[") || line.starts_with("#!") {
            attrs_start = prev_start;
            continue;
        }
        terminator = Some(line.to_string());
        break;
    }
    let terminator = match terminator {
        Some(t) => t,
        None => {
            return Err(format!(
                "the attribute block of `{name}` runs to the start of {PAYLOAD} with no terminating \
                 symbol — the window is unbounded and every assertion under it is worthless"
            ))
        }
    };
    if !(terminator.ends_with('}') || terminator.ends_with(';') || terminator.ends_with('{')) {
        return Err(format!(
            "the attribute block of `{name}` stopped on `{terminator}`, which is not an item \
             boundary — the upward bound is not a real symbol"
        ));
    }

    let open = match src[header..].find('{') {
        Some(o) => header + o,
        None => return Err(format!("`{decl}` has no body")),
    };
    let close = match match_brace(src, open) {
        Some(c) => c,
        None => return Err(format!("`{decl}`'s body is unbalanced; refusing to scan an open-ended window")),
    };

    Ok(StructSpan { attrs: attrs_start..header, body: open..close + 1, terminator })
}

/// Parse the struct body into fields, attaching to each field EVERY attribute line above it back to
/// the previous field declaration. This is what makes distance irrelevant: a `serde(rename)` written
/// above a 900-byte doc comment belongs to the field just as much as one written adjacent to it, and
/// is reported the same way.
fn fields(src: &str, body: &Range<usize>) -> Vec<Field> {
    let inner = &src[body.start + 1..body.end - 1];
    let base = body.start + 1;
    let mut out = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    let mut off = 0usize;
    for line in inner.split_inclusive('\n') {
        let at = base + off;
        off += line.len();
        let t = line.trim();
        if t.is_empty() {
            continue; // blanked doc comments live here; they must not detach an attribute
        }
        if t.starts_with("#[") {
            pending.push(t);
            continue;
        }
        if let Some(name) = field_name(t) {
            out.push(Field { name, attrs: pending.join("\n"), at });
            pending.clear();
            continue;
        }
        pending.clear();
    }
    out
}

fn field_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("pub ")
        .or_else(|| line.strip_prefix("pub(crate) "))
        .or_else(|| line.strip_prefix("pub(super) "))?;
    let (name, _) = rest.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_string())
}

/// THE PROPERTY: for every wire key the JS reads off `statechainInfo`, the served struct declares a
/// field of exactly that name, and nothing renames it — neither a container `rename_all` nor a
/// per-field `rename`, wherever in the field's attribute block it is written.
fn wire_key_violations(rust_src: &str, keys: &[String]) -> Vec<String> {
    let src = strip_rust(rust_src);
    let span = match struct_span(&src, STRUCT) {
        Ok(s) => s,
        Err(e) => return vec![e],
    };
    let mut v = Vec::new();

    let attrs = &src[span.attrs.clone()];
    if attrs.contains("rename_all") {
        v.push(format!(
            "`{STRUCT}` has acquired a container-level `rename_all`, so EVERY JSON key is respelled \
             and both JS clients now read `undefined` from `statechainInfo` — the structural refusal \
             has silently stopped refusing anything and the flat census compares `undefined`. \
             Attribute block (bounded above by `{}`):\n{attrs}",
            span.terminator
        ));
    }

    let decl = fields(&src, &span.body);
    for key in keys {
        match decl.iter().find(|f| &f.name == key) {
            None => v.push(format!(
                "the JS reads `statechainInfo.{key}`, but `{STRUCT}` declares no field `{key}`. \
                 The client is reading `undefined` off the coordinator's payload."
            )),
            Some(f) => {
                if f.attrs.contains("serde(rename") || f.attrs.contains("rename_all") {
                    v.push(format!(
                        "`{key}` has acquired a serde rename, so its JSON key is no longer `{key}` — \
                         the JS that reads `statechainInfo.{key}` now gets `undefined`. Attribute \
                         block attached to the field (byte {}):\n{}",
                        f.at, f.attrs
                    ));
                }
            }
        }
    }
    v
}

// ---------------------------------------------------------------------------------------------
// The JS side: the gate, its refusal, and its position.
// ---------------------------------------------------------------------------------------------

/// The gate's own `if` block: from the first `{` after the gate expression, brace-matched to its `}`.
/// The window therefore ends on a real symbol — the end of the very statement the gate opens — and
/// cannot borrow the NEXT statement's refusal.
fn gate_block(js: &str) -> Result<(usize, Range<usize>), String> {
    let at = js.find(GATE).ok_or_else(|| {
        format!("`{GATE}` does not appear in the CODE (comments and string bodies are blanked before \
                 this search). The only gate left is three SENDER-declared fields, and a sender \
                 declaring protocol_version 0 with tesr_ladder and child_tesr_bundle omitted falls \
                 through to a bare equality against an unauthenticated num_sigs — the cheapest route \
                 in the trust model")
    })?;
    let open = at + js[at..].find('{').ok_or("the gate expression opens no block")?;
    let between = &js[at..open];
    if between.contains(';') || between.contains('}') {
        return Err(format!(
            "the first `{{` after `{GATE}` is not the block its own condition opens (a statement ends \
             in between) — scanning it would test the wrong code:\n{between}"
        ));
    }
    let close = match_brace(js, open).ok_or("the gate's block is unbalanced")?;
    Ok((at, open..close + 1))
}

/// A refusal is pinned as a `throw` — never as its message. A `console.error` carrying the identical
/// sentence satisfies a message pin and refuses nothing.
fn refusal_violations(js_src: &str) -> Vec<String> {
    let js = strip_js(js_src);
    let (_, block) = match gate_block(&js) {
        Ok(x) => x,
        Err(e) => return vec![e],
    };
    let body = &js[block.clone()];
    if !body.contains("throw ") {
        return vec![format!(
            "the attestation field is read but the block it guards does not `throw` — it does not \
             REFUSE, whatever its message says. Block (brace-matched, so this is the gate's own \
             statement and not the next one's):\n{body}"
        )];
    }
    Vec::new()
}

/// The structural refusal must come BEFORE the census it protects — a check that runs after the
/// thing it guards is decoration. Compared as POSITIONS of the two ordered things.
fn order_violations(js_src: &str) -> Vec<String> {
    let js = strip_js(js_src);
    let gate = match js.find(GATE) {
        Some(g) => g,
        None => return vec![format!("`{GATE}`: gate is gone from the code")],
    };
    let census = match js.find(CENSUS) {
        Some(c) => c,
        None => return vec!["the flat census is gone — this guard has lost its subject".to_string()],
    };
    if gate >= census {
        return vec![format!(
            "the attestation refusal (byte {gate}) runs AFTER the flat census it exists to protect \
             (byte {census}). The census would already have accepted the coin."
        )];
    }
    Vec::new()
}

/// Every `statechainInfo.<key>` the client actually reads, taken from CODE.
fn wire_keys_read_by(js_src: &str) -> Vec<String> {
    let js = strip_js(js_src);
    let mut out: Vec<String> = Vec::new();
    let needle = "statechainInfo.";
    let mut from = 0usize;
    while let Some(p) = js[from..].find(needle) {
        let start = from + p + needle.len();
        let key: String = js[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
        from = start;
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// The invariants.
// ---------------------------------------------------------------------------------------------

/// THE STRUCTURAL GATE, in both clients: present in code, and refusing.
#[test]
fn both_js_clients_gate_on_coordinator_served_evidence() {
    for c in CLIENTS {
        let v = refusal_violations(&read(c));
        assert!(v.is_empty(), "{c}:\n{}", v.join("\n\n"));
    }
}

/// THE ORDER.
#[test]
fn the_structural_gate_precedes_the_flat_census() {
    for c in CLIENTS {
        let v = order_violations(&read(c));
        assert!(v.is_empty(), "{c}:\n{}", v.join("\n\n"));
    }
}

/// THE WIRE NAMES. The gate reads JSON keys; the keys are the Rust field names only for as long as
/// nothing renames them. Checked over the whole struct — container attributes included — and over
/// every key either client reads, not just the one this guard is named after.
#[test]
fn the_wire_field_names_are_still_what_the_js_reads() {
    let mut keys: Vec<String> = Vec::new();
    for c in CLIENTS {
        for k in wire_keys_read_by(&read(c)) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    // The extraction must not have silently degraded to "no keys" — that would make the check below
    // vacuously true.
    for expected in ["sig_count_attestation", "num_sigs", "enclave_public_key"] {
        assert!(
            keys.iter().any(|k| k == expected),
            "the JS no longer reads `statechainInfo.{expected}`; extracted keys were {keys:?}. \
             Either the gate/census moved or the extraction is broken — both make this test vacuous."
        );
    }

    let v = wire_key_violations(&read(PAYLOAD), &keys);
    assert!(v.is_empty(), "{PAYLOAD}:\n{}", v.join("\n\n"));
}

/// THE BOUNDS THEMSELVES, asserted rather than assumed.
///
/// Both ends must be real symbols, and the pass above must be a real result rather than an empty
/// haystack: `receiver.rs` genuinely contains `#[serde(rename = …)]` (on `TransferReceiverError`),
/// so a file-wide grep would fire and a correctly-bounded scan must not.
#[test]
fn the_scan_bounds_terminate_on_real_symbols() {
    let raw = read(PAYLOAD);
    assert!(
        raw.contains("serde(rename"),
        "{PAYLOAD} no longer contains any `serde(rename` at all, so the struct-scoped scan above is \
         no longer distinguishable from a file-wide grep — re-check that the bounds still matter"
    );
    let src = strip_rust(&raw);
    let span = struct_span(&src, STRUCT).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        span.terminator.ends_with('}'),
        "the upward bound stopped on `{}` rather than an item's closing brace",
        span.terminator
    );
    assert!(
        src[span.body.clone()].starts_with('{') && src[span.body.clone()].ends_with('}'),
        "the struct body window is not brace-delimited"
    );
    // ...and the out-of-struct renames really are outside it.
    let outside_renames = raw.match_indices("serde(rename").filter(|(i, _)| !span.body.contains(i)).count();
    assert!(
        outside_renames > 0,
        "expected `serde(rename` outside `{STRUCT}` (it is on TransferReceiverError); the bounds test \
         has lost its control case"
    );

    let decl = fields(&src, &span.body);
    let f = decl
        .iter()
        .find(|f| f.name == "sig_count_attestation")
        .expect("the attestation field is gone from the served payload");
    // The old guard looked 200 bytes back from here. Record how far the container attributes really
    // are, so the reason it was blind stays measured rather than asserted.
    let distance = f.at - span.attrs.start;
    assert!(
        distance > 200,
        "the struct's attribute block is only {distance} bytes above `sig_count_attestation`; the \
         old 200-byte look-back would have covered it and this rewrite's premise needs re-checking"
    );
}

/// NON-VACUITY, Rust half. Each mutation is applied to the REAL `receiver.rs` and the checker must
/// fail on it. The first one is the mutation the old guard survived.
#[test]
fn non_vacuity_the_rust_mutations_this_guard_exists_to_catch() {
    let raw = read(PAYLOAD);
    let keys: Vec<String> = ["sig_count_attestation", "num_sigs", "enclave_public_key"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let clean = wire_key_violations(&raw, &keys);
    assert!(clean.is_empty(), "the unmutated tree must pass:\n{}", clean.join("\n"));

    let struct_decl = format!("pub struct {STRUCT} {{");
    let field_decl = "    pub sig_count_attestation: Option<String>,";
    let prev_field = "    pub aggregate_pubkey: Option<String>,";
    assert!(raw.contains(&struct_decl) && raw.contains(field_decl) && raw.contains(prev_field));

    // (1) THE ONE THE OLD GUARD SURVIVED: a container rename_all, ~1.6 kB above the field and so
    //     entirely outside a 200-byte look-back.
    let m1 = raw.replace(&struct_decl, &format!("#[serde(rename_all = \"camelCase\")]\n{struct_decl}"));
    let v1 = wire_key_violations(&m1, &keys);
    assert!(
        v1.iter().any(|s| s.contains("rename_all")),
        "a container-level rename_all on {STRUCT} was NOT caught — this is the exact mutation that \
         left the old guard green while both JS gates read `undefined`"
    );

    // (2) The adjacent per-field rename — the only case the old window could see. Still caught.
    let m2 = raw.replace(
        field_decl,
        &format!("    #[serde(rename = \"sigCountAttestation\")]\n{field_decl}"),
    );
    let v2 = wire_key_violations(&m2, &keys);
    assert!(
        v2.iter().any(|s| s.contains("sig_count_attestation")),
        "an adjacent per-field rename was not caught (regression against the old guard)"
    );

    // (3) The same rename, written at the TOP of the field's attribute block — above its doc
    //     comment, hence out of the old window's reach but still attached to the field.
    let m3 = raw.replace(
        prev_field,
        &format!("{prev_field}\n    #[serde(rename = \"sigCountAttestation\")]"),
    );
    let inserted = m3.find("serde(rename = \"sigCountAttestation\")").unwrap();
    let field_at = m3.find(field_decl).unwrap();
    assert!(
        field_at - inserted > 200,
        "this mutant is meant to sit OUTSIDE a 200-byte look-back; it is only {} bytes away",
        field_at - inserted
    );
    let v3 = wire_key_violations(&m3, &keys);
    assert!(
        v3.iter().any(|s| s.contains("sig_count_attestation")),
        "a rename written above the field's doc comment was NOT caught — distance must not decide \
         whether an attribute counts"
    );

    // (4) The field removed outright: the JS then reads `undefined` forever.
    let m4 = raw.replace(field_decl, "");
    assert_ne!(m4, raw, "the deletion mutant did not apply");
    let v4 = wire_key_violations(&m4, &keys);
    assert!(
        v4.iter().any(|s| s.contains("declares no field")),
        "deleting the served field was not caught"
    );

    // (5) NEGATIVE CONTROL, upward bound: a rename_all on the struct declared immediately ABOVE must
    //     not be attributed to ours.
    let m5 = raw.replace(
        "pub struct StatechainInfo {",
        "#[serde(rename_all = \"camelCase\")]\npub struct StatechainInfo {",
    );
    assert_ne!(m5, raw, "the neighbouring struct used as a control is gone");
    assert!(
        wire_key_violations(&m5, &keys).is_empty(),
        "a rename_all on the PRECEDING struct was blamed on {STRUCT}: the upward bound overshoots"
    );

    // (6) NEGATIVE CONTROL, downward bound: likewise for the struct declared immediately BELOW.
    let m6 = raw.replace(
        "pub struct NewKeyInfo {",
        "#[serde(rename_all = \"camelCase\")]\npub struct NewKeyInfo {",
    );
    assert_ne!(m6, raw, "the following struct used as a control is gone");
    assert!(
        wire_key_violations(&m6, &keys).is_empty(),
        "a rename_all on the FOLLOWING struct was blamed on {STRUCT}: the downward bound overshoots"
    );

    // (7) NEGATIVE CONTROL, description vs construction: the defect written as PROSE is not the
    //     defect. If this fires, the guard is grepping documentation.
    let m7 = raw.replace(
        &struct_decl,
        &format!("/// Never write `#[serde(rename_all = \"camelCase\")]` here.\n{struct_decl}"),
    );
    assert_ne!(m7, raw, "the prose mutant did not apply");
    assert!(
        wire_key_violations(&m7, &keys).is_empty(),
        "a doc comment MENTIONING rename_all was treated as a rename_all"
    );
}

/// NON-VACUITY, JS half. Same discipline: mutate the real clients.
#[test]
fn non_vacuity_the_js_mutations_this_guard_exists_to_catch() {
    for c in CLIENTS {
        let raw = read(c);
        assert!(refusal_violations(&raw).is_empty() && order_violations(&raw).is_empty());

        // (1) The refusal downgraded to logging, message untouched. A guard pinning the message text
        //     passes this. A guard pinning `throw` cannot.
        let m1 = raw.replacen(
            "throw new Error(\"This coordinator attests sig_count",
            "console.error(\"This coordinator attests sig_count",
            1,
        );
        assert_ne!(m1, raw, "{c}: the refusal this guard pins is no longer recognisable");
        let v1 = refusal_violations(&m1);
        assert!(
            v1.iter().any(|s| s.contains("does not `throw`")),
            "{c}: a console.error carrying the identical refusal text was accepted as a refusal"
        );

        // (1b) THE SAME MUTATION, with 250 bytes of the intervening comment deleted — which is all it
        //      takes to pull the NEXT statement's `throw` inside a fixed 800-byte window. In the tree
        //      today that throw sits at +870, i.e. 70 bytes outside; the old window was therefore not
        //      merely narrow but one comment edit away from certifying someone else's refusal. This
        //      is asserted in both directions: the old window WOULD have passed, the brace-matched
        //      window does not.
        let m1b = {
            let stripped = strip_js(&m1);
            let (_, blk) = gate_block(&stripped).unwrap_or_else(|e| panic!("{c}: {e}"));
            let next = blk.end
                + m1[blk.end..]
                    .find("if (transferMsg.protocol_version")
                    .unwrap_or_else(|| panic!("{c}: the sender-declared usability check is gone"));
            let line_start = m1[..next].rfind('\n').expect("a preceding newline");
            // 150 bytes is enough: the next statement's throw sits at gate+870, and 870-150 < 800.
            const CUT: usize = 150;
            assert!(
                line_start > blk.end + CUT + 20,
                "{c}: only {} bytes of comment sit between the gate block and the next statement; \
                 the {CUT}-byte cut this mutant needs would run into code",
                line_start - blk.end
            );
            // Snap the cut back to just after a `//` so the surviving prefix is still commented out
            // — the mutant must shorten a COMMENT, never delete code (the two clients indent this
            // block differently, so a raw offset lands mid-indent in one of them).
            let cut_start = blk.end
                + m1[blk.end..line_start - CUT]
                    .rfind("//")
                    .expect("the region between the gate block and the next statement is comment")
                + 2;
            assert!(
                line_start - cut_start >= CUT,
                "{c}: the snapped cut is only {} bytes",
                line_start - cut_start
            );
            let mut s = m1.clone();
            s.replace_range(cut_start..line_start, "");
            s
        };
        let stripped = strip_js(&m1b);
        let gate = stripped.find(GATE).unwrap();
        let old_window = &stripped[gate..stripped.len().min(gate + 800)];
        assert!(
            old_window.contains("throw "),
            "{c}: expected the shortened-comment mutant to bring the NEXT block's throw inside a \
             fixed 800-byte window (that is the unsoundness being demonstrated)"
        );
        assert!(
            !refusal_violations(&m1b).is_empty(),
            "{c}: the brace-matched window accepted the NEXT statement's throw — exactly the failure \
             the fixed byte count had"
        );

        // (2) The gate deleted from the code while every comment describing it stays. A guard that
        //     greps un-stripped source stays green here.
        let m2 = raw.replacen(
            "if (statechainInfo.sig_count_attestation !== undefined && \
             statechainInfo.sig_count_attestation !== null) {",
            "if (false) {",
            1,
        );
        assert_ne!(m2, raw, "{c}: the gate condition is no longer recognisable");
        let v2 = refusal_violations(&m2);
        assert!(
            !v2.is_empty(),
            "{c}: the gate was removed from the CODE and only its comments remained, and the guard \
             stayed green — it is pinning the description"
        );

        // (3) Ordering: the census run before the gate.
        let census_stmt = "if (statechainInfo.num_sigs != transferMsg.backup_transactions.length) {\n        throw new Error(\"num_sigs is not correct\");\n    }\n";
        let m3 = raw.replacen(
            "if (statechainInfo.sig_count_attestation !== undefined",
            &format!("{census_stmt}    if (statechainInfo.sig_count_attestation !== undefined"),
            1,
        );
        assert_ne!(m3, raw);
        let v3 = order_violations(&m3);
        assert!(
            v3.iter().any(|s| s.contains("runs AFTER")),
            "{c}: a census placed before the structural refusal was not caught"
        );

        // (4) NEGATIVE CONTROL: the refusal message rephrased, the `throw` intact. The property is
        //     the refusal, not its wording — this must stay green.
        let m4 = raw.replacen(
            "throw new Error(\"This coordinator attests sig_count",
            "throw new Error(\"refusing: attested sig_count",
            1,
        );
        assert!(
            refusal_violations(&m4).is_empty() && order_violations(&m4).is_empty(),
            "{c}: rewording the refusal broke the guard — it is pinning the message, not the throw"
        );
    }
}
