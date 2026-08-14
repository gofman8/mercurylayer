//! **[#152] A token balance may not depend on the SHAPE an allocation arrived in.**
//!
//! `get_asset_balance` is chain-anchored: it settles an allocation when its witness is mined. Every
//! allocation in this design is deliberately UN-BROADCAST, so what the RGB engine can settle depends
//! on how the allocation reached the wallet — and it did. A wallet receiving the same asset twice
//! summed correctly when the second arrived as a whole-child forward and reported only the FIRST
//! when it arrived as a piece carved from a spine tip. Both children adopted, both carrying their
//! consignment, one balance.
//!
//! That is not a rounding difference; it is a balance whose value depends on a routing detail the
//! holder did not choose and cannot see.
//!
//! The fix is not to patch the engine's view but to stop asking it about material it structurally
//! cannot see. The wallet's OWN adopted records — root carriers (`tesr-`), children (`ctesr-`) and
//! spine tips (`spinetip-`) — each carry the consignment-derived amount the receiver validated at
//! claim. That is the authority a receiver's safety already rests on, so it is what the balance
//! reports.
//!
//! ## Why this guard was rewritten (it used to pin a DESCRIPTION, not the property)
//!
//! The first version of this file pinned the three PROBES — `load_spine_tip(`, `load_child(`,
//! `tesr::load(` — anywhere inside a fixed 2_500-byte window, and nothing else. An adversarial
//! review applied the obvious mutation to the real tree and watched the guard stay green:
//!
//! ```ignore
//! if let Some(t) = mercuryrustlib::tesr::load_spine_tip(cc, name, &sid).await? {
//!     let _ = t;          // the probe survives; the SUM is gone
//!     continue;
//! }
//! ```
//!
//! Three tests passed. That is defect #152 verbatim, one shape over: the balance under-reports for
//! whoever happens to hold that shape, and the probe the guard was checking is still right there in
//! the source. **Looking a shape up is not counting it.** So the pins below are on the three
//! ACCUMULATIONS `*out.entry(r.contract_id.clone()).or_default() += r.amount;`, each one required
//! inside the segment governed by ITS OWN probe — not merely somewhere in the same function, which a
//! surviving sibling shape's sum would satisfy.
//!
//! Two further weaknesses of the same kind are closed here:
//!
//! * the window was `at + 2_500` over a body of roughly 1_500 characters, so it ran a thousand
//!   characters into `token_carrier_outpoints` — every assertion could in principle have been
//!   satisfied by the WRONG function's text. Windows are now brace-matched: they end on the
//!   function's own closing brace, a real symbol that exists in the stripped source, and
//!   [`the_scan_windows_stop_at_each_functions_own_closing_brace`] proves the next function is out
//!   of reach.
//! * `get_token_balances` was pinned only on `.max(settled)`, which `balance: 0u64.max(settled)`
//!   satisfies — the ledger computed, then thrown away. It is now pinned on `led.max(settled)` AND
//!   on `led` being read out of the ledger map.
//!
//! And, per the rule these guards live by: a guard that cannot fail is decoration, so
//! [`guard_catches_every_mutation_it_was_written_for`] replays each of those mutations against the
//! real source text in memory and requires the checker to reject it.

use std::path::PathBuf;

const SRC: &str = "clients/libs/rust-sdk/src/tokens.rs";
const LEDGER_SIG: &str = "pub async fn ledger_token_balances(";
const PUBLIC_SIG: &str = "pub async fn get_token_balances(";
/// The function that immediately follows the ledger in the file. Named only so the window test can
/// assert it is OUT of the ledger's window — the old fixed byte count pulled it in.
const NEXT_FN: &str = "fn token_carrier_outpoints(";

/// The accumulation itself, whitespace-squeezed. This — not the probe above it — is the property
/// #152 is about: the shape's consignment-derived amount added (`+=`, never `=`, or a second
/// receive of the same contract silently replaces the first) into the per-contract total.
const ACCUM: &str = "*out.entry(r.contract_id.clone()).or_default() += r.amount;";

/// The three shapes a coloured allocation can live in, in the order they MUST be probed: narrowest
/// key first, so the first hit is the right hit.
const SHAPES: [(&str, &str); 3] = [
    ("load_spine_tip(", "a spine TIP — the shape the original defect lost"),
    ("load_child(", "an adopted CHILD"),
    ("tesr::load(", "a root CARRIER"),
];

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// Whitespace-insensitive comparison, so a rustfmt line break cannot break a pin.
fn squeeze(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// A scan window that ends on a REAL symbol: the function's own closing brace, found by matching
/// braces (string literals skipped) from the opening one. Never a byte count, never EOF — a window
/// that overshoots lets the NEXT function's text satisfy assertions about THIS one.
fn fn_body<'a>(code: &'a str, sig: &str) -> &'a str {
    let at = code
        .find(sig)
        .unwrap_or_else(|| panic!("`{sig}` is gone from {SRC} — the balance is chain-anchored again"));
    let open = code[at..]
        .find('{')
        .map(|i| at + i)
        .unwrap_or_else(|| panic!("`{sig}` has no body brace"));
    let bytes = code.as_bytes();
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for i in open..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &code[at..=i];
                }
            }
            _ => {}
        }
    }
    panic!("`{sig}` body never closes — the window would have run to EOF")
}

fn need(hay: &str, needle: &str, why: &str) -> Result<(), String> {
    if squeeze(hay).contains(&squeeze(needle)) {
        Ok(())
    } else {
        Err(format!("{why}\n\n(looked for `{needle}` in)\n\n{hay}"))
    }
}

fn at_of(hay: &str, needle: &str, why: &str) -> Result<usize, String> {
    hay.find(needle).ok_or_else(|| format!("{why}\n\n(looked for `{needle}` in)\n\n{hay}"))
}

// ---------------------------------------------------------------------------------------------
// The checks, as pure functions over the source text, so the non-vacuity test can run the very
// same code against mutated source and require a rejection.
// ---------------------------------------------------------------------------------------------

/// THE LEDGER EXISTS, probes all THREE shapes, and — the part the old guard missed — actually ADDS
/// each shape's amount inside that shape's own branch. Missing one is a silently-low balance for
/// whoever happens to hold that shape, which is exactly the defect, one shape over.
fn check_every_shape_is_summed(code: &str) -> Result<(), String> {
    let body = fn_body(code, LEDGER_SIG);

    let mut probes: Vec<(usize, &str, &str)> = Vec::new();
    for (probe, shape) in SHAPES {
        let at = at_of(
            body,
            probe,
            &format!("the ledger no longer looks up {shape}. A shape it never probes is a shape it \
                      never counts, which is the defect this function exists to close:"),
        )?;
        probes.push((at, probe, shape));
    }

    // ORDERING, pinned by comparing POSITIONS (not by prose): tip, then child, then root. The keys
    // narrow in that direction and each branch `continue`s on a hit, so probing the root first
    // would answer a spine-tip coin with the ROOT's amount — a wrong figure, not a missing one.
    if !(probes[0].0 < probes[1].0 && probes[1].0 < probes[2].0) {
        return Err(format!(
            "the shapes are no longer probed narrowest-key-first ({} at {}, {} at {}, {} at {}). \
             Because the first hit wins, a wider probe placed first answers a narrower coin with \
             the wrong amount:\n\n{body}",
            probes[0].1, probes[0].0, probes[1].1, probes[1].0, probes[2].1, probes[2].0
        ));
    }

    // Each probe governs the text from itself up to the NEXT probe (the last runs to the closing
    // brace of the loop body, which the brace-matched window already bounds). Requiring the
    // accumulation *inside that segment* is what makes this a per-shape pin: a surviving sibling's
    // sum elsewhere in the function cannot stand in for the one that was deleted.
    for (i, (start, probe, shape)) in probes.iter().enumerate() {
        let end = probes.get(i + 1).map(|p| p.0).unwrap_or(body.len());
        let seg = &body[*start..end];
        need(
            seg,
            ACCUM,
            &format!(
                "the ledger looks {shape} up through `{probe}` and then does NOT add its amount. \
                 That is defect #152 verbatim, one shape over: the probe is still there for a \
                 reviewer to see, the balance silently under-reports for whoever holds this shape. \
                 A guard that pins only the probe is green for this bug — which is why the pin is \
                 on the accumulation. Note `+=`: with `=`, a second receive of the same contract \
                 replaces the first instead of adding to it, which is the same defect by \
                 arithmetic instead of by omission."
            ),
        )?;
        // The first two branches must short-circuit, or a coin that answers to two loaders is
        // counted twice — the over-reporting mirror of #152.
        if i < 2 {
            need(
                seg,
                "continue;",
                &format!(
                    "the `{probe}` branch no longer `continue`s. Without it a coin that answers to \
                     more than one loader is summed more than once."
                ),
            )?;
        }
    }
    Ok(())
}

/// NON-VACUITY OF THE AMOUNT: it must come from the bundle's `rgb` half — the CONSIGNMENT-derived
/// figure the receiver validated — over CONFIRMED, non-duplicate coins only.
fn check_the_amount_is_consignment_derived(code: &str) -> Result<(), String> {
    let body = fn_body(code, LEDGER_SIG);
    need(
        body,
        "r.amount",
        "the ledger no longer reads the bundle's `rgb` half. Any other source for the amount is a \
         number the sender chose:",
    )?;
    need(body, "r.contract_id", "the ledger no longer keys by contract id:")?;

    let filter = at_of(
        body,
        "CoinStatus::CONFIRMED",
        "the ledger no longer restricts to confirmed coins — a spent row would inflate it:",
    )?;
    need(
        body,
        "duplicate_index == 0",
        "the ledger no longer excludes duplicate rows — a duplicated coin would be counted twice:",
    )?;
    // ORDERING, by position: the filter is worthless if it does not run BEFORE the summing loop.
    let first_probe = at_of(body, SHAPES[0].0, "the first probe is gone:")?;
    if filter > first_probe {
        return Err(format!(
            "the confirmed/non-duplicate filter ({filter}) no longer precedes the summing loop \
             ({first_probe}), so it filters nothing that gets summed:\n\n{body}"
        ));
    }
    Ok(())
}

/// THE CONSUMER. A ledger nothing reads is a ledger that proves nothing — and a ledger that is read
/// and then discarded is worse, because the call site looks right.
fn check_the_public_balance_consumes_the_ledger(code: &str) -> Result<(), String> {
    let body = fn_body(code, PUBLIC_SIG);
    let call = at_of(
        body,
        "ledger_token_balances()",
        "`get_token_balances` no longer consults the ledger, so it is back to reporting only what \
         the chain-anchored engine can settle:",
    )?;
    // ORDERING, by position: the ledger needs the wallet DB and must therefore be computed BEFORE
    // the engine is borrowed — the `block_in_place` closure cannot await inside.
    let closure = at_of(body, "block_in_place", "the engine closure is gone:")?;
    if call > closure {
        return Err(format!(
            "the ledger is computed at {call}, inside/after the `block_in_place` closure at \
             {closure}. It must be awaited BEFORE the engine is borrowed — the closure cannot \
             await the wallet DB the ledger reads:\n\n{body}"
        ));
    }
    // The per-asset figure must actually come OUT of the ledger map. `.max(settled)` alone is
    // satisfied by `0u64.max(settled)` — ledger computed, ledger ignored, guard green.
    need(
        body,
        "ledger.get(&asset_id)",
        "the per-asset balance no longer reads the ledger map. Calling `ledger_token_balances()` \
         and then not using its answer reports the chain-anchored figure again:",
    )?;
    // Combined with `max`, deliberately: on-chain material the ledger does not track (an exited
    // allocation now living on a plain UTXO) must still be reported.
    need(
        body,
        "led.max(settled)",
        "the ledger figure no longer combines with the engine's settled figure via `max`. \
         Replacing it outright would drop an EXITED allocation, which lives on a plain UTXO and is \
         exactly what the engine tracks and the ledger does not:",
    )?;
    need(
        body,
        "future.max(led)",
        "`total` no longer accounts for the ledger, so the pending/total figure under-reports \
         off-chain material for exactly the same reason `balance` used to:",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The invariants, against the real tree.
// ---------------------------------------------------------------------------------------------

fn source() -> String {
    code_only(&read(SRC))
}

#[test]
fn the_ledger_balance_sums_every_shape_an_allocation_can_live_in() {
    if let Err(e) = check_every_shape_is_summed(&source()) {
        panic!("{e}");
    }
}

#[test]
fn the_ledger_reads_the_consignment_derived_amount() {
    if let Err(e) = check_the_amount_is_consignment_derived(&source()) {
        panic!("{e}");
    }
}

#[test]
fn the_public_balance_consumes_the_ledger() {
    if let Err(e) = check_the_public_balance_consumes_the_ledger(&source()) {
        panic!("{e}");
    }
}

/// The window property, asserted rather than assumed. The previous version took `at + 2_500` bytes
/// of a ~1_500-character function; the excess was the head of the NEXT function, which means every
/// assertion above could have been satisfied by text belonging to code the pin says nothing about.
#[test]
fn the_scan_windows_stop_at_each_functions_own_closing_brace() {
    let code = source();

    let ledger = fn_body(&code, LEDGER_SIG);
    assert!(
        ledger.ends_with('}'),
        "the ledger window does not end on a closing brace: {ledger}"
    );
    assert!(
        !ledger.contains(NEXT_FN),
        "the ledger window reaches into `{NEXT_FN}`. A window that overshoots lets the wrong \
         function's text satisfy assertions about this one:\n\n{ledger}"
    );
    assert!(
        !ledger.contains(PUBLIC_SIG),
        "the ledger window reaches into `{PUBLIC_SIG}`:\n\n{ledger}"
    );

    let public = fn_body(&code, PUBLIC_SIG);
    assert!(public.ends_with('}'), "the public window does not end on a closing brace: {public}");
    assert!(
        !public.contains(LEDGER_SIG),
        "the `get_token_balances` window reaches into the ledger's DEFINITION, so its assertions \
         could be satisfied by the ledger's body instead of by the consumer:\n\n{public}"
    );
}

// ---------------------------------------------------------------------------------------------
// Non-vacuity. A guard that cannot fail is decoration.
// ---------------------------------------------------------------------------------------------

/// Replace the `n`-th (0-based) occurrence of `pat` in `region`. The three accumulations are
/// textually identical, so mutating exactly ONE of them is the whole point.
fn replace_nth(region: &str, pat: &str, n: usize, repl: &str) -> String {
    let mut start = 0usize;
    for idx in 0..=n {
        let found = start
            + region[start..].find(pat).unwrap_or_else(|| {
                panic!("mutation target `{pat}` has no occurrence {n} in the scanned region")
            });
        if idx == n {
            return format!("{}{repl}{}", &region[..found], &region[found + pat.len()..]);
        }
        start = found + pat.len();
    }
    unreachable!()
}

/// Mutations are applied INSIDE the named function's brace-matched body, never file-wide: a
/// file-wide `replace` would hit an unrelated `continue;` or `tesr::load(` elsewhere in
/// `tokens.rs` and the "mutant" would leave the guarded property untouched — a non-vacuity test
/// that is itself vacuous.
fn mutate_nth(code: &str, sig: &str, pat: &str, n: usize, repl: &str) -> String {
    let start = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let body = fn_body(code, sig);
    let mutated = replace_nth(body, pat, n, repl);
    assert_ne!(mutated, body, "mutation `{pat}` -> `{repl}` changed nothing");
    format!("{}{mutated}{}", &code[..start], &code[start + body.len()..])
}

fn mutate(code: &str, sig: &str, pat: &str, repl: &str) -> String {
    mutate_nth(code, sig, pat, 0, repl)
}

/// Every occurrence, still scoped to the one function body.
fn mutate_all(code: &str, sig: &str, pat: &str, repl: &str) -> String {
    let start = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let body = fn_body(code, sig);
    let mutated = body.replace(pat, repl);
    assert_ne!(mutated, body, "mutation `{pat}` -> `{repl}` changed nothing");
    format!("{}{mutated}{}", &code[..start], &code[start + body.len()..])
}

/// Every mutation listed here is a real, plausible edit to `tokens.rs`. The first one is not
/// hypothetical: an adversarial review applied it to the working tree and the previous version of
/// this file reported **3 passed**. Each entry must now be REJECTED by the same checker that runs
/// against the real source above. (The mutants are text, not compiled code; a couple of them would
/// not build. That is fine and deliberate — the point is that the property, not the description,
/// is what the assertion is attached to.)
#[test]
fn guard_catches_every_mutation_it_was_written_for() {
    type Check = fn(&str) -> Result<(), String>;
    let sum: Check = check_every_shape_is_summed;
    let amount: Check = check_the_amount_is_consignment_derived;
    let consumer: Check = check_the_public_balance_consumes_the_ledger;

    let code = source();

    // Sanity: the unmutated source must PASS all three, or "rejected" below proves nothing.
    for (name, chk) in [("sum", sum), ("amount", amount), ("consumer", consumer)] {
        if let Err(e) = chk(&code) {
            panic!("the real source fails check `{name}` — fix the source, not the guard:\n{e}");
        }
    }

    let cases: Vec<(&str, String, Check)> = vec![
        // THE PROVEN SURVIVOR: keep the probe, delete the sum. Green under the old guard.
        (
            "spine_tip_probed_but_never_summed",
            mutate_nth(&code, LEDGER_SIG, ACCUM, 0, "let _ = r.amount;"),
            sum,
        ),
        (
            "child_probed_but_never_summed",
            mutate_nth(&code, LEDGER_SIG, ACCUM, 1, "let _ = r.amount;"),
            sum,
        ),
        (
            "root_probed_but_never_summed",
            mutate_nth(&code, LEDGER_SIG, ACCUM, 2, "let _ = r.amount;"),
            sum,
        ),
        // The same defect by arithmetic: the second receive of a contract replaces the first.
        (
            "child_overwrites_instead_of_accumulating",
            mutate_nth(
                &code,
                LEDGER_SIG,
                ACCUM,
                1,
                "*out.entry(r.contract_id.clone()).or_default() = r.amount;",
            ),
            sum,
        ),
        // The shape is not looked up at all (the property the old guard did cover — preserved).
        (
            "spine_tip_shape_dropped_entirely",
            mutate(&code, LEDGER_SIG, "load_spine_tip(", "load_nothing_at_all("),
            sum,
        ),
        // Short-circuit removed: a coin answering two loaders is counted twice.
        (
            "tip_branch_no_longer_short_circuits",
            mutate(&code, LEDGER_SIG, "continue;", "{}"),
            sum,
        ),
        // Root probed before the tip: the first hit wins, so a tip coin gets the ROOT's amount.
        ("root_probed_before_the_tip", {
            let m = mutate(&code, LEDGER_SIG, "tesr::load_spine_tip(", "tesr::__swapped(");
            let m = mutate(&m, LEDGER_SIG, "tesr::load(", "tesr::load_spine_tip(");
            mutate(&m, LEDGER_SIG, "tesr::__swapped(", "tesr::load(")
        }, sum),
        // Unconfirmed / duplicate coins summed.
        (
            "spent_and_duplicate_coins_are_summed",
            mutate(
                &code,
                LEDGER_SIG,
                "c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0",
                "true",
            ),
            amount,
        ),
        // Sender-declared amount instead of the consignment-derived one.
        (
            "amount_no_longer_comes_from_the_rgb_half",
            mutate_all(&code, LEDGER_SIG, "r.amount", "declared_amount"),
            amount,
        ),
        // Ledger consulted and then thrown away — satisfies the OLD `.max(settled)` pin.
        (
            "ledger_computed_then_ignored",
            mutate(&code, PUBLIC_SIG, "ledger.get(&asset_id).copied().unwrap_or(0)", "0u64"),
            consumer,
        ),
        // Ledger not consulted at all.
        (
            "ledger_never_called",
            mutate(
                &code,
                PUBLIC_SIG,
                "let ledger = self.ledger_token_balances().await?;",
                "let ledger: std::collections::HashMap<String, u64> = Default::default();",
            ),
            consumer,
        ),
        // Ledger moved inside the engine closure (ORDERING, caught by comparing positions).
        ("ledger_computed_inside_the_engine_closure", {
            let m =
                mutate(&code, PUBLIC_SIG, "let ledger = self.ledger_token_balances().await?;", "");
            mutate(
                &m,
                PUBLIC_SIG,
                "tokio::task::block_in_place(|| -> Result<Vec<TokenBalance>> {",
                "tokio::task::block_in_place(|| -> Result<Vec<TokenBalance>> { let ledger = \
                 self.ledger_token_balances().await?;",
            )
        }, consumer),
        // `balance` stops combining with the engine's settled figure.
        (
            "balance_drops_the_engines_settled_figure",
            mutate(&code, PUBLIC_SIG, "balance: led.max(settled)", "balance: led"),
            consumer,
        ),
        // `total` under-reports off-chain material for the original #152 reason.
        (
            "total_ignores_the_ledger",
            mutate(&code, PUBLIC_SIG, "total: future.max(led)", "total: future"),
            consumer,
        ),
    ];

    for (tag, mutant, chk) in cases {
        assert_ne!(mutant, code, "mutation `{tag}` did not change the source");
        assert!(
            chk(&mutant).is_err(),
            "planted mutation `{tag}` was NOT caught — the guard is pinning a description this \
             mutation satisfies, not the property it names"
        );
    }
}
