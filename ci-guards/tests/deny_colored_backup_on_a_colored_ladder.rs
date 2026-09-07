//! **[D35 / RGB-1 → ONE COIN SHAPE] A laddered coin conveys NO flat backup, and BOTH acceptance
//! paths must refuse one — on either lane.**
//!
//! # Where this guard came from
//!
//! It used to pin a LANE RULE: a coloured ladder's flat backups had to be plain (an OP_RETURN on a
//! hop backup over `F` let a prior owner re-assign the allocation — capture, not griefing), while a
//! plain ladder's flat backups had to carry no consignment (its tiers carry no transition, so its
//! exit BURNS the allocation). The union of the two lanes' shapes was the surface, and the rule was
//! keyed on `is_colored()` so that each lane got its own refusal.
//!
//! # What replaced it
//!
//! There is no flat backup beside a ladder at all. The deposit-time `tx1` is gone (the ladder
//! `T, X_0, S_0` is signed at the FIRST MEMPOOL SIGHTING of the funding transaction instead), the
//! per-hop `create_backup_tx_to_receiver` is deleted, and a transfer conveys
//! `backup_transactions: []`. The receiver's census is exactly `se_num_sigs == tiers + superseded`
//! with the flat term 0, so ANY conveyed flat backup is a co-sign the census cannot account for —
//! and, worse, a matured spend of `F` that a prior owner keeps: plain, it burns a carrier's
//! allocation the moment it matures; with an opret, it re-assigns it. Neither shape is admitted on
//! either lane. `verify_flat_backup_lane` therefore refuses every NON-EMPTY vector and reads the
//! lane only to NAME it in the refusal.
//!
//! The same rule holds at every level of the tree: a conveyed child, tail, stub or spine tip carries
//! `parent_flat_backups`, and `refuse_conveyed_flat_backups` refuses a non-empty one by name.
//!
//! **What is pinned here is the CONSTRUCTION, not the description.** Every assertion below reads
//! comment-stripped source, because the last four times a pin like this went green while the code
//! was wrong, it matched a sentence I had written about the code. A refusal message is not evidence
//! that the refusal happens — so the emptiness rule is pinned as "exactly one `Ok(` in the body, and
//! it sits inside the `is_empty()` block", and the non-vacuity test plants a coloured-lane `Ok`
//! escape and requires it to be caught.

use std::path::PathBuf;

/// **[D64] Strip whole-line AND TRAILING `//` comments.**
///
/// Every stripper in this crate filtered only `l.trim_start().starts_with("//")`, so a TRAILING
/// comment survived — and a trailing comment is enough to defeat any substring pin in the file. An
/// adversarial pass proved it on the real tree with the mutation a guard printed in its own header:
///
/// ```ignore
/// println!( // was: return Err(anyhow::anyhow!(
///     "a COLOURED spine batch must not be driven through the PLAIN batch driver. …"
/// );
/// ```
///
/// The refusal is gone, the message is intact, and `arm.contains("return Err(")` is satisfied by the
/// ANNOTATION. The guard stayed green.
///
/// `//` inside a string literal must not be treated as a comment (URLs, `"http://…"`, and the
/// `"//"` in this very doc), so the scan tracks `"` and escapes.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let b = line.as_bytes();
        let (mut i, mut in_str, mut esc, mut cut) = (0usize, false, false, line.len());
        while i + 1 <= b.len() {
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
            } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
                cut = i;
                break;
            }
            i += 1;
        }
        out.push_str(line[..cut].trim_end());
        out.push('\n');
    }
    out
}

/// Blank the CONTENTS of every double-quoted string literal (offsets preserved), so that a refusal
/// message quoting `Ok(())` or `is_colored()` can never satisfy a construction pin.
fn blank_strings(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() {
                if b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if b[j] == b'"' {
                    break;
                }
                j += 1;
            }
            let end = j.min(b.len());
            for k in (i + 1)..end {
                if out[k] != b'\n' {
                    out[k] = b' ';
                }
            }
            i = end.saturating_add(1);
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking preserves UTF-8")
}

fn repo(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// Source with every `//` comment removed. Doc comments are prose about the code; a pin that can be
/// satisfied by prose pins nothing.
fn code_only(src: &str) -> String {
    strip_comments(src)
}

/// The body of a column-0 item, ending at the next column-0 `pub fn`/`fn`/`pub async fn`/`async fn`
/// — a real symbol, never a byte count and never end-of-file.
fn fn_body(code: &str, sig: &str) -> String {
    let at = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let rest = &code[at..];
    let end = ["\npub fn ", "\nfn ", "\npub async fn ", "\nasync fn ", "\npub(crate) fn "]
        .iter()
        .filter_map(|m| rest[1..].find(m).map(|i| i + 1))
        .min()
        .unwrap_or_else(|| panic!("`{sig}` has no following column-0 item to bound its body"));
    rest[..end].to_string()
}

/// The brace-matched `{ … }` block opening at or after `from`, as `(inner, open, close)`. Strings
/// must already be blanked by the caller.
fn block_after(code: &str, from: usize) -> Option<(&str, usize, usize)> {
    let b = code.as_bytes();
    let open = (from..b.len()).find(|&i| b[i] == b'{')?;
    let mut depth = 0usize;
    for i in open..b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&code[open + 1..i], open, i));
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// THE RULE, as a pure function over source text — so the non-vacuity test can drive it with
// planted shapes as well as with the real tree.
// ---------------------------------------------------------------------------------------------

/// Decide whether `body` (the comment-stripped text of `verify_flat_backup_lane`) refuses EVERY
/// non-empty vector, regardless of lane.
fn lane_rule_refuses_any_flat_backup(body: &str) -> Result<(), String> {
    let code = blank_strings(body);

    // (1) The ONLY admission is emptiness: an `is_empty()` test on the backups…
    let probe = code
        .find("backups.is_empty()")
        .ok_or_else(|| format!("`verify_flat_backup_lane` no longer tests `backups.is_empty()`:\n{body}"))?;
    let (inner, _open, close) = block_after(&code, probe)
        .ok_or_else(|| "the `is_empty()` test opens no block".to_string())?;
    if !inner.contains("Ok(") {
        return Err(format!(
            "the `is_empty()` block does not return `Ok` — an empty vector is the ONLY legitimate \
             shape and must be admitted there:\n{body}"
        ));
    }
    // …and the head of the test is `if backups.is_empty()`, not `if !backups.is_empty()`: an
    // inverted probe admits exactly the vectors it exists to refuse.
    let head_start = code[..probe].rfind('\n').map_or(0, |n| n + 1);
    let head = code[head_start..probe].trim_start();
    if head != "if " {
        return Err(format!(
            "the emptiness test is not `if backups.is_empty()` — the line reads `{head}…`. The \
             polarity is the property: with `if !` the `Ok` inside the block admits every \
             NON-empty vector:\n{body}"
        ));
    }

    // (2) EXACTLY ONE `Ok(` in the whole body, and it is the one inside the emptiness block. A
    //     second `Ok(` anywhere — a coloured-lane escape, a plain-lane escape, a "parses fine" arm —
    //     is a non-empty vector admitted on some lane.
    let oks = code.matches("Ok(").count();
    if oks != 1 {
        return Err(format!(
            "`verify_flat_backup_lane` has {oks} `Ok(` sites; exactly one is allowed, inside the \
             `is_empty()` block. Every other `Ok` admits a NON-empty flat vector on some lane — a \
             co-sign the census `tiers + superseded` cannot account for, and a matured spend of \
             `F` a prior owner keeps:\n{body}"
        ));
    }

    // (3) After the emptiness block, the body REFUSES — `Err(` — and nothing else.
    let tail = &code[close + 1..];
    if !tail.contains("Err(") {
        return Err(format!(
            "after the emptiness test `verify_flat_backup_lane` does not produce an `Err` — a \
             non-empty vector falls through to something that is not a refusal:\n{body}"
        ));
    }

    // (4) The lane is read — to NAME it in the refusal, so that a sender can tell which coin was
    //     refused — but it must never open a branch that ends in `Ok`; (2) already guarantees that,
    //     and this pins that the read is still there so a refusal stays diagnosable.
    if !code.contains("bundle.is_colored()") {
        return Err(format!(
            "`verify_flat_backup_lane` no longer reads the lane (`bundle.is_colored()`). It is read \
             only to NAME the lane in the refusal, but a refusal that cannot say which shape it \
             refused sends the sender to the wrong remedy:\n{body}"
        ));
    }
    Ok(())
}

/// THE RULE ITSELF, read off the real code.
#[test]
fn the_lane_rule_refuses_every_non_empty_flat_vector() {
    let code = code_only(&repo("clients/libs/rust/src/tesr.rs"));
    let body = fn_body(&code, "pub fn verify_flat_backup_lane(");
    lane_rule_refuses_any_flat_backup(&body).unwrap_or_else(|e| panic!("{e}"));
}

/// NON-VACUITY. Each planted shape is the old lane rule creeping back in one form or another; the
/// checker must refuse every one, and must accept the shipped shape.
#[test]
fn the_lane_rule_check_rejects_every_lane_escape() {
    const SHIPPED: &str = r#"
pub fn verify_flat_backup_lane(bundle: &TesrBundle, backups: &[BackupTx]) -> Result<()> {
    if backups.is_empty() {
        return Ok(());
    }
    let lane = if bundle.is_colored() { "COLOURED" } else { "plain" };
    Err(anyhow::anyhow!("refusing a {lane} ladder conveyed with {} flat backup(s)", backups.len()))
}
"#;
    assert_eq!(
        lane_rule_refuses_any_flat_backup(SHIPPED),
        Ok(()),
        "the shipped shape must pass, or the rejections below prove nothing"
    );

    let cases: [(&str, &str, &str); 5] = [
        (
            // The old coloured-lane rule: plain backups admitted on a coloured ladder.
            "coloured_lane_admits_plain_backups",
            r#"
pub fn verify_flat_backup_lane(bundle: &TesrBundle, backups: &[BackupTx]) -> Result<()> {
    if backups.is_empty() {
        return Ok(());
    }
    if bundle.is_colored() && backups.iter().all(|b| b.rgb_consignment.is_none()) {
        return Ok(());
    }
    Err(anyhow::anyhow!("refused"))
}
"#,
            "`Ok(` sites",
        ),
        (
            // The old plain-lane rule: plain sats on a plain ladder admitted.
            "plain_lane_admits_plain_sats",
            r#"
pub fn verify_flat_backup_lane(bundle: &TesrBundle, backups: &[BackupTx]) -> Result<()> {
    if backups.is_empty() {
        return Ok(());
    }
    let colored = bundle.is_colored();
    for b in backups {
        if !colored && b.rgb_consignment.is_some() {
            return Err(anyhow::anyhow!("burns the allocation"));
        }
    }
    Ok(())
}
"#,
            "`Ok(` sites",
        ),
        (
            // Inverted polarity: the `Ok` is now on the NON-empty branch.
            "emptiness_test_inverted",
            r#"
pub fn verify_flat_backup_lane(bundle: &TesrBundle, backups: &[BackupTx]) -> Result<()> {
    if !backups.is_empty() {
        return Ok(());
    }
    let lane = if bundle.is_colored() { "COLOURED" } else { "plain" };
    Err(anyhow::anyhow!("refusing a {lane} ladder"))
}
"#,
            "polarity",
        ),
        (
            // The refusal demoted to a message: same words, `Ok` returned. A `contains("refusing")`
            // pin passes this; the construction pin cannot (two `Ok(` and no `Err(`).
            "refusal_is_only_a_message",
            r#"
pub fn verify_flat_backup_lane(bundle: &TesrBundle, backups: &[BackupTx]) -> Result<()> {
    if backups.is_empty() {
        return Ok(());
    }
    let lane = if bundle.is_colored() { "COLOURED" } else { "plain" };
    println!("refusing a {lane} ladder conveyed with {} flat backup(s): Err(", backups.len());
    Ok(())
}
"#,
            "`Ok(` sites",
        ),
        (
            // The lane no longer read at all: the refusal cannot say which shape it refused.
            "lane_not_read",
            r#"
pub fn verify_flat_backup_lane(bundle: &TesrBundle, backups: &[BackupTx]) -> Result<()> {
    if backups.is_empty() {
        return Ok(());
    }
    Err(anyhow::anyhow!("refusing a ladder conveyed with {} flat backup(s)", backups.len()))
}
"#,
            "no longer reads the lane",
        ),
    ];
    for (tag, src, expected) in cases {
        match lane_rule_refuses_any_flat_backup(src) {
            Ok(()) => panic!("planted shape `{tag}` was ACCEPTED — the lane rule is back"),
            Err(e) => assert!(
                e.contains(expected),
                "planted shape `{tag}` was refused for the WRONG reason (expected `{expected}`):\n{e}"
            ),
        }
    }
}

/// BOTH acceptance paths. R′ is the claim path AND the SSP's pre-payment census; the pre-pay one is
/// the path that authorises an irreversible Lightning leg, so a rule enforced on only one of them
/// leaves the payer holding a coin an ancestor can take. Both run the rule over the conveyed
/// `backup_transactions` vector (the pre-pay path's separate `backup_group` is gone with the flat
/// chain it grouped), and both refuse branch material — an exit branch beside a ladder describes a
/// coin rooted off-chain, which no laddered coin is.
#[test]
fn both_acceptance_paths_refuse_flat_backups_and_branch_material() {
    let code = code_only(&repo("clients/libs/rust/src/transfer_receiver.rs"));
    const CALL: &str = "crate::tesr::verify_flat_backup_lane(&bundle, &transfer_msg.backup_transactions)";
    let calls = code.matches(CALL).count();
    assert!(
        calls >= 2,
        "`{CALL}` has {calls} call site(s) in transfer_receiver.rs — R′ is the claim path AND \
         `prepay_flat_census`, and both must run the rule over the CONVEYED vector"
    );
    // Both call sites must be REFUSALS: the result propagated, never discarded.
    for (at, _) in code.match_indices(CALL) {
        let stmt_end = code[at..].find(';').map(|d| at + d).expect("the call is a statement");
        let stmt = &code[at..stmt_end];
        assert!(
            stmt.trim_end().ends_with('?'),
            "a `verify_flat_backup_lane` call in transfer_receiver.rs does not propagate its \
             refusal (`?`):\n{stmt}"
        );
    }
    let branch_calls = code
        .matches("refuse_branch_material(")
        .count()
        .saturating_sub(usize::from(code.contains("fn refuse_branch_material(")));
    assert!(
        branch_calls >= 2,
        "`refuse_branch_material(` has {branch_calls} call site(s) in transfer_receiver.rs; both \
         acceptance paths must refuse `branch_txs` / `terminal_parents` beside a ladder"
    );
    // ...and the branch refusal is a refusal on BOTH fields, expressed as `return Err`.
    let body = fn_body(&code, "fn refuse_branch_material(");
    let scrubbed = blank_strings(&body);
    assert!(
        scrubbed.contains("branch_txs.is_empty()") && scrubbed.contains("terminal_parents.is_empty()"),
        "`refuse_branch_material` no longer tests both `branch_txs` and `terminal_parents`:\n{body}"
    );
    assert!(
        scrubbed.contains("return Err("),
        "`refuse_branch_material` does not REFUSE — a message is not a refusal:\n{body}"
    );
}

/// EVERY LEVEL OF THE TREE. A conveyed child, tail, stub and spine tip each carry the parent's
/// `parent_flat_backups`; the vector must be refused when non-empty at every one of those
/// adoptions, by the one function that says why.
#[test]
fn every_tree_level_refuses_a_conveyed_flat_vector() {
    let code = code_only(&repo("clients/libs/rust/src/tesr.rs"));
    let scrubbed = blank_strings(&code);
    let calls: Vec<usize> = scrubbed
        .match_indices("refuse_conveyed_flat_backups(")
        .map(|(i, _)| i)
        .filter(|i| !scrubbed[..*i].ends_with("fn "))
        .collect();
    assert!(
        calls.len() >= 4,
        "`refuse_conveyed_flat_backups(` has {} call site(s) in tesr.rs; the conveyed child, tail, \
         stub and spine-tip adoptions must each run it over `parent_flat_backups`",
        calls.len()
    );
    for at in calls {
        let stmt_end = scrubbed[at..].find(';').map(|d| at + d).expect("the call is a statement");
        let stmt = &scrubbed[at..stmt_end];
        assert!(
            stmt.contains("parent_flat_backups") && stmt.trim_end().ends_with('?'),
            "a `refuse_conveyed_flat_backups` call does not run over `parent_flat_backups` with \
             its refusal propagated:\n{stmt}"
        );
    }
    let body = fn_body(&code, "pub fn refuse_conveyed_flat_backups(");
    let b = blank_strings(&body);
    assert!(
        b.contains("!backups.is_empty()") && b.contains("return Err("),
        "`refuse_conveyed_flat_backups` no longer refuses a NON-empty vector with `return Err`:\n{body}"
    );
    assert_eq!(
        b.matches("Ok(").count(),
        1,
        "`refuse_conveyed_flat_backups` admits more than the empty vector:\n{body}"
    );
    // The census constants say the same thing in numbers: no flat term at either level.
    assert!(
        code.contains("pub const PARENT_V2_BASELINE: u32 = 0;")
            && code.contains("pub const CHILD_V2_BASELINE: u32 = 0;"),
        "the flat-backup baselines are no longer both 0 — the census `se_num_sigs == tiers + \
         superseded` has grown a flat term again"
    );
}

/// THE SENDER'S HALF. `execute_ex` conveys an EMPTY vector by construction and co-signs no flat
/// backup: the per-hop `create_backup_transactions` / `create_backup_tx_to_receiver` are deleted.
/// (The receiver refusing what the sender still produced would be a transfer that can never be
/// claimed, with the sender's coin stuck IN_TRANSFER.)
#[test]
fn the_sender_conveys_an_empty_flat_vector() {
    let code = code_only(&repo("clients/libs/rust/src/transfer_sender.rs"));
    for gone in ["create_backup_transactions(", "create_backup_tx_to_receiver("] {
        assert!(
            !code.contains(gone),
            "`{gone}` is back in transfer_sender.rs — a laddered coin conveys no flat backup, and \
             every receiver refuses one by name"
        );
    }
    let at = code
        .find("let backup_transactions")
        .expect("`execute_ex` no longer binds the conveyed `backup_transactions`");
    let stmt_end = code[at..].find(';').map(|d| at + d).expect("the binding is a statement");
    let stmt = &code[at..stmt_end];
    assert!(
        stmt.contains("Vec::new()") || stmt.contains("vec![]"),
        "the conveyed `backup_transactions` is not empty by construction:\n{stmt}"
    );
}

/// THE UNION RULE IS GONE, not merely bypassed. Leaving the old predicate in place would let a
/// later reader restore the call and re-introduce a lane that admits a flat backup.
#[test]
fn the_union_keyed_predicate_no_longer_exists() {
    let src = repo("clients/libs/rust/src/transfer_receiver.rs");
    assert!(
        !src.contains("first_rgb_envelope_on_a_laddered_message"),
        "the pre-correction predicate is still present. It keyed on `a ladder is present` and \
         sorted flat backups into admissible and not — there is no admissible flat backup any more"
    );
    assert!(
        !src.contains("backup_group"),
        "the pre-pay census has grown a `backup_group` again — a flat backup chain has nothing to \
         group beside a ladder, and the rule runs over `transfer_msg.backup_transactions` directly"
    );
}
