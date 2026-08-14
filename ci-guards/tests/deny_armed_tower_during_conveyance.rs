//! **[A2 / sdk80] Every lane that hands value away must disarm this wallet's own watchtower FIRST.**
//!
//! `defend_ladders`' child loop keys L1 on an **allowlist**: it broadcasts a retained state only for
//! a coin whose status reads `CONFIRMED`, which is this wallet's own record of *"mine, unspent, no
//! counterparty holds anything over it"*. The allowlist form is deliberate — a lane added tomorrow
//! parks its coin in SOME non-CONFIRMED status and is refused by default, so nobody can re-arm the
//! tower against their own recipient by forgetting to extend a denylist.
//!
//! **But the allowlist is only as good as when the field is written.** The loop re-reads that status
//! from the wallet DB on every pass. A lane that writes it LAST — after the SE has co-signed the
//! superseding split state, after the bundle has reached the recipient's mailbox — leaves a window
//! in which a pass reads a stale `CONFIRMED`, is admitted, and broadcasts the retained state over
//! the very outpoint the recipients' new state depends on.
//!
//! That is the sender's own tower destroying the payment the sender just made. On a coloured coin it
//! destroys the allocation.
//!
//! A2 closed this for `execute_ex`, `child_retransfer` and `cosign_colored_child_retransfer` by
//! hoisting a DURABLE status write ahead of the first step that can produce material for anybody
//! else. **The four in-ladder lanes were not covered**, and `sdk80` measured the window open in 1 of
//! 1 samples on `child_in_ladder_pay_many`.
//!
//! So this is a CENSUS rather than four assertions: the next lane to be added is the one that
//! matters, and the failure mode is silence.
//!
//! ---
//!
//! **WHY THIS FILE WAS REWRITTEN: the guard used to pin a DESCRIPTION, not the property it names.**
//!
//! `the_whole_coin_lanes_keep_their_arm_down` asserted, over the WHOLE file,
//! `sender.contains("persist_coin_status(") && sender.contains("CoinStatus::IN_TRANSFER")`. Every
//! clause of that is satisfied by something other than the arm-down:
//!
//!   * `persist_coin_status(` is satisfied by the function's OWN DEFINITION (`transfer_sender.rs`
//!     declares it), by the CANCEL restore path further down `execute_ex`, and by the restore arms
//!     in `tesr.rs` — none of which is a pre-conveyance arm-down;
//!   * `CoinStatus::IN_TRANSFER` occurs at ten other sites in the same file, two of them inside unit
//!     tests, i.e. inside the very file the guard scans;
//!   * there was NO ordering assertion at all, despite the failure message asserting the write
//!     happens "before the conveyance" — and ordering is the entire property. An arm-down that
//!     happens after `get_new_x1` is not a weaker arm-down, it is the original D1 defect;
//!   * a whole-file `contains` cannot tell the difference between a call and a `println!` carrying
//!     the same words, and cannot tell a REFUSAL (`?`) from a swallow (`.ok()`).
//!
//! An adversarial review deleted the entire A2 arm-down block from `execute_ex` on the real tree —
//! the very refusal whose message says it exists because otherwise the sender's tower "destroys
//! their allocation" — and this guard stayed GREEN, 3 passed. It was pinning prose.
//!
//! **What is pinned now**, for each lane, and only on CODE (comments blanked, string interiors
//! excluded, so no message and no test fixture can satisfy a clause):
//!
//!   1. the arm-down call is INSIDE the named function, in a window that begins at that function's
//!      column-anchored signature and ends at its column-anchored closing brace — both real symbols
//!      that must exist, never a byte budget and never "to end of file". The window is additionally
//!      asserted not to contain a second function signature, so it cannot have overshot into the
//!      next function and been satisfied by the wrong body;
//!   2. the arm-down's own statement carries `CoinStatus::IN_TRANSFER` — read from the statement's
//!      real extent (balanced delimiters, literals skipped), not from the next N bytes;
//!   3. the arm-down REFUSES on failure: its statement propagates (`?`) or returns `Err`/`bail!`.
//!      A `println!`, a `let _ =`, an `.ok()` or an `unwrap_or` fails this clause even though the
//!      words are identical;
//!   4. the arm-down's POSITION is before the position of EVERY call in that lane that can produce
//!      material for somebody else — the co-sign of the receiver-paying state, the backup co-sign,
//!      the coordinator open. Each of those markers must itself be present, so a rename cannot make
//!      the ordering vacuously true.
//!
//! And every one of those clauses is exercised by `the_audit_rejects_every_mutation_a_prose_pin_
//! survived`, which replants the deleted arm-down, the moved arm-down, the swallowed arm-down, the
//! arm-down-in-the-next-function and the arm-down-that-is-only-a-message against synthetic sources
//! and requires each to be REJECTED — a guard that cannot fail is decoration.

use std::path::PathBuf;

// ---------------------------------------------------------------------------------------------
// Reading the tree as CODE.
//
// Everything below works on a source in which comment bytes have been blanked (positions
// preserved) and the extent of every string literal is known. Both matter for this guard
// specifically: the arm-down it looks for is quoted verbatim inside the refusal messages next to
// it, inside the rationale comments above it, and inside the assertion messages of the unit tests
// at the bottom of the same files. A `contains` over raw text is satisfied by all three.
// ---------------------------------------------------------------------------------------------

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// A Rust source with comments blanked and string-literal extents recorded.
struct Source {
    /// Byte-for-byte the original, except every comment byte is a space (newlines kept), so every
    /// index below is an index into the real file.
    code: String,
    /// `(start, end)` of each string literal INCLUDING its quotes/hashes.
    strings: Vec<(usize, usize)>,
}

/// Blank comments, record literals. Handles `//`, nested `/* */`, escapes and raw strings; `'` is
/// deliberately treated as an ordinary character (these files contain no char literal holding a
/// quote, and lifetimes would otherwise have to be disambiguated).
fn scan(src: &str) -> Source {
    let b = src.as_bytes();
    let mut code: Vec<u8> = b.to_vec();
    let mut strings = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    code[i] = b' ';
                    i += 1;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                let mut depth = 0usize;
                while i < b.len() {
                    if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                        depth += 1;
                        code[i] = b' ';
                        code[i + 1] = b' ';
                        i += 2;
                        continue;
                    }
                    if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                        depth -= 1;
                        code[i] = b' ';
                        code[i + 1] = b' ';
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    if b[i] != b'\n' {
                        code[i] = b' ';
                    }
                    i += 1;
                }
            }
            b'r' if !is_ident_byte(if i == 0 { b' ' } else { b[i - 1] })
                && raw_string_hashes(b, i).is_some() =>
            {
                let hashes = raw_string_hashes(b, i).unwrap();
                let start = i;
                let mut j = i + 1 + hashes + 1; // past `r`, the hashes and the opening quote
                loop {
                    if j >= b.len() {
                        break;
                    }
                    if b[j] == b'"' && b[j + 1..].iter().take(hashes).all(|c| *c == b'#') {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                strings.push((start, j.min(b.len())));
                i = j;
            }
            b'"' => {
                let start = i;
                let mut j = i + 1;
                while j < b.len() {
                    match b[j] {
                        b'\\' => j += 2,
                        b'"' => {
                            j += 1;
                            break;
                        }
                        _ => j += 1,
                    }
                }
                strings.push((start, j.min(b.len())));
                i = j;
            }
            _ => i += 1,
        }
    }
    Source { code: String::from_utf8_lossy(&code).into_owned(), strings }
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// `r`, then zero or more `#`, then `"` — the raw-string opener, and how many hashes close it.
fn raw_string_hashes(b: &[u8], at: usize) -> Option<usize> {
    let mut j = at + 1;
    while j < b.len() && b[j] == b'#' {
        j += 1;
    }
    if j < b.len() && b[j] == b'"' {
        Some(j - at - 1)
    } else {
        None
    }
}

impl Source {
    /// Strictly inside a literal. A match that BEGINS at the opening quote is code (it is the
    /// literal itself); a match in the middle of one is text.
    fn inside_literal(&self, at: usize) -> bool {
        self.strings.iter().any(|(s, e)| at > *s && at < *e)
    }

    /// First occurrence of `needle` at or after `from` that is not inside a literal.
    fn find_code(&self, needle: &str, from: usize) -> Option<usize> {
        if from >= self.code.len() {
            return None;
        }
        self.code[from..]
            .match_indices(needle)
            .map(|(i, _)| i + from)
            .find(|i| !self.inside_literal(*i))
    }

    fn find_all_code(&self, needle: &str) -> Vec<usize> {
        self.code
            .match_indices(needle)
            .map(|(i, _)| i)
            .filter(|i| !self.inside_literal(*i))
            .collect()
    }

    /// The statement that STARTS at `at`, up to but not including its terminating `;`.
    ///
    /// Delimiters are balanced and literals are skipped whole, so neither a `;` inside a refusal
    /// message nor one inside a `map_err` closure ends it early, and the returned extent is the
    /// real one rather than a byte budget. `None` means the statement is not terminated inside the
    /// window — which every caller treats as a failure, never as a pass.
    fn statement<'a>(&'a self, at: usize, limit: usize) -> Option<&'a str> {
        let b = self.code.as_bytes();
        let mut depth = 0i32;
        let mut i = at;
        while i < limit.min(b.len()) {
            if let Some(e) = self.strings.iter().find(|(s, _)| *s == i).map(|(_, e)| *e) {
                i = e; // skip the whole literal: its `;`, braces and parens are text
                continue;
            }
            match b[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth < 0 {
                        return None; // ran out of the enclosing block before the statement ended
                    }
                }
                b';' if depth == 0 => return Some(&self.code[at..i]),
                _ => {}
            }
            i += 1;
        }
        None
    }
}

/// A statement that REFUSES: the failure becomes the caller's failure. `?` is the shape all four
/// lanes use; `return Err` / `bail!` are accepted because they are the same property written
/// longhand. Everything else — `.ok()`, `unwrap_or`, `let _ =`, a `println!` with the same words —
/// is a swallow, and a swallowed arm-down leaves the tower armed exactly as a missing one does.
fn refuses(statement: &str) -> bool {
    let t = statement.trim_end();
    t.ends_with('?') || t.contains("return Err(") || t.contains("bail!(")
}

/// Every column-anchored function opener. Used to prove a window did not overshoot: if a second one
/// appears inside the window, the terminator was wrong and every assertion below it could be
/// satisfied by the WRONG function's text.
/// Both indentation levels: a free function's window must not swallow the next free function, and a
/// method's window must not swallow the next method.
const FN_OPENERS: [&str; 12] = [
    "\nfn ",
    "\npub fn ",
    "\nasync fn ",
    "\npub async fn ",
    "\npub(crate) fn ",
    "\npub(crate) async fn ",
    "\n    fn ",
    "\n    pub fn ",
    "\n    async fn ",
    "\n    pub async fn ",
    "\n    pub(crate) fn ",
    "\n    pub(crate) async fn ",
];

/// The body of the function whose signature is `signature` (leading `\n` = column anchored),
/// running to `terminator` (its column-anchored closing brace).
///
/// Both ends are REAL SYMBOLS that must be found: a missing signature and a missing terminator are
/// failures, never `unwrap_or(len)` and never a slice to end of file. An overshoot is a failure
/// too, so the window can never be silently widened into the next function.
fn window(src: &Source, signature: &str, terminator: &str) -> Result<(usize, usize), String> {
    let at = src.find_code(signature, 0).ok_or_else(|| {
        format!(
            "`{}` no longer exists — this guard has lost its subject and must be re-derived against \
             the lane that replaced it, not deleted",
            signature.trim()
        )
    })?;
    let start = at + 1; // past the anchoring newline
    let end = src.find_code(terminator, start).ok_or_else(|| {
        format!(
            "`{}` has no column-anchored closing brace `{}` — refusing to scan to end of file, \
             because a window that overshoots makes every assertion below it satisfiable by another \
             function's text",
            signature.trim(),
            terminator.escape_debug()
        )
    })? + 1;
    for opener in FN_OPENERS {
        if let Some(i) = src.find_code(opener, start) {
            if i < end {
                return Err(format!(
                    "the window for `{}` swallowed a second function signature (`{}` at byte {i}) — \
                     the terminator is wrong, and every ordering assertion in this guard would be \
                     comparing positions across two different functions",
                    signature.trim(),
                    opener.trim()
                ));
            }
        }
    }
    Ok((start, end))
}

// ---------------------------------------------------------------------------------------------
// The property, as a pure audit — so it can be run against the real tree AND against planted
// mutations.
// ---------------------------------------------------------------------------------------------

struct Lane {
    /// Repo-relative file.
    file: &'static str,
    /// Column-anchored signature of the conveying function.
    signature: &'static str,
    /// Column-anchored terminator of its body.
    terminator: &'static str,
    /// The durable arm-down call.
    arm_down: &'static str,
    /// The status it must write.
    status: &'static str,
    /// Every call in this lane that can produce material for somebody else. Each must be PRESENT
    /// (or the ordering below is vacuous) and each must come AFTER the arm-down.
    conveyance: &'static [&'static str],
}

/// The three whole-coin / whole-child lanes A2 covered. `execute_ex` is the whole-coin hop, where
/// the loss is the entire coin rather than one piece; the two child lanes are the leaf equivalent.
const LANES: [Lane; 3] = [
    Lane {
        file: "clients/libs/rust/src/transfer_sender.rs",
        signature: "\nasync fn execute_ex(",
        terminator: "\n}\n",
        arm_down: "persist_coin_status(",
        status: "CoinStatus::IN_TRANSFER",
        // `presign_receiver_state` / `cosign_colored_receiver_state` co-sign the receiver-paying
        // `S'`; `create_backup_transactions` co-signs a backup and raises `num_sigs`; `get_new_x1`
        // OPENS the transfer at the coordinator, after which the recipient can complete the
        // handover. `transfer/update_msg` posts the ciphertext and is strictly below `get_new_x1`,
        // so pinning the open pins it too — and pinning a route string rather than a call symbol is
        // exactly the sort of prose pin this rewrite exists to remove.
        conveyance: &[
            "presign_receiver_state(",
            "cosign_colored_receiver_state(",
            "create_backup_transactions(",
            "get_new_x1(",
        ],
    },
    Lane {
        file: "clients/libs/rust/src/tesr.rs",
        signature: "\npub async fn child_retransfer(",
        terminator: "\n}\n",
        arm_down: "persist_coin_status(",
        status: "CoinStatus::IN_TRANSFER",
        conveyance: &["cosign_tier(", "persist_child(", "convey_child_bundle("],
    },
    Lane {
        file: "clients/libs/rust/src/tesr.rs",
        signature: "\npub async fn cosign_colored_child_retransfer(",
        terminator: "\n}\n",
        arm_down: "persist_coin_status(",
        status: "CoinStatus::IN_TRANSFER",
        conveyance: &["cosign_tier(", "persist_child(", "convey_child_bundle("],
    },
];

/// The arm-down of `lane` inside `[start, end)`, plus why each rejected candidate is not one.
///
/// EVERY call of the arm-down inside the window is a candidate, and the first one that is a real,
/// durable, refusing write of the arm-down status is the arm-down. Candidates are enumerated rather
/// than taken first-and-only because these lanes legitimately call the same function again on the
/// ABORT path (`persist_coin_status(.., status_before_conveyance)` inside a `match`), and that
/// restore is not an arm-down: it puts the coin back.
fn arm_down_in(src: &Source, lane: &Lane, start: usize, end: usize) -> (Option<usize>, Vec<String>) {
    let mut rejected: Vec<String> = Vec::new();
    for at in src.find_all_code(lane.arm_down).into_iter().filter(|i| *i >= start && *i < end) {
        let Some(stmt) = src.statement(at, end) else {
            rejected.push(format!(
                "  byte {at}: the statement is not terminated inside the function body, so its \
                 extent is unknown and the guard refuses to guess it (an abort-path `match` on the \
                 same call looks like this)"
            ));
            continue;
        };
        if !stmt.contains(lane.status) {
            rejected.push(format!(
                "  byte {at}: does not write `{}` — L1 is an ALLOWLIST on `CONFIRMED`, so a write \
                 of any other status is what stands the tower down, and a write that leaves the row \
                 in the allowlist is not an arm-down at all:\n---\n{stmt}\n---",
                lane.status
            ));
            continue;
        }
        if !refuses(stmt) {
            rejected.push(format!(
                "  byte {at}: does not REFUSE on failure — it must propagate with `?` (or \
                 `return Err`/`bail!`). A swallowed arm-down conveys the coin with the row still \
                 reading CONFIRMED, which is byte for byte the state a MISSING arm-down produces, \
                 except that the printed message makes it look handled:\n---\n{stmt}\n---"
            ));
            continue;
        }
        return (Some(at), rejected);
    }
    (None, rejected)
}

/// PRESENT, DURABLE, REFUSING, and FIRST. All four, or the lane conveys against an armed tower.
fn audit_lane(src: &Source, lane: &Lane) -> Result<(), String> {
    let (start, end) = window(src, lane.signature, lane.terminator)?;
    let fname = lane.signature.trim();

    let (arm, rejected) = arm_down_in(src, lane, start, end);

    let arm = arm.ok_or_else(|| {
        format!(
            "{}: `{fname}` contains no `{}` CALL that durably writes `{}` and refuses on failure. \
             `defend_ladders` reads the coin's status from the wallet DB on every pass, so without \
             that write inside this function there is a window in which the recipient holds the \
             superseding state while this wallet's DISK still says CONFIRMED — the tower is \
             admitted by L1 and broadcasts the sender's retained state over the same outpoint. On a \
             coloured coin that destroys the recipient's allocation. (Occurrences inside comments \
             and inside string literals do not count: this file quotes the call in its own refusal \
             messages and in its unit tests, which is exactly how the previous version of this \
             guard stayed green through the deletion of the whole block.){}",
            lane.file,
            lane.arm_down,
            lane.status,
            if rejected.is_empty() {
                String::new()
            } else {
                format!("\nCandidates considered and rejected:\n{}", rejected.join("\n"))
            }
        )
    })?;

    for marker in lane.conveyance {
        let at = src.find_code(marker, start).filter(|i| *i < end).ok_or_else(|| {
            format!(
                "{}: `{fname}` no longer calls `{marker}` — this guard orders the arm-down against \
                 that call, so its disappearance makes the ordering vacuously true. Re-derive the \
                 lane's first material-producing step; do not drop the marker.",
                lane.file
            )
        })?;
        if arm > at {
            return Err(format!(
                "{}: in `{fname}` the arm-down (byte {arm}) comes AFTER `{marker}` (byte {at}). \
                 That is the D1 defect verbatim, not a weaker version of the fix: between the \
                 co-sign and the status write, a `defend_ladders` pass reads a stale CONFIRMED, is \
                 admitted, and broadcasts the retained state against the one the recipient now \
                 holds over the same output. The arm-down must be hoisted above every step that can \
                 produce material for anybody else.",
                lane.file
            ));
        }
    }

    Ok(())
}

/// THE IN-LADDER CENSUS. Every co-sign of an in-ladder split must be preceded, IN ITS OWN METHOD,
/// by a durable arm-down that writes `IN_TRANSFER` and refuses on failure.
///
/// The enclosing method is found by its column-anchored signature rather than by a byte budget:
/// "an arm-down within 3,000 characters" is satisfied by the PREVIOUS lane's arm-down whenever two
/// lanes sit close together, which is precisely how these four are laid out.
fn audit_in_ladder_census(src: &Source) -> Result<(), String> {
    const COSIGNS: [&str; 2] = [
        "mercuryrustlib::tesr::in_ladder_split(",
        "mercuryrustlib::tesr::child_in_ladder_split(",
    ];
    const METHOD_OPENERS: [&str; 3] =
        ["\n    pub async fn ", "\n    async fn ", "\n    pub(crate) async fn "];
    const ARM: &str = "self.set_coin_status(";
    const STATUS: &str = "CoinStatus::IN_TRANSFER";

    let mut cosigns: Vec<usize> =
        COSIGNS.iter().flat_map(|m| src.find_all_code(m)).collect();
    cosigns.sort_unstable();
    if cosigns.len() < 4 {
        return Err(format!(
            "expected at least the four in-ladder lanes (child_in_ladder_pay, \
             child_in_ladder_pay_many, in_ladder_pay, in_ladder_pay_many), found {} co-sign call \
             sites — this census has lost its subject",
            cosigns.len()
        ));
    }

    for c in cosigns {
        let method = METHOD_OPENERS
            .iter()
            .flat_map(|o| src.find_all_code(o))
            .filter(|m| *m < c)
            .max()
            .ok_or_else(|| {
                format!(
                    "the in-ladder co-sign at byte {c} is not inside any column-anchored method — \
                     the guard cannot bound its search window and refuses to guess one"
                )
            })?;

        let armed = src
            .find_all_code(ARM)
            .into_iter()
            .filter(|a| *a > method && *a < c)
            .any(|a| src.statement(a, c).is_some_and(|s| s.contains(STATUS) && refuses(s)));

        if !armed {
            return Err(format!(
                "the in-ladder split co-signing at byte {c} has no durable, refusing \
                 `{ARM}…{STATUS}` between its own method's signature (byte {method}) and the \
                 co-sign. `defend_ladders` re-reads the coin's status from the wallet DB on every \
                 pass, so between that co-sign and whenever this lane writes the status, the \
                 wallet's own tower is armed with a state that rivals the one the recipients now \
                 hold over the same outpoint. sdk80 measured that window open in 1 of 1 samples. \
                 (An arm-down in a NEIGHBOURING method does not count — that is what the old \
                 3,000-byte proximity test accepted.)"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The invariants, on the real tree.
// ---------------------------------------------------------------------------------------------

/// THE THREE LANES A2 ALREADY COVERED must stay covered — a regression there is the same defect on
/// the whole-coin hop, where the loss is the entire coin rather than one piece.
#[test]
fn the_whole_coin_lanes_arm_down_inside_the_lane_and_before_the_conveyance() {
    for lane in &LANES {
        let src = scan(&read(lane.file));
        if let Err(e) = audit_lane(&src, lane) {
            panic!("{e}");
        }
    }
}

/// THE CENSUS. Every co-sign of an in-ladder split must be preceded by a durable arm-down.
#[test]
fn every_in_ladder_cosign_is_preceded_by_a_durable_arm_down() {
    let src = scan(&read("clients/libs/rust-sdk/src/transfer.rs"));
    if let Err(e) = audit_in_ladder_census(&src) {
        panic!("{e}");
    }
}

/// THE RULE THE ARM-DOWN SERVES must still exist. If the tower stopped keying on `CONFIRMED`, or
/// stopped re-reading the status per pass, the arm-down would be decorative and the real defence
/// something else entirely — in which case this census pins the wrong thing and must be re-derived
/// rather than deleted.
///
/// Scoped to `defend_ladders_inner`'s own body: `wallet.rs` mentions `CoinStatus::CONFIRMED` in
/// several unrelated places, so a whole-file `contains` here would survive the loop's L1 filter
/// being removed outright.
#[test]
fn the_tower_still_keys_on_a_confirmed_allowlist() {
    let src = scan(&read("clients/libs/rust-sdk/src/wallet.rs"));
    let (start, end) = window(&src, "\n    async fn defend_ladders_inner(", "\n    }\n")
        .unwrap_or_else(|e| panic!("{e}"));

    for needle in ["CoinStatus::CONFIRMED", "live_sids"] {
        assert!(
            src.find_code(needle, start).is_some_and(|i| i < end),
            "`defend_ladders_inner` no longer references `{needle}` — L1 has changed shape, so the \
             arm-down this file enforces may now be protecting a rule that does not exist. \
             Re-derive the census against the new filter."
        );
    }
}

/// **THE MUTATION, ON THE REAL FILE.** Everything else here plants defects in synthetic sources;
/// this one excises the shipped arm-down from the shipped `execute_ex` — mechanically, by the
/// statement's real extent rather than by matching its text — and requires the audit to go red.
///
/// It also asserts, on the same mutated bytes, that the OLD pin
/// (`contains("persist_coin_status(") && contains("CoinStatus::IN_TRANSFER")`) is still SATISFIED.
/// That pair of assertions is the whole rewrite in one place: the file the reviewer produced by
/// deleting the refusal whose own message says it exists so the sender's tower does not "destroy
/// their allocation" still passed the old guard, and cannot pass this one.
/// Excise the whole statement beginning at `at`: back to the start of its line, forward past its
/// terminating `;`. This is the mutation, applied mechanically by the statement's REAL extent
/// rather than by matching its text, so it stays valid as the lanes are edited.
fn excise_statement(raw: &str, src: &Source, at: usize, end: usize) -> String {
    let stmt = src.statement(at, end).expect("the statement's extent must be known to excise it");
    let line_start = raw[..at].rfind('\n').expect("the statement is not on the first line") + 1;
    format!("{}{}", &raw[..line_start], &raw[at + stmt.len() + 1..])
}

#[test]
fn deleting_a_shipped_arm_down_goes_red_here_and_stayed_green_under_the_old_pin() {
    for lane in &LANES {
        let raw = read(lane.file);
        let src = scan(&raw);
        let (start, end) =
            window(&src, lane.signature, lane.terminator).unwrap_or_else(|e| panic!("{e}"));
        let (arm, _) = arm_down_in(&src, lane, start, end);
        let arm = arm.expect("the shipped arm-down must be locatable before it can be excised");
        let mutated = excise_statement(&raw, &src, arm, end);

        // THE OLD PIN, verbatim, on the mutated bytes.
        assert!(
            mutated.contains(lane.arm_down) && mutated.contains(lane.status),
            "the OLD pin must still be satisfied after the deletion — if it is not, this test has \
             stopped demonstrating why a whole-file `contains` was decoration ({})",
            lane.file
        );

        // The new one.
        let e = audit_lane(&scan(&mutated), lane).unwrap_err();
        assert!(
            e.contains(lane.signature.trim())
                && (e.contains("CALL that durably writes") || e.contains("comes AFTER")),
            "removing the shipped arm-down from `{}` must be refused by name:\n{e}",
            lane.signature.trim()
        );
    }
}

/// The same demonstration for the in-ladder census: excise ONE lane's arm-down from the shipped
/// `transfer.rs` and the census must name it. The old census would also have caught a deletion —
/// but only because the four lanes are far enough apart; it accepted an arm-down up to 3,000 bytes
/// away, i.e. one belonging to a neighbouring method, which is the shape
/// `the_census_rejects_the_lane_shapes_it_exists_to_catch` plants.
#[test]
fn deleting_a_shipped_in_ladder_arm_down_goes_red() {
    let raw = read("clients/libs/rust-sdk/src/transfer.rs");
    let src = scan(&raw);
    let cosign = src
        .find_all_code("mercuryrustlib::tesr::child_in_ladder_split(")
        .into_iter()
        .min()
        .expect("the in-ladder co-signs must exist");
    let arm = src
        .find_all_code("self.set_coin_status(")
        .into_iter()
        .filter(|a| *a < cosign)
        .filter(|a| {
            src.statement(*a, cosign)
                .is_some_and(|s| s.contains("CoinStatus::IN_TRANSFER") && refuses(s))
        })
        .max()
        .expect("the first in-ladder lane must have a qualifying arm-down");

    let mutated = excise_statement(&raw, &src, arm, cosign);
    let e = audit_in_ladder_census(&scan(&mutated)).unwrap_err();
    assert!(
        e.contains("has no durable, refusing"),
        "deleting an in-ladder lane's arm-down must be refused by name:\n{e}"
    );
}

// ---------------------------------------------------------------------------------------------
// NON-VACUITY: the mutations a prose pin survived.
//
// These are not "a test that could fail in principle" — they are the actual defect shapes, run
// through the same audit the tests above run, and each is required to be REJECTED. The first case
// is the one the adversarial review applied to the real tree while the old guard reported 3 passed.
// ---------------------------------------------------------------------------------------------

/// A synthetic `execute_ex`-shaped lane, so a mutation can be planted without touching the tree.
fn planted_lane() -> Lane {
    Lane {
        file: "<planted>",
        signature: "\nasync fn execute_ex(",
        terminator: "\n}\n",
        arm_down: "persist_coin_status(",
        status: "CoinStatus::IN_TRANSFER",
        conveyance: &["create_backup_transactions(", "get_new_x1("],
    }
}

/// The lane's preamble and signature. Note the COMMENT quoting the arm-down verbatim: the shipped
/// files do exactly this (the rationale note above the real call names it), and a guard that reads
/// raw text is satisfied by the note alone.
const HEAD: &str = "\
use anyhow::anyhow;

async fn execute_ex(cc: &ClientConfig, sid: String) -> Result<()> {
    // persist_coin_status(cc, name, &sid, CoinStatus::IN_TRANSFER) is hoisted here, ahead of every
    // step that can produce material for anybody else.
";

/// The durable, refusing arm-down, in the shape all three lanes ship.
const ARM: &str = "\
    let status_before_conveyance = persist_coin_status(cc, name, &sid, CoinStatus::IN_TRANSFER)
        .await
        .map_err(|e| {
            anyhow!(
                \"refusing to transfer statechain id {sid}: its coin could not be durably marked \\
                 IN_TRANSFER before the conveyance ({e}); nothing has been co-signed.\"
            )
        })?;
";

/// The two steps that produce material for somebody else.
const CONVEY: &str = "\
    let backups = create_backup_transactions(cc, addr, &mut wallet, &sid).await?;
    let x1 = get_new_x1(cc, &sid).await?;
";

/// The end of the lane, plus the function that follows it in the real file — present so that every
/// case below has something for the window to wrongly overshoot INTO.
const TAIL: &str = "\
    update_wallet(&cc.pool, &wallet).await?;
    Ok(())
}

async fn create_backup_tx_to_receiver(cc: &ClientConfig) -> Result<String> {
    Ok(String::new())
}
";

fn lane_source(parts: &[&str]) -> String {
    let mut s = String::from("\n");
    for p in parts {
        s.push_str(p);
    }
    s
}

#[test]
fn the_audit_accepts_the_shipped_shape() {
    let src = scan(&lane_source(&[HEAD, ARM, CONVEY, TAIL]));
    assert_eq!(
        audit_lane(&src, &planted_lane()),
        Ok(()),
        "the shipped shape must pass, or the rejections below prove nothing"
    );
}

#[test]
fn the_audit_rejects_every_mutation_a_prose_pin_survived() {
    // The call's own DEFINITION, and the status named elsewhere — both present in the real file,
    // and between them they satisfied every clause of the old whole-file `contains` pin.
    const DEFINITION: &str = "\
pub(crate) async fn persist_coin_status(cc: &ClientConfig) -> Result<CoinStatus> {
    let status = CoinStatus::IN_TRANSFER;
    Ok(status)
}
";
    // The arm-down, in the NEXT function. Real, durable, refusing — and useless to this lane.
    const ARM_IN_ANOTHER_FN: &str = "\
async fn some_other_lane(cc: &ClientConfig) -> Result<()> {
    persist_coin_status(cc, name, &sid, CoinStatus::IN_TRANSFER).await?;
    Ok(())
}
";
    const SWALLOWED: &str =
        "    let _ = persist_coin_status(cc, name, &sid, CoinStatus::IN_TRANSFER).await.ok();\n";
    const PRINTED: &str =
        "    println!(\"persist_coin_status(cc, name, &sid, CoinStatus::IN_TRANSFER) before the conveyance\");\n";
    const WRONG_STATUS: &str =
        "    let previous = persist_coin_status(cc, name, &sid, CoinStatus::CONFIRMED).await?;\n";

    let cases: [(&str, String, &str); 7] = [
        (
            // THE PROVEN ONE. The adversarial review deleted this exact block from the real
            // `execute_ex` and the old guard stayed GREEN, 3 passed: `persist_coin_status(` was
            // still satisfied by the function's own definition, and `CoinStatus::IN_TRANSFER` by
            // ten other sites in the same file, two of them unit tests.
            "arm_down_block_deleted",
            lane_source(&[HEAD, CONVEY, TAIL, DEFINITION]),
            "contains no `persist_coin_status(` CALL",
        ),
        (
            // Present, durable, refusing — but AFTER the co-sign. The old pin had no ordering
            // assertion at all, despite its failure message claiming "before the conveyance".
            "arm_down_moved_below_the_conveyance",
            lane_source(&[HEAD, CONVEY, ARM, TAIL]),
            "comes AFTER `create_backup_transactions(`",
        ),
        (
            // The swallow: same call, same status, same words, no refusal. The coin is conveyed
            // with the row still reading CONFIRMED — byte for byte the state the deleted arm-down
            // produced.
            "arm_down_swallowed",
            lane_source(&[HEAD, SWALLOWED, CONVEY, TAIL]),
            "does not REFUSE on failure",
        ),
        (
            // The message that looks like the code. A `contains` cannot tell these apart; this is
            // the shape the review named explicitly.
            "arm_down_is_only_a_printed_message",
            lane_source(&[HEAD, PRINTED, CONVEY, TAIL]),
            "contains no `persist_coin_status(` CALL",
        ),
        (
            // L1 is an ALLOWLIST on CONFIRMED. A write that leaves the row inside the allowlist is
            // not an arm-down, however durable and however loudly it refuses.
            "arm_down_writes_a_status_still_in_the_allowlist",
            lane_source(&[HEAD, WRONG_STATUS, CONVEY, TAIL]),
            "does not write `CoinStatus::IN_TRANSFER`",
        ),
        (
            // THE WINDOW TEST. The arm-down exists in the file, refuses, writes the right status —
            // and belongs to a different function. A window that runs to end of file, or one
            // bounded by a byte count, accepts this; a window bounded by the lane's own closing
            // brace cannot.
            "arm_down_lives_in_the_next_function",
            lane_source(&[HEAD, CONVEY, TAIL, ARM_IN_ANOTHER_FN]),
            "contains no `persist_coin_status(` CALL",
        ),
        (
            // The ordering must not be vacuous either: rename the step being ordered against and
            // the guard has to say so rather than pass.
            "the_conveyance_marker_disappeared",
            lane_source(&[HEAD, ARM, CONVEY, TAIL])
                .replace("create_backup_transactions(", "co_sign_backups("),
            "no longer calls `create_backup_transactions(`",
        ),
    ];

    for (tag, body, expected) in cases {
        let src = scan(&body);
        match audit_lane(&src, &planted_lane()) {
            Ok(()) => panic!(
                "planted defect `{tag}` was NOT caught — this guard would stay green through the \
                 mutation it exists to stop:\n---\n{body}\n---"
            ),
            Err(e) => assert!(
                e.contains(expected),
                "planted defect `{tag}` was caught for the WRONG reason (the failure must name \
                 `{expected}`):\n{e}"
            ),
        }
    }

    // ...and an unterminated lane is a failure, never a silent slice to end of file.
    let truncated = lane_source(&[HEAD, ARM, CONVEY, "    Ok(())\n"]);
    let e = audit_lane(&scan(&truncated), &planted_lane()).unwrap_err();
    assert!(
        e.contains("no column-anchored closing brace"),
        "a lane whose body is not terminated must fail loudly:\n{e}"
    );
}

// ---------------------------------------------------------------------------------------------
// NON-VACUITY for the in-ladder census.
// ---------------------------------------------------------------------------------------------

/// One in-ladder lane, in the shape all four ship.
fn census_lane(name: &str, arm: &str, cosign: &str) -> String {
    format!(
        "    pub async fn {name}(&self, sid: &str) -> Result<()> {{\n{arm}        let split = \
         mercuryrustlib::tesr::{cosign}(&self.inner.cc).await?;\n        Ok(())\n    }}\n"
    )
}

const CENSUS_ARM: &str = "\
        self.set_coin_status(sid, CoinStatus::IN_TRANSFER)
            .await
            .map_err(|e| anyhow!(\"refusing the in-ladder split of {sid} ({e})\"))?;
";

fn census_source(lanes: &[(&str, &str, &str)]) -> String {
    let mut s = String::from("\nimpl UtexoWallet {\n");
    for (name, arm, cosign) in lanes {
        s.push_str(&census_lane(name, arm, cosign));
        s.push('\n');
    }
    s.push_str("}\n");
    s
}

fn shipped_four() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("child_in_ladder_pay", CENSUS_ARM, "child_in_ladder_split"),
        ("child_in_ladder_pay_many", CENSUS_ARM, "child_in_ladder_split"),
        ("in_ladder_pay", CENSUS_ARM, "in_ladder_split"),
        ("in_ladder_pay_many", CENSUS_ARM, "in_ladder_split"),
    ]
}

#[test]
fn the_census_rejects_the_lane_shapes_it_exists_to_catch() {
    assert_eq!(
        audit_in_ladder_census(&scan(&census_source(&shipped_four()))),
        Ok(()),
        "the shipped four-lane shape must pass"
    );

    // A FIFTH LANE, added tomorrow, with no arm-down at all. This is the failure mode the census
    // exists for: L1's allowlist admits the coin silently, so nothing else notices.
    let mut lanes = shipped_four();
    lanes.push(("spine_batch_pay", "", "in_ladder_split"));
    assert!(
        audit_in_ladder_census(&scan(&census_source(&lanes))).is_err(),
        "a new in-ladder lane with no arm-down must be refused — that is the whole point of a \
         census rather than four assertions"
    );

    // THE PROXIMITY DEFECT the old test accepted: the last lane's arm-down is gone, but the
    // PREVIOUS lane's arm-down sits a few hundred bytes above it, well inside a 3,000-byte
    // "nearest preceding arm-down" budget.
    let mut lanes = shipped_four();
    lanes.last_mut().unwrap().1 = "";
    let borrowed = census_source(&lanes);
    assert!(
        borrowed.contains("CoinStatus::IN_TRANSFER"),
        "the neighbour's arm-down must still be present, or this case proves nothing"
    );
    assert!(
        audit_in_ladder_census(&scan(&borrowed)).is_err(),
        "an arm-down in the PRECEDING method must not satisfy this lane — proximity is not \
         containment"
    );

    // A swallowed arm-down in one lane: the words are all there, the failure is not.
    let mut lanes = shipped_four();
    lanes[1].1 = "        let _ = self.set_coin_status(sid, CoinStatus::IN_TRANSFER).await;\n";
    assert!(
        audit_in_ladder_census(&scan(&census_source(&lanes))).is_err(),
        "an arm-down whose failure is discarded conveys the pieces with the row still CONFIRMED, \
         and must be refused"
    );

    // A status write that leaves the row inside the allowlist is not an arm-down.
    let mut lanes = shipped_four();
    lanes[2].1 = "        self.set_coin_status(sid, CoinStatus::CONFIRMED).await?;\n";
    assert!(
        audit_in_ladder_census(&scan(&census_source(&lanes))).is_err(),
        "writing a status the tower still broadcasts on is not an arm-down"
    );

    // An arm-down that only appears in a MESSAGE.
    let mut lanes = shipped_four();
    lanes[3].1 =
        "        println!(\"self.set_coin_status(sid, CoinStatus::IN_TRANSFER) before the co-sign\");\n";
    assert!(
        audit_in_ladder_census(&scan(&census_source(&lanes))).is_err(),
        "a printed description of the arm-down is not an arm-down"
    );

    // And the subject must still exist: an empty tree is a loud failure, not a silent pass.
    assert!(
        audit_in_ladder_census(&scan("fn nothing() {}\n")).is_err(),
        "a tree with no in-ladder co-signs must fail — a census whose subject vanished is pinning \
         nothing"
    );
}
