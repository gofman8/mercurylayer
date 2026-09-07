//! **[#145] A coin may not be offered to a payment unless this wallet holds material to EXIT it.**
//!
//! Every filter in `payment_coins` asks whether a coin is ELIGIBLE — confirmed, not a duplicate, not
//! an RGB carrier, worth more than its own renewal fee. None of them asks the separate question of
//! whether the wallet can actually *exit or convey* it:
//!
//! * a laddered coin's exit material is its bundle (`tesr-` root, `ctesr-` child, `spinetip-` tip),
//!   and under ONE COIN SHAPE that is the ONLY exit material there is: the ladder is established at
//!   the first mempool sighting of the deposit, and no flat absolute-locktime backup is ever
//!   co-signed — not at deposit, not at any hop;
//! * a coin with **no ladder row** is a slot the SE knows about, that this wallet holds a key for,
//!   and that it can neither exit unilaterally nor hand on — most often a deposit whose establish
//!   pass has not yet succeeded (it stays INITIALISED and is retried), or a derived child slot whose
//!   split failed after the slot was minted.
//!
//! That second shape was offered to selection, and the failure surfaced at the FAR END of the
//! payment. `chaos22`'s oracle could only class it as an unclassified breach — the wallet reported
//! balance, the planner promised it, and the send died. The remedy is upstream: the coin is never
//! offered, so a different coin funds the payment and nobody meets the refusal.
//!
//! # Why the shape of the check matters more than the check
//!
//! `parent_shape` reaches "no shape" through THREE CONSECUTIVE ABSENCES — no tip row, no child row,
//! no root row. This repo has already been bitten once by treating that as a positive answer (the
//! spine tip fell through all three and was routed as un-laddered, at the wrong floor, to the
//! [B1]-unsafe plain split). There is no un-laddered lane any more, so an absence is not a route:
//! `transfer_sender::execute_ex` refuses a coin with no ladder row BY NAME ("has no exit ladder and
//! cannot be conveyed"), before any SE co-sign, and the selection filter must never offer one.
//!
//! # What the sender must not do on the way to refusing
//!
//! The only co-sign `execute_ex` performs is the receiver-paying state `S'`
//! (`presign_receiver_state` / `cosign_colored_receiver_state`). It permanently raises the coin's
//! enclave `num_sigs`, so every refusal in the lane — the spine-tip refusal, the no-ladder refusal —
//! must sit ABOVE it: a refusal after the co-sign leaves the coin with a count the receiver's census
//! `se_num_sigs == tiers + superseded` can never balance. The lane co-signs NO flat backup: the old
//! per-hop `create_backup_transactions` / `create_backup_tx_to_receiver` are deleted, and the
//! conveyed `backup_transactions` vector is empty by construction.
//!
//! # Why this guard was rewritten (the two pins that a mutation survived)
//!
//! An adversarial review applied the following two mutations to the real tree and watched the
//! previous version of this file stay GREEN. Both failures are of the same species as the bug the
//! guard exists to prevent: a *description* was pinned where a *property* was meant.
//!
//! * **A3 — a refusal pinned as a POSITION, not as a refusal.** The old
//!   `the_flat_sender_refuses_a_spine_tip_itself` found `load_spine_tip(` and the co-sign and
//!   asserted `refusal < cosign`. It never asserted that anything was *refused*. Replacing
//!   `return Err(anyhow!("statechain id {statechain_id} is a SPINE TIP …"))` with a `println!`
//!   carrying the identical text left the guard green (5 passed) — and a spine tip then walked on
//!   with no error on either side. Worse, the scan window was `&code[at..]` to END OF FILE, so both
//!   anchors could have been satisfied by any LATER function's text. A guard whose window overshoots
//!   is pinning the file, not the function.
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

/// The item that follows `execute_ex` in `transfer_sender.rs` — the real symbol that terminates its
/// window (rule 4). Column-anchored, because `get_new_x1(` is also CALLED inside `execute_ex` and a
/// bare `get_new_x1(` would end the window at the call.
const SENDER_WINDOW_END: &str = "\npub async fn get_new_x1(";

/// The ONLY co-sign `execute_ex` performs: the receiver-paying state `S'`, on the plain lane and on
/// the coloured lane. Each raises the coin's enclave `num_sigs`, so every refusal in the lane must
/// precede BOTH — the guard orders against the earlier of the two.
const S_PRIME_COSIGNS: [&str; 2] = ["cosign_colored_receiver_state(", "presign_receiver_state("];

/// The deleted flat co-signs. A laddered coin conveys `backup_transactions: []`; a per-hop backup
/// would be a co-sign the receiver's census `tiers + superseded` cannot account for, and a matured
/// spend of `F` left in this wallet's hands after the coin is gone.
const FLAT_COSIGNS: [&str; 2] = ["create_backup_transactions(", "create_backup_tx_to_receiver("];

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
            "the window for `{start}` no longer terminates: `{}` (the item that follows it) is \
             gone. Refusing to scan to end of file — an unterminated window lets the WRONG \
             function's text satisfy every assertion below it, which is exactly how the spine-tip \
             refusal pin stayed green while the refusal was a `println!`.",
            end.trim()
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

/// A block REFUSES: `return Err` / `bail!` / `Err(anyhow!(` inside it. The message text is not the
/// property; a `println!` carrying the identical words satisfies a message pin and refuses nothing.
fn refuses(block: &str) -> bool {
    block.contains("return Err(") || block.contains("bail!(") || block.contains("Err(anyhow!(")
}

/// `execute_ex`'s window, bounded by the item that follows it (rule 4).
fn execute_ex_body(sender: &str) -> Result<&str, String> {
    item_body(sender, "async fn execute_ex(", SENDER_WINDOW_END)
}

/// The position of the FIRST `S'` co-sign in `execute_ex`. BOTH lanes' co-signs must be present —
/// a rename would otherwise make every ordering below vacuously true (rule 2 orders against a real
/// symbol, never against an absence).
fn first_cosign(body: &str) -> Result<usize, String> {
    let mut first = usize::MAX;
    for needle in S_PRIME_COSIGNS {
        let at = find_one(
            body,
            needle,
            "`execute_ex` no longer co-signs the receiver-paying state S' through it — this guard \
             orders every refusal in the lane against that co-sign, so its disappearance makes the \
             ordering vacuous. Re-derive the lane's first material-producing step; do not drop \
             the marker",
        )?;
        first = first.min(at);
    }
    Ok(first)
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
    // [ONE COIN SHAPE] The short-circuit used to read `ParentShape::Unladdered`; that variant is
    // deleted with the lane, and the probe is now `parent_shape_opt(..).is_some()`. The PROPERTY is
    // unchanged and is what this pins: the three laddered shapes ARE exit material — the ONLY exit
    // material — and this function must ask the one probe rather than re-deriving it.
    if !body.contains("parent_shape_opt(") {
        return Err(format!(
            "`has_exit_material` no longer short-circuits on the laddered shapes; a bundle IS exit \
             material and re-deriving that probe here would be a second definition to \
             drift:\n\n{body}"
        ));
    }
    // IF the function still reads the legacy flat-backup rows as a fallback, it must do so through
    // `try_get_backup_txs`: `get_backup_txs` is a `fetch_one`, so a MISSING row and a FAILED read are
    // the same value there — and reading a failed read as 'no material' retires a perfectly good
    // coin from the wallet's balance. Conditional, because under ONE COIN SHAPE the fallback is a
    // legacy residue (a coin with rows and no ladder is refused by `execute_ex` anyway) and removing
    // it outright must not trip this guard — a guard that fires on right code gets weakened, every
    // time, until it means nothing.
    let bare_reads = body
        .match_indices("get_backup_txs(")
        .filter(|(i, _)| !body[..*i].ends_with("try_"))
        .count();
    if bare_reads > 0 {
        return Err(format!(
            "`has_exit_material` reads backup rows through `get_backup_txs` ({bare_reads} call(s)). \
             That is a `fetch_one`, so a MISSING row and a FAILED read are the same value there — \
             and reading a failed read as 'no material' retires a perfectly good coin from the \
             wallet's balance. Use `try_get_backup_txs`, or drop the flat-row fallback:\n\n{body}"
        ));
    }
    Ok(())
}

/// The shared selection filter — the one route that decides which coins a payment may take. The
/// proof must GATE the route and stand BEFORE it.
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

/// THE SELECTION GATE. The one place that decides the spendable set proves the material first —
/// *proves* (the answer decides the branch) and *first* (the proof precedes the route).
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
                     coin walks on to the sender anyway and the failure resurfaces at the far end \
                     of the payment as an unclassified breach.",
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
            &format!("`{}` no longer builds the spendable set here — re-derive this guard", g.item),
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
/// caller has no dispatch. Handing a tip over whole is a `spinetip-` conveyance whose builder is
/// not landed; what used to stop it was an ABSENCE (the tip had no flat backup rows), and
/// refusal-by-absence is one guard away from a money loss.
///
/// So this pins the REFUSAL (a `return Err` inside the probe's own block), not the message — the old
/// version pinned only the position of the `load_spine_tip(` call, and a `println!` carrying the
/// identical text satisfied it. And it pins that the refusal precedes the `S'` co-sign, the only
/// step in the lane that raises the coin's `num_sigs`.
fn check_tip_refusal(sender: &str) -> Result<(), String> {
    let body = execute_ex_body(sender)?;
    let probe = find_one(
        body,
        "load_spine_tip(",
        "`execute_ex` no longer refuses a spine tip ITSELF. The refusal must not live only in \
         `UtexoWallet::transfer`'s dispatch: `execute` is public and a direct caller would convey \
         the tip whole, which is a spine-tip conveyance whose builder is not landed",
    )?;
    let block = block_after(body, probe)
        .ok_or_else(|| "the spine-tip probe in `execute_ex` has no `{ … }` block".to_string())?;

    // The probe must FAIL CLOSED. A tip read that is swallowed reads as "not a tip" and conveys the
    // tip — the same silent-degradation shape, arriving through the read instead of the branch.
    let condition = &body[probe..block.open];
    if !condition.contains(".await?") || condition.contains("unwrap_or") || condition.contains(".ok()")
    {
        return Err(format!(
            "the spine-tip probe in `execute_ex` no longer propagates a failed read:\n{condition}\n\
             \nA swallowed read answers 'not a tip', and a tip conveyed whole hands the recipient \
             a ladder over an un-broadcast funding output."
        ));
    }

    // THE REFUSAL ITSELF — inside the probe's own block, and expressed as a refusal.
    if !refuses(block.inner) {
        return Err(format!(
            "the spine-tip branch in `execute_ex` does not REFUSE. Its block reads:\n{}\n\nThis is \
             the pin the old guard did not have: it asserted only that `load_spine_tip(` appeared \
             before the co-sign, so replacing the `return Err(anyhow!(\"… is a SPINE TIP …\"))` \
             with a `println!` carrying the identical text left the guard green — and the tip then \
             walked on with no error on either side. The message text is not the property; the \
             `return Err` is.",
            block.inner
        ));
    }

    // ...and it must refuse BEFORE the SE co-signs anything (rule 2 — positions).
    let cosign = first_cosign(body)?;
    if block.close >= cosign {
        return Err(format!(
            "the spine-tip refusal (block ends at byte {}) runs at or AFTER the S' co-sign (byte \
             {cosign}). Then the tip is refused only after its `num_sigs` has already been raised \
             by a co-sign it should never have reached — the receiver's census \
             `se_num_sigs == tiers + superseded` can never balance for that coin again.",
            block.close
        ));
    }
    Ok(())
}

/// **A COIN WITH NO LADDER ROW IS REFUSED BY NAME, BEFORE THE CO-SIGN.** Under ONE COIN SHAPE there
/// is no un-laddered lane: a coin whose `tesr::load` answers `None` has NO exit material and no
/// census a receiver could balance. `execute_ex` must refuse it (a `return Err` inside the
/// `is_none()` block) and must do so ABOVE the `S'` co-sign — refusing after the co-sign would raise
/// `num_sigs` on a coin that has no ladder to account for it, bricking it for good.
fn check_no_ladder_refusal(sender: &str) -> Result<(), String> {
    let body = execute_ex_body(sender)?;
    let probe = find_one(
        body,
        "if tesr_bundle.is_none()",
        "`execute_ex` no longer refuses a coin with no ladder row by name. There is no un-laddered \
         lane: such a coin has no exit material, and conveying it would hand the recipient a coin \
         with no exit and no census",
    )?;
    let block = block_after(body, probe)
        .ok_or_else(|| "the no-ladder probe in `execute_ex` has no `{ … }` block".to_string())?;
    if !refuses(block.inner) {
        return Err(format!(
            "the no-ladder branch in `execute_ex` does not REFUSE. Its block reads:\n{}\n\nA coin \
             with no ladder row has no exit material; anything but a `return Err` here conveys it.",
            block.inner
        ));
    }
    let cosign = first_cosign(body)?;
    if block.close >= cosign {
        return Err(format!(
            "the no-ladder refusal (block ends at byte {}) runs at or AFTER the S' co-sign (byte \
             {cosign}). A co-sign on a coin with no ladder raises its `num_sigs` with no tier to \
             account for it — the coin is bricked in the act of being refused.",
            block.close
        ));
    }
    Ok(())
}

/// **THE LANE CO-SIGNS NO FLAT BACKUP.** A laddered coin conveys `backup_transactions: []`: the
/// deposit-time `tx1` and the per-hop `create_backup_tx_to_receiver` are deleted, and every receiver
/// refuses a non-empty vector by name. A flat co-sign re-appearing here would be a co-sign the
/// receiver's census cannot account for AND a matured spend of `F` retained by a former owner.
fn check_no_flat_cosign(sender: &str) -> Result<(), String> {
    for needle in FLAT_COSIGNS {
        if let Some(at) = sender.find(needle) {
            return Err(format!(
                "`{needle}` is back in transfer_sender.rs (byte {at}). A laddered coin conveys NO \
                 flat backup — none at deposit, none at any hop — so this co-sign is one the \
                 receiver's census `tiers + superseded` cannot account for, and the transaction it \
                 signs is a spend of `F` this wallet keeps after the coin is gone."
            ));
        }
    }
    let body = execute_ex_body(sender)?;
    let bind = find_one(
        body,
        "let backup_transactions",
        "`execute_ex` no longer binds the conveyed `backup_transactions` — re-derive this guard \
         against whatever now fills that field of the transfer message",
    )?;
    let stmt_end = body[bind..]
        .find(';')
        .map(|d| bind + d)
        .ok_or_else(|| "the `backup_transactions` binding is not terminated".to_string())?;
    let stmt = &body[bind..stmt_end];
    if !(stmt.contains("Vec::new()") || stmt.contains("vec![]")) {
        return Err(format!(
            "`execute_ex` conveys a `backup_transactions` vector that is not empty by \
             construction:\n{stmt}\n\nEvery receiver refuses a non-empty vector \
             (`verify_flat_backup_lane`), so anything else here is a transfer that cannot be \
             claimed."
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

/// THE PROOF EXISTS, and fails closed. The laddered shapes are the exit material, asked through the
/// one probe; any residual row read must keep the absence-vs-failure split.
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

/// THE SENDER REFUSES A TIP ITSELF — see [`check_tip_refusal`].
#[test]
fn the_flat_sender_refuses_a_spine_tip_itself() {
    check_tip_refusal(&code_only(&read(SENDER))).unwrap_or_else(|e| panic!("{e}"));
}

/// THE SENDER REFUSES A COIN WITH NO LADDER, BY NAME, BEFORE THE CO-SIGN — see
/// [`check_no_ladder_refusal`].
#[test]
fn the_sender_refuses_a_coin_with_no_ladder_before_the_cosign() {
    check_no_ladder_refusal(&code_only(&read(SENDER))).unwrap_or_else(|e| panic!("{e}"));
}

/// THE SENDER CO-SIGNS NO FLAT BACKUP AND CONVEYS AN EMPTY VECTOR — see [`check_no_flat_cosign`].
#[test]
fn the_sender_conveys_no_flat_backup() {
    check_no_flat_cosign(&code_only(&read(SENDER))).unwrap_or_else(|e| panic!("{e}"));
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
         a coin whose value is below its renewal fee; it cannot conjure a ladder."
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
    let body = execute_ex_body(&sender).unwrap_or_else(|e| panic!("{e}"));
    for needle in S_PRIME_COSIGNS {
        assert!(
            body.contains(needle),
            "`execute_ex`'s window no longer contains the S' co-sign `{needle}` — re-derive this \
             guard"
        );
    }
    assert!(
        !body.contains(SENDER_WINDOW_END),
        "`execute_ex`'s window overshot into the next item"
    );
    // The window ends on the item that follows `execute_ex` in the file — which is a real symbol
    // only if it is still there.
    assert!(
        sender.contains(SENDER_WINDOW_END),
        "`{}` is gone from transfer_sender.rs; the window terminator must be re-pointed",
        SENDER_WINDOW_END.trim()
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

/// A hoisted `S'` co-sign, in the shape the lane ships — planted ABOVE a refusal to prove the
/// ordering pins compare positions.
const HOISTED_COSIGN: &str = "let _early = crate::tesr::presign_receiver_state(client_config, \
                              &coin, &bundle, recipient_address).await?;\n    ";

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

    // A3 ordering: hoist the S' co-sign ABOVE the spine-tip refusal. The tip is then refused (if at
    // all) after its `num_sigs` has already been raised.
    let hoisted = mutate_after(
        &sender,
        "async fn execute_ex(",
        "let coin = coin.unwrap().clone();",
        &format!("{HOISTED_COSIGN}let coin = coin.unwrap().clone();"),
    );
    assert!(
        check_tip_refusal(&hoisted).is_err(),
        "A3: co-signing S' BEFORE the spine-tip refusal was not caught"
    );

    // A3: a swallowed tip read answers "not a tip" and conveys the tip.
    let swallowed =
        mutate_after(&sender, "load_spine_tip(", ".await?", ".await.unwrap_or(None)");
    assert!(
        check_tip_refusal(&swallowed).is_err(),
        "A3: a swallowed `load_spine_tip` read was not caught"
    );

    // [ONE COIN SHAPE] The no-ladder refusal demoted to a message. The coin then reaches the S'
    // co-sign with no ladder to account for it.
    let no_ladder_printed =
        mutate_after(&sender, "if tesr_bundle.is_none()", "return Err(anyhow!(", "println!(");
    assert!(
        check_no_ladder_refusal(&no_ladder_printed).is_err(),
        "turning the no-ladder `return Err` into a `println!` with identical text was NOT caught"
    );

    // [ONE COIN SHAPE] The S' co-sign hoisted ABOVE the no-ladder refusal — but still below the
    // spine-tip refusal, so ONLY the no-ladder ordering may fire. That is the point: the two pins
    // are independent positions, not one shared anchor.
    let cosign_above_no_ladder = mutate_after(
        &sender,
        "async fn execute_ex(",
        "if tesr_bundle.is_none()",
        &format!("{HOISTED_COSIGN}if tesr_bundle.is_none()"),
    );
    assert!(
        check_no_ladder_refusal(&cosign_above_no_ladder).is_err(),
        "co-signing S' BEFORE the no-ladder refusal was not caught"
    );
    check_tip_refusal(&cosign_above_no_ladder).unwrap_or_else(|e| {
        panic!("the spine-tip pin fired on a mutation that left its own ordering intact: {e}")
    });

    // [ONE COIN SHAPE] A flat backup co-sign re-planted in the lane, and a conveyed vector that is
    // no longer empty by construction. Both are the retired shape coming back.
    let flat_replanted = mutate_after(
        &sender,
        "async fn execute_ex(",
        "let backup_transactions: Vec<BackupTx> = Vec::new();",
        "let backup_transactions: Vec<BackupTx> = create_backup_transactions(client_config, \
         recipient_address, &mut wallet, &statechain_id).await?;",
    );
    assert!(
        check_no_flat_cosign(&flat_replanted).is_err(),
        "a re-planted `create_backup_transactions(` was not caught"
    );
    let flat_filled = mutate_after(
        &sender,
        "async fn execute_ex(",
        "let backup_transactions: Vec<BackupTx> = Vec::new();",
        "let backup_transactions: Vec<BackupTx> = get_backup_txs(&client_config.pool, \
         &wallet.name, &statechain_id).await?;",
    );
    assert!(
        check_no_flat_cosign(&flat_filled).is_err(),
        "a conveyed `backup_transactions` read from the legacy rows was not caught"
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

    // The fail-closed read in `has_exit_material` itself: the ambiguous `fetch_one` read in place of
    // the absence-vs-failure split. (Conditional on the flat-row fallback still being present — if
    // it has been removed, there is nothing to mutate and this case is retired with it.)
    if sdk.contains("try_get_backup_txs(") {
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
    }
    // ...and the one probe removed outright.
    let no_probe = mutate_after(
        &sdk,
        "pub(crate) async fn has_exit_material(",
        "parent_shape_opt(",
        "parent_shape_gone(",
    );
    assert!(
        check_exit_material_proof(&no_probe).is_err(),
        "dropping the laddered-shape probe from `has_exit_material` was not caught"
    );

    // ...and the unmutated tree must still pass, or the cases above prove nothing.
    check_tip_refusal(&sender).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
    check_no_ladder_refusal(&sender).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
    check_no_flat_cosign(&sender).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
    check_plain_split_gates(&sdk).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
    check_exit_material_proof(&sdk).unwrap_or_else(|e| panic!("clean tree rejected: {e}"));
}
