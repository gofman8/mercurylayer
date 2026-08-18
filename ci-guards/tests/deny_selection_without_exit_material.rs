//! **[#145] A coin may not be offered to a payment unless this wallet holds material to EXIT it.**
//!
//! Every filter in `payment_coins` asks whether a coin is ELIGIBLE — confirmed, not a duplicate, not
//! an RGB carrier, worth more than its own renewal fee. None of them asks the separate question of
//! whether the wallet can actually *exit or convey* it:
//!
//! * a laddered coin's exit material is its bundle (`tesr-` root, `ctesr-` child, `spinetip-` tip);
//! * an un-laddered coin's is its flat backup chain;
//! * a coin with **neither** is a slot the SE knows about, that this wallet holds a key for, and
//!   that it can neither exit unilaterally nor hand on — most often a derived child slot whose split
//!   failed after the slot was minted.
//!
//! That third shape was offered to selection, and the failure surfaced at the FAR END of the
//! payment: `transfer_sender` reached for the flat backup rows, found none, and refused. `chaos22`'s
//! oracle could only class it as an unclassified breach — the wallet reported balance, the planner
//! promised it, and the send died. The remedy is upstream: the coin is never offered, so a different
//! coin funds the payment and nobody meets the refusal.
//!
//! # Why the shape of the check matters more than the check
//!
//! `parent_shape` reaches `Unladdered` through THREE CONSECUTIVE ABSENCES — no tip row, no child
//! row, no root row. This repo has already been bitten once by treating that as a positive answer
//! (the spine tip fell through all three and was routed as un-laddered, at the wrong floor, to the
//! [B1]-unsafe plain split). "Un-laddered" is a claim about a coin that still has a flat backup
//! chain; a coin with no chain either is not un-laddered, it is un-exitable, and the two must not
//! share a route.
//!
//! # Why this guard was rewritten (the two pins that a mutation survived)
//!
//! An adversarial review applied the following two mutations to the real tree and watched the
//! previous version of this file stay GREEN. Both failures are of the same species as the bug the
//! guard exists to prevent: a *description* was pinned where a *property* was meant.
//!
//! * **A3 — a refusal pinned as a POSITION, not as a refusal.** The old
//!   `the_flat_sender_refuses_a_spine_tip_itself` found `load_spine_tip(` and
//!   `create_backup_transactions(` and asserted `refusal < rows`. It never asserted that anything
//!   was *refused*. Replacing `return Err(anyhow!("statechain id {statechain_id} is a SPINE TIP …"))`
//!   with a `println!` carrying the identical text left the guard green (5 passed) — and a spine tip
//!   then walked the flat lane with no error on either side, which is the money-loss shape the
//!   comment above the refusal describes. Worse, the scan window was `&code[at..]` to END OF FILE, so
//!   both anchors could have been satisfied by any LATER function's text. A guard whose window
//!   overshoots is pinning the file, not the function.
//!
//! * **A4 — an ordering pinned as a COUNT.** The old
//!   `every_plain_split_route_proves_its_material_first` asserted `has_exit_material` appeared at
//!   least three times. Three call sites that do not GATE anything satisfy that: keep all three and
//!   make two of them `let _ = self.has_exit_material(&id).await?;` and the guard stayed green, while
//!   the batch lane and `ensure_exact_coin` happily routed a materialless slot to the plain split.
//!   The name of the test says "proves its material FIRST" — a count expresses neither "proves" nor
//!   "first".
//!
//! The rules the rewrite follows, and which any future edit to this file must keep:
//!
//! 1. a guard asserting a REFUSAL pins `return Err` / `?` / `bail!` — never the message text, which a
//!    `println!` with the identical text satisfies;
//! 2. a guard asserting an ORDERING compares the POSITIONS of the two ordered things;
//! 3. a guard asserting a GATE pins the call in the CONDITION of an `if`, with the right polarity,
//!    and pins what the losing branch DOES (`continue`, a rejection, the exclusion list);
//! 4. every scan window terminates on a REAL symbol that exists in the stripped source — never
//!    `unwrap_or(N)`, never a fixed byte count, never "to end of file". A missing anchor is a loud
//!    failure that says "re-derive this guard", not a silently wider window;
//! 5. every property is checked by a pure function over the source text, and
//!    [`guard_catches_each_mutation_it_was_written_for`] replays the mutations that the OLD guard
//!    survived against those same functions. A guard with no failing case is decoration.

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


fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    strip_comments(src)
}

const SDK: &str = "clients/libs/rust-sdk/src/transfer.rs";
const SENDER: &str = "clients/libs/rust/src/transfer_sender.rs";

// ---------------------------------------------------------------------------------------------
// Window machinery. Rule 4: a window is bounded by two REAL symbols, and a missing one is fatal.
// ---------------------------------------------------------------------------------------------

fn find_one(code: &str, needle: &str, why: &str) -> Result<usize, String> {
    code.find(needle)
        .ok_or_else(|| format!("`{needle}` is gone from the scanned source — {why}"))
}

/// The body of a named item, ending at the REAL symbol that follows it in the file.
///
/// `end` is not decoration: with `&code[at..]` (what this file used to do) every assertion below
/// could be satisfied by the text of a completely different function further down. If either anchor
/// disappears this returns `Err` — the guard fails LOUDLY and asks to be re-derived, rather than
/// quietly widening to cover the whole file.
fn item_body<'a>(code: &'a str, start: &str, end: &str) -> Result<&'a str, String> {
    let at = find_one(code, start, "this guard has lost its subject; re-derive it")?;
    let rest = &code[at..];
    let to = rest.find(end).ok_or_else(|| {
        format!(
            "the window for `{start}` no longer terminates: `{end}` (the item that follows it) is \
             gone. Refusing to scan to end of file — an unterminated window lets the WRONG \
             function's text satisfy every assertion below it, which is exactly how the spine-tip \
             refusal pin stayed green while the refusal was a `println!`."
        )
    })?;
    Ok(&rest[..to])
}

struct Block<'a> {
    /// Text between the braces.
    inner: &'a str,
    /// Index of the `{`.
    open: usize,
    /// Index of the matching `}`.
    close: usize,
}

/// The brace-matched `{ … }` block that opens at or after `from`, skipping string/char literals and
/// any residual trailing comments so that a `{sid}` inside a message can never be mistaken for code.
fn block_after(code: &str, from: usize) -> Option<Block<'_>> {
    let b = code.as_bytes();
    let mut i = from;
    let mut open: Option<usize> = None;
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'r' if b[i..].starts_with(b"r#\"") => {
                i += 3;
                while i < b.len() && !b[i..].starts_with(b"\"#") {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            // A char literal such as '{' or '}' — three bytes, never a block delimiter.
            b'\'' if b.get(i + 2) == Some(&b'\'') => {
                i += 3;
                continue;
            }
            b'{' => {
                if open.is_none() {
                    open = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let o = open?;
                    return Some(Block { inner: &code[o + 1..i], open: o, close: i });
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The head of the line `at` sits on, trimmed — used to prove a call is the CONDITION of an `if`
/// rather than a statement whose answer is thrown away.
fn line_head(code: &str, at: usize) -> &str {
    let start = code[..at].rfind('\n').map_or(0, |n| n + 1);
    code[start..at].trim_start()
}

// ---------------------------------------------------------------------------------------------
// The properties, as pure functions over source text (rule 5).
// ---------------------------------------------------------------------------------------------

/// THE PROOF EXISTS, and fails closed.
fn check_exit_material_proof(sdk: &str) -> Result<(), String> {
    let body = item_body(
        sdk,
        "pub(crate) async fn has_exit_material(",
        // Rule 4: a real terminator, not `at + 1_400` bytes. The old fixed byte count happened to
        // stop inside the function today and would silently start covering the NEXT function the
        // moment a comment line or an argument was added.
        "pub(crate) async fn spendable_payment_coins(",
    )?;
    if !body.contains("try_get_backup_txs(") {
        return Err(format!(
            "`has_exit_material` no longer uses `try_get_backup_txs`. `get_backup_txs` is a \
             `fetch_one`, so a MISSING row and a FAILED read are the same value there — and reading \
             a failed read as 'no material' retires a perfectly good coin from the wallet's \
             balance:\n\n{body}"
        ));
    }
    // [ONE COIN SHAPE] The short-circuit used to read `ParentShape::Unladdered`; that variant is
    // deleted with the lane, and the probe is now `parent_shape_opt(..).is_some()`. The PROPERTY is
    // unchanged and is what this pins: the three laddered shapes ARE exit material, and this
    // function must ask the one probe rather than re-deriving it.
    if !body.contains("parent_shape_opt(") {
        return Err(format!(
            "`has_exit_material` no longer short-circuits on the laddered shapes; a bundle IS exit \
             material and re-deriving that probe here would be a second definition to \
             drift:\n\n{body}"
        ));
    }
    Ok(())
}

/// One of the three routes that acts on `ParentShape::Unladdered` (or on its flat-sender equivalent)
/// by handing a coin to a PLAIN split. Each must PROVE the material, with the proof GATING the route
/// and standing BEFORE it.
struct Gate {
    /// The item this gate lives in.
    item: &'static str,
    /// The real symbol that terminates the item's window (rule 4).
    ends_at: &'static str,
    /// Required polarity of the condition: `"if !"` excludes on the false branch, `"if "` admits on
    /// the true branch. Pinning polarity is what makes an INVERTED gate a failure rather than a
    /// rename.
    polarity: &'static str,
    /// What the gate's own block must DO. A gate whose block does nothing is not a gate.
    block_must: &'static [&'static str],
    /// For a positive gate: the `else` branch is the exclusion, and it must say so.
    else_must: Option<&'static str>,
    /// The act the proof must precede (rule 2 — positions, not counts).
    route: &'static str,
    /// Why this site needs its own proof.
    why: &'static str,
}

const GATES: &[Gate] = &[
    Gate {
        item: "pub(crate) async fn spendable_payment_coins(",
        ends_at: "pub(crate) async fn parent_shape(",
        polarity: "if ",
        block_must: &["usable.push(c);"],
        else_must: Some("no_exit_material.push("),
        route: "Ok(SpendableCoins {",
        why: "the SHARED selection filter — the [B2] 'one coin set' both `quote_transfer` and \
              `transfer` plan over. A coin that survives this loop un-probed is a coin the planner \
              promises and the sender cannot spend",
    },
];

/// THE UN-LADDERED ROUTES. Every place that routes a coin to a PLAIN split proves the material
/// first — *proves* (the answer decides the branch) and *first* (the proof precedes the route).
///
/// This replaces the old `proofs >= 3` count. The count could not see the difference between a
/// blocking probe and `let _ = self.has_exit_material(&id).await?;`, which is precisely the mutation
/// that survived.
fn check_plain_split_gates(sdk: &str) -> Result<(), String> {
    // No call to the proof anywhere in this file may be a bare statement whose answer is discarded.
    // (The definition itself is `pub(crate) async fn has_exit_material`, not `self.`, so it is not
    // matched here.)
    for (at, _) in sdk.match_indices("self.has_exit_material(") {
        let head = line_head(sdk, at);
        if head != "if " && head != "if !" {
            return Err(format!(
                "a `self.has_exit_material(` call is not the condition of an `if` — the line reads \
                 `{head}…`. An answer that does not choose a branch is not a proof: this is exactly \
                 the mutation the old `proofs >= 3` count survived, three call sites of which two \
                 were non-blocking, while both self-serve candidate lists went on routing \
                 materialless slots to the plain split."
            ));
        }
    }

    for g in GATES {
        let body = item_body(sdk, g.item, g.ends_at)?;
        let gate = body
            .match_indices("self.has_exit_material(")
            .map(|(i, _)| i)
            .find(|&i| line_head(body, i) == g.polarity)
            .ok_or_else(|| {
                format!(
                    "`{}` has no `{}self.has_exit_material(…)` gate. {}.\n\nA call is only a gate if \
                     its answer chooses the branch, and the POLARITY is part of the property: with \
                     `if !` the losing branch is the exclusion, with `if ` the losing branch is the \
                     `else`. An inverted or non-blocking probe admits precisely the coins it was \
                     written to exclude.",
                    g.item, g.polarity, g.why
                )
            })?;
        let block = block_after(body, gate).ok_or_else(|| {
            format!("the `has_exit_material` gate in `{}` has no `{{ … }}` block", g.item)
        })?;
        for must in g.block_must {
            if !block.inner.contains(must) {
                return Err(format!(
                    "the `has_exit_material` gate in `{}` no longer contains `{must}`; its block \
                     reads:\n{}\n\nA gate whose losing branch does not DIVERT is not a gate — the \
                     coin walks on to the plain split anyway and the failure resurfaces at the far \
                     end of the payment as an unclassified breach.",
                    g.item, block.inner
                ));
            }
        }
        if let Some(exclusion) = g.else_must {
            let tail = body[block.close + 1..].trim_start();
            if !tail.starts_with("else") {
                return Err(format!(
                    "the positive `has_exit_material` gate in `{}` has no `else` branch. With this \
                     polarity the `else` IS the exclusion; without it the coin is neither admitted \
                     nor recorded, and `TransferQuote` under-reports balance with no field saying \
                     why.",
                    g.item
                ));
            }
            let else_block = block_after(body, block.close + 1)
                .ok_or_else(|| format!("the `else` of the gate in `{}` has no block", g.item))?;
            if !else_block.inner.contains(exclusion) {
                return Err(format!(
                    "the `else` of the `has_exit_material` gate in `{}` no longer records the coin \
                     with `{exclusion}`; it reads:\n{}\n\nSilently withholding balance with no \
                     field saying why is the shape that made this a chaos-oracle breach rather than \
                     a message.",
                    g.item, else_block.inner
                ));
            }
        }
        // Rule 2: ORDERING is a comparison of POSITIONS.
        let route = find_one(
            body,
            g.route,
            &format!("`{}` no longer takes the plain-split route — re-derive this guard", g.item),
        )?;
        if gate >= route {
            return Err(format!(
                "in `{}` the `has_exit_material` proof (byte {gate}) runs at or AFTER `{}` (byte \
                 {route}). {}.\n\nThe test is called 'proves its material FIRST': a proof that runs \
                 after the route it is meant to gate has already let the materialless coin through.",
                g.item, g.route, g.why
            ));
        }
    }
    Ok(())
}

/// **THE REFUSAL MUST BE CALLER-INDEPENDENT.** This is the half of #145 that was NOT a selection
/// bug, and it is the more dangerous half.
///
/// The spine-tip refusal lived in exactly one caller — `UtexoWallet::transfer`'s handover loop.
/// `transfer_sender::execute` is public, `chaos22`'s `respend` calls it directly, and a direct
/// caller has no dispatch: the tip walked into the flat lane, whose classifier LICENSES it
/// (`FundingNotOnChain` — a tip's funding output is un-broadcast, exactly like a `ctesr-` child's).
/// What stopped the conveyance was an ABSENCE, the missing backup rows.
///
/// Refusal-by-absence is one guard away from a money loss: a flat conveyance of a tip hands the
/// recipient a backup chain over an outpoint that does not exist on chain and never will — a coin
/// with no exit, with no error on either side.
///
/// So this pins the REFUSAL (a `return Err` inside the probe's own block), not the message — the old
/// version pinned only the position of the `load_spine_tip(` call, and a `println!` carrying the
/// identical text satisfied it.
fn check_tip_refusal(sender: &str) -> Result<(), String> {
    let body = item_body(
        sender,
        "async fn execute_ex(",
        // Rule 4. The old window was `&code[at..]` — to end of file.
        "async fn create_backup_tx_to_receiver(",
    )?;
    let probe = find_one(
        body,
        "load_spine_tip(",
        "`execute_ex` no longer refuses a spine tip ITSELF. The refusal must not live only in \
         `UtexoWallet::transfer`'s dispatch: `execute` is public and a direct caller reaches the \
         flat lane, which LICENSES an un-broadcast funding output and would convey the tip on a \
         backup chain the recipient can never exit through",
    )?;
    let block = block_after(body, probe)
        .ok_or_else(|| "the spine-tip probe in `execute_ex` has no `{ … }` block".to_string())?;

    // The probe must FAIL CLOSED. A tip read that is swallowed reads as "not a tip" and routes the
    // tip flat — the same silent-degradation shape, arriving through the read instead of the branch.
    let condition = &body[probe..block.open];
    if !condition.contains(".await?") || condition.contains("unwrap_or") || condition.contains(".ok()")
    {
        return Err(format!(
            "the spine-tip probe in `execute_ex` no longer propagates a failed read:\n{condition}\n\
             \nA swallowed read answers 'not a tip', and a tip conveyed flat is a backup chain over \
             an outpoint that will never exist on chain."
        ));
    }

    // THE REFUSAL ITSELF — inside the probe's own block, and expressed as a refusal.
    let refuses = block.inner.contains("return Err(")
        || block.inner.contains("bail!(")
        || block.inner.contains("Err(anyhow!(");
    if !refuses {
        return Err(format!(
            "the spine-tip branch in `execute_ex` does not REFUSE. Its block reads:\n{}\n\nThis is \
             the pin the old guard did not have: it asserted only that `load_spine_tip(` appeared \
             before `create_backup_transactions(`, so replacing the `return Err(anyhow!(\"… is a \
             SPINE TIP …\"))` with a `println!` carrying the identical text left the guard green — \
             and the tip then walked the flat lane with no error on either side. The message text is \
             not the property; the `return Err` is.",
            block.inner
        ));
    }

    // ...and it must refuse BEFORE the SE co-signs anything (rule 2 — positions).
    let rows = find_one(
        body,
        "create_backup_transactions(",
        "`execute_ex` no longer builds the flat backup chain — re-derive this guard",
    )?;
    if block.close >= rows {
        return Err(format!(
            "the spine-tip refusal (block ends at byte {}) runs at or AFTER \
             `create_backup_transactions` (byte {rows}). Then the tip is stopped by the ABSENCE of \
             backup rows rather than by a rule — refusal by accident, which reads to `chaos22`'s \
             oracle as an unclassified breach, and it leaves the coin with an inflated `num_sigs` \
             from a co-sign it should never have reached.",
            block.close
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The invariants, checked against the real tree.
// ---------------------------------------------------------------------------------------------

/// THE FUNNEL. `payment_coins` is the synchronous half of the [B2] "one coin set" property; the
/// exit-material filter is the half that needs the database. If a call site takes the synchronous
/// half alone, it has the un-filtered set — and [B2]'s whole point is that `quote_transfer` and
/// `transfer` cannot look at different wallets.
#[test]
fn payment_coins_has_exactly_one_caller() {
    let code = code_only(&read(SDK));
    // Word-boundary matching: `spendable_payment_coins(` CONTAINS `payment_coins(`, and counting the
    // wrapper's own definition and calls as violations would make this guard unsatisfiable.
    let calls: Vec<usize> = code
        .match_indices("payment_coins(")
        .map(|(i, _)| i)
        .filter(|i| {
            let before = &code[i.saturating_sub(1)..*i];
            before != "_" // not the tail of `spendable_payment_coins(`
        })
        .filter(|i| !code[i.saturating_sub(3)..*i].ends_with("fn ")) // not the definition
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "`payment_coins` has {} call sites; exactly one is allowed (inside \
         `spendable_payment_coins`). A caller taking the synchronous half alone gets coins with no \
         exit material — slots this wallet can neither exit nor convey — and the failure surfaces at \
         the far end of the payment as an unclassified breach rather than as a refusal to plan.",
        calls.len()
    );
}

/// THE PROOF EXISTS, and fails closed. `try_get_backup_txs` is the absence-vs-failure split: reading
/// a FAILED read as "no material" would silently retire a good coin from the spendable balance —
/// the same silent-degradation shape pointed the other way.
#[test]
fn the_exit_material_proof_reads_absence_not_failure() {
    check_exit_material_proof(&code_only(&read(SDK))).unwrap_or_else(|e| panic!("{e}"));
}

/// THE SHARED SELECTION FILTER, gated and ordered — see [`check_plain_split_gates`].
///
/// [ONE COIN SHAPE] This used to cover THREE routes. Two of them — `transfer_many`'s plain-split
/// candidate arm and `ensure_exact_coin`'s minting fallback — are DELETED with the un-laddered
/// shape, along with `split_coin` itself, so their gates have no subject left to police. What
/// survives is the one that was never about plain splits: `spendable_payment_coins`, the [B2] shared
/// coin set that `quote_transfer` and `transfer` both plan over. A coin surviving that loop un-probed
/// is still a coin the planner promises and the sender cannot spend, so the gate still earns its
/// keep — and #145 is still the defect it exists to prevent.
#[test]
fn the_shared_selection_filter_proves_its_material_first() {
    check_plain_split_gates(&code_only(&read(SDK))).unwrap_or_else(|e| panic!("{e}"));
}

/// THE FLAT SENDER REFUSES A TIP ITSELF — see [`check_tip_refusal`].
#[test]
fn the_flat_sender_refuses_a_spine_tip_itself() {
    check_tip_refusal(&code_only(&read(SENDER))).unwrap_or_else(|e| panic!("{e}"));
}

/// THE REMEDY IS NAMED, and named CORRECTLY. A stuck coin has a fee problem that combining rescues;
/// a materialless coin is missing the material itself and combining does nothing. Reporting them as
/// one count tells a user to run an operation that cannot work.
#[test]
fn the_two_exclusions_keep_two_remedies() {
    let sdk = read(SDK);
    assert!(
        sdk.contains("not rescuable by combining"),
        "the quote no longer distinguishes a materialless coin from a stuck one. Combining rescues \
         a coin whose value is below its renewal fee; it cannot conjure a backup chain."
    );
    let types = read("clients/libs/rust-sdk/src/types.rs");
    assert!(
        types.contains("pub no_exit_material_coins: Vec<String>"),
        "`TransferQuote` no longer reports the excluded coins. Silently withholding balance from \
         `fundable` with no field saying why is the shape that made this a chaos-oracle breach \
         rather than a message."
    );
}

/// The scan windows really are bounded (rule 4). If `execute_ex`'s window leaked into the function
/// after it, every assertion in [`check_tip_refusal`] could be satisfied by the wrong function's
/// text — which is what "to end of file" meant in the old version.
#[test]
fn every_scan_window_stops_at_a_real_symbol() {
    let sender = code_only(&read(SENDER));
    let body = item_body(&sender, "async fn execute_ex(", "async fn create_backup_tx_to_receiver(")
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        body.contains("create_backup_transactions("),
        "`execute_ex`'s window no longer contains the flat backup build — re-derive this guard"
    );
    assert!(
        !body.contains("async fn create_backup_tx_to_receiver("),
        "`execute_ex`'s window overshot into the next item"
    );

    let sdk = code_only(&read(SDK));
    for g in GATES {
        let body = item_body(&sdk, g.item, g.ends_at).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !body[g.item.len()..].contains(g.ends_at),
            "the window for `{}` overshot past `{}`",
            g.item,
            g.ends_at
        );
    }
}

// ---------------------------------------------------------------------------------------------
// NON-VACUITY. Rule 5: replay, against the real sources, the mutations the OLD guard survived.
// ---------------------------------------------------------------------------------------------

/// Replace the first occurrence of `from` that appears AFTER `anchor`, so a mutation lands in the
/// function it was written for rather than on an identical line elsewhere in the file.
fn mutate_after(code: &str, anchor: &str, from: &str, to: &str) -> String {
    let at = code.find(anchor).unwrap_or_else(|| panic!("mutation anchor `{anchor}` not found"));
    let tail = code[at..].replacen(from, to, 1);
    assert!(
        tail != code[at..],
        "mutation `{from}` -> `{to}` did not apply after `{anchor}`; the source moved and this \
         non-vacuity case is now vacuous"
    );
    format!("{}{tail}", &code[..at])
}

/// **A guard that cannot fail is decoration.** Each case below is a mutation an adversarial review
/// applied (or could apply) to the real tree; the first two are the ones the PREVIOUS version of
/// this file was proven to survive.
#[test]
fn guard_catches_each_mutation_it_was_written_for() {
    let sender = code_only(&read(SENDER));
    let sdk = code_only(&read(SDK));

    // A3, THE PROVEN MUTATION. Same message text, no refusal. The old guard: GREEN (5 passed).
    let m = mutate_after(&sender, "load_spine_tip(", "return Err(anyhow!(", "println!(");
    assert!(
        check_tip_refusal(&m).is_err(),
        "A3: turning the spine-tip `return Err` into a `println!` with identical text was NOT \
         caught — the guard is back to pinning the message instead of the refusal"
    );

    // A3 again, with the refusal text moved into a LATER function. This is what an unterminated
    // window (`&code[at..]`) would have accepted: the right words, in the wrong function.
    let decoy = format!(
        "{m}\nasync fn decoy_refuser() -> Result<()> {{\n    if crate::tesr::load_spine_tip(c, w, \
         &sid).await?.is_some() {{\n        return Err(anyhow!(\"statechain id is a SPINE TIP\"));\n\
         \n    }}\n    Ok(())\n}}\n"
    );
    assert!(
        check_tip_refusal(&decoy).is_err(),
        "A3: a refusal planted in a LATER function satisfied the guard — the scan window is \
         overshooting again"
    );

    // A3 ordering: hoist the SE co-sign ABOVE the refusal. The tip is then stopped (if at all) by
    // the absence of backup rows, after its `num_sigs` has already been raised.
    let hoisted = mutate_after(
        &sender,
        "async fn execute_ex(",
        "let coin = coin.unwrap().clone();",
        "let _bkps = create_backup_transactions(client_config, recipient_address, &mut wallet, \
         &statechain_id, duplicated_indexes).await?;\n    let coin = coin.unwrap().clone();",
    );
    assert!(
        check_tip_refusal(&hoisted).is_err(),
        "A3: co-signing the backup chain BEFORE the spine-tip refusal was not caught"
    );

    // A3: a swallowed tip read answers "not a tip" and routes the tip flat.
    let swallowed =
        mutate_after(&sender, "load_spine_tip(", ".await?", ".await.unwrap_or(None)");
    assert!(
        check_tip_refusal(&swallowed).is_err(),
        "A3: a swallowed `load_spine_tip` read was not caught"
    );




    // A4: the selection filter stops recording what it excluded.
    let no_record = mutate_after(
        &sdk,
        "pub(crate) async fn spendable_payment_coins(",
        "} else {",
        "}\n            if false {",
    );
    assert!(
        check_plain_split_gates(&no_record).is_err(),
        "A4: dropping the `else` that records the excluded coins was not caught"
    );

    // [ONE COIN SHAPE] The ORDERING case is retired with its subject. It ran the plain split before
    // the proof meant to gate it, inside `ensure_exact_coin` — and both the plain split and that
    // function's candidate loop are deleted. The ordering RULE still holds for the surviving gate and
    // is exercised by the cases above; what is gone is a mutation with nothing left to mutate. A
    // vacuous non-vacuity case is worse than none, which is exactly what `mutate_after` asserts.

    // The fail-closed read in `has_exit_material` itself.
    let fetch_one = mutate_after(
        &sdk,
        "pub(crate) async fn has_exit_material(",
        "try_get_backup_txs(",
        "get_backup_txs(",
    );
    assert!(
        check_exit_material_proof(&fetch_one).is_err(),
        "reverting the proof to the ambiguous `fetch_one` read was not caught"
    );

    // ...and the unmutated tree must still pass, or the cases above prove nothing.
    check_tip_refusal(&sender).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
    check_plain_split_gates(&sdk).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
    check_exit_material_proof(&sdk).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
}
