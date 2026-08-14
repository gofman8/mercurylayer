//! **[D40 / A.2] The deadline defence is UNCONDITIONAL. Routine re-anchoring is not.**
//!
//! A whole laddered coin carries exactly one absolute clock: `min(L_k)` over its flat backup chain,
//! and that chain is held by its PRIOR OWNERS. Every tier below `F` is *relative*-timelocked, so
//! none of them can out-race a transaction that is simply valid now. When `L_k` passes, any
//! ancestor's matured rung spends `F` and takes the coin.
//!
//! # How it came to be optional
//!
//! `auto_refresh_due` did two unrelated jobs under one name:
//!
//! * **routine background re-anchoring** — an ECONOMICS choice. B4 folds the re-anchor cost into
//!   `transfer` and pays it on demand, so a running wallet must not silently shrink a balance in the
//!   background. Correctly opt-in, correctly off by default.
//! * **the deadline defence** — a SAFETY property.
//!
//! Both sat behind `auto_refresh && background_auto_refresh`. Turning the economics off turned the
//! safety off with it, and the comment at the call site asserted the gap was covered elsewhere —
//! *"deadline safety for idle wallets is the `auto_exit` pass below"* — which is false.
//! `auto_exit_due` protects sub-coins and materialises carriers; the whole-coin clock had **no
//! scheduled defender at all** on a default wallet.
//!
//! # And why the fallback is load-bearing
//!
//! Re-anchoring is COOPERATIVE — one fresh SE co-signature. Under D40.1 the party most interested in
//! this deadline passing is the operator, i.e. the same party being asked to sign. **A defence its
//! adversary can decline is not a defence.** So the pass falls back to severing from `F`: broadcast
//! the already-co-signed trigger, which is un-timelocked and therefore wins against every retained
//! rung by being valid first, with no SE and no counterparty.
//!
//! # Why THIS GUARD had to be rewritten (the pin was weaker than its own name)
//!
//! An adversarial review applied the obvious mutation to the real tree and watched this file stay
//! green. Two separate failures, one mechanical and one substantive.
//!
//! **Mechanical.** The `else`-arm check was written as
//!
//! ```text
//! arm.contains("} else {") && arm[..arm.find("…").unwrap_or(arm.len()).min(arm.len())].len() > 0
//! ```
//!
//! The second conjunct is LITERALLY VACUOUS: `unwrap_or(arm.len())` guarantees a valid index,
//! `.min(arm.len())` guarantees it again, and the length of a slice that starts at 0 and ends at a
//! non-zero index is never `> 0`-false for any arm long enough to contain `} else {` at all. It
//! could not fail. It was also an instance of the very shape the guard rules forbid — a scan window
//! that FALLS BACK to "the rest of the text" rather than terminating on a real symbol, so it can
//! overshoot into the next construct and let the wrong code satisfy the assertion.
//!
//! **Substantive, and the reason for the rewrite.** The test is named
//! `the_background_loop_always_runs_a_deadline_pass`, but what it actually asserted was
//! *"`deadline_safety_due` appears somewhere in the method, and the routine-refresh gate has an
//! `else` arm, and that `else` arm calls it."* That is a strictly WEAKER statement, and the shape it
//! permits is the shape that shipped:
//!
//! ```text
//! if auto_refresh && background_auto_refresh {
//!     auto_refresh_due(..).await;        // <- routine pass only. NO deadline pass on this path.
//! } else {
//!     deadline_safety_due(..).await;     // <- the guard was satisfied by this arm alone
//! }
//! ```
//!
//! A wallet that OPTS INTO background maintenance thereby LOSES the sever. `auto_refresh_due` is the
//! cooperative half alone: it asks the SE to co-sign a fresh anchor and gives up when the SE
//! declines — which, under D40.1, is exactly what the adversary does. So the most safety-conscious
//! configuration on offer was the one with no unilateral fallback, and the guard's name over-claimed
//! a property nothing checked. Naming a property is not pinning it.
//!
//! # What is pinned now
//!
//! The property, not a description of it: **a deadline pass runs on EVERY path through the poll
//! loop.** That is checked structurally — the loop body is delimited by its own matching braces (not
//! by a byte count and not by a fallback to end-of-text), string literals and trailing comments are
//! blanked so nothing can be satisfied by TEXT, and `deadline_safety_due` must appear at brace-depth
//! ZERO of the loop body, i.e. as a statement of the loop itself rather than inside any `if`, `else`
//! or `match` arm. The complementary half is pinned too, in the opposite direction: `auto_refresh_due`,
//! if it is scheduled at all, must remain CONDITIONAL, because the economics must never become
//! unconditional either.
//!
//! The source shape this rewrite demands — hoisting `deadline_safety_due` out of the `else` — is
//! made separately by the orchestrator; this file is the guard that holds it afterwards.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Blank the CONTENTS of every trailing `//` comment and every double-quoted string literal,
/// preserving byte offsets and line structure so positions computed here still point at real source.
///
/// This is what makes the checks below non-textual. Without it, `println!("run deadline_safety_due
/// every pass")` satisfies a `contains` check, and `format!("{id}")` corrupts the brace-depth walk
/// that the whole "runs on every path" property rests on.
fn scrub(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b[i] == b'"' {
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

/// The body of the item starting at `sig`, bounded by the next COLUMN-4 `pub`/`fn` item (these are
/// inherent-impl methods, so they are indented). Never a fixed byte count and never a fallback to
/// end-of-text: a window that falls back to one is long enough to look like a body and short enough
/// to miss what is at the end of it, and — worse — it can run PAST the method into the next one, so
/// every assertion under it can be satisfied by the WRONG function's text. If the terminator is
/// genuinely gone the guard must fail loudly rather than widen.
fn method_body<'a>(code: &'a str, sig: &str) -> &'a str {
    let at = code.find(sig).unwrap_or_else(|| panic!("`{sig}` is gone"));
    let rest = &code[at + sig.len()..];
    let end = [
        "\n    pub async fn ",
        "\n    pub fn ",
        "\n    pub(crate) async fn ",
        "\n    pub(crate) fn ",
        "\n    async fn ",
        "\n    fn ",
    ]
    .iter()
    .filter_map(|m| rest.find(m))
    .min()
    .unwrap_or_else(|| {
        panic!(
            "`{sig}` has no following column-4 item to bound its body — refusing to scan to \
             end-of-file, because a window that overshoots lets the NEXT function's text satisfy \
             every assertion below it"
        )
    });
    let body = &code[at..at + sig.len() + end];
    assert!(body.len() > 300, "`{sig}` scanned to {} bytes — not a method body", body.len());
    body
}

/// The interior of the brace block whose `{` is the first one at or after `from`, delimited by its
/// own MATCHING `}`. The terminator is therefore a real symbol of the scanned source, computed from
/// it, and unbalanced input is a loud failure rather than a silent widening.
fn block_after(scrubbed: &str, from: usize, what: &str) -> (usize, usize) {
    let b = scrubbed.as_bytes();
    let open = (from..b.len())
        .find(|&i| b[i] == b'{')
        .unwrap_or_else(|| panic!("no `{{` opening the {what} block"));
    let mut depth = 0usize;
    for i in open..b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (open + 1, i);
                }
            }
            _ => {}
        }
    }
    panic!("the {what} block is never closed — braces are unbalanced");
}

/// Every occurrence of `needle` in `region`, with the brace-nesting depth at which it sits.
/// Depth 0 = a statement of the region's own block, i.e. something that runs on EVERY path through
/// it. Depth > 0 = inside an `if`, an `else`, a `match` arm or a closure — i.e. conditional.
fn sites(region: &str, needle: &str) -> Vec<(usize, usize)> {
    let b = region.as_bytes();
    let n = needle.as_bytes();
    let mut depth = 0usize;
    let mut out = Vec::new();
    for i in 0..b.len() {
        if b[i..].starts_with(n) {
            out.push((i, depth));
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    out
}

/// The statement a call site belongs to: from the site to the next `;`. A real symbol, present in
/// every statement of the scanned source; there is no byte-count fallback.
fn statement(region: &str, at: usize) -> &str {
    let end = region[at..].find(';').map(|d| at + d).unwrap_or_else(|| {
        panic!("the call at byte {at} is not terminated by `;` — cannot tell what it does")
    });
    &region[at..end]
}

/// THE PROPERTY, extracted so the non-vacuity tests below can drive it with source that is NOT this
/// repo's — a checker that only ever sees one input has never been shown to be able to say no.
///
/// Given the raw text of `start_background`, decide whether a deadline pass runs on every path
/// through its poll loop.
fn deadline_pass_is_unconditional(method_src: &str) -> Result<(), String> {
    let code = scrub(method_src);
    let loop_at = code
        .find("loop {")
        .ok_or_else(|| "there is no `loop {` — the background poll loop is gone".to_string())?;
    let (lo, hi) = block_after(&code, loop_at, "poll-loop");
    let body = &code[lo..hi];

    // Sanity that we are looking at the POLL loop and not some inner loop: the pass interval is
    // slept at the end of every iteration.
    if sites(body, "tokio::time::sleep(").iter().all(|(_, d)| *d != 0) {
        return Err(
            "the block found is not the poll loop (no unconditional `tokio::time::sleep(` in it) — \
             refusing to certify a loop this guard has not identified"
                .to_string(),
        );
    }

    // An early `continue` above the pass would reintroduce the very shape being fixed: a path
    // through the loop on which the deadline pass does not run.
    if let Some((at, _)) = sites(body, "continue").first().copied() {
        return Err(format!(
            "the poll loop contains `continue` at byte {at}. Any early continue creates a path \
             through the loop that skips whatever follows it, which is exactly the defect this \
             guard exists to deny — the deadline pass must not be skippable:\n\n{}",
            &body[..hi - lo]
        ));
    }

    let deadline = sites(body, "deadline_safety_due(");
    if deadline.is_empty() {
        return Err(format!(
            "the poll loop never calls `deadline_safety_due(`. The whole-coin `L_k` clock then has \
             NO scheduled defender — `auto_exit_due` protects sub-coins and materialises carriers, \
             not this:\n\n{body}"
        ));
    }
    let unconditional: Vec<usize> = deadline
        .iter()
        .filter(|(_, d)| *d == 0)
        .map(|(at, _)| *at)
        .filter(|at| statement(body, *at).contains(".await"))
        .collect();
    if unconditional.is_empty() {
        return Err(format!(
            "every `deadline_safety_due(` in the poll loop sits at brace-depth {:?} — inside an \
             `if`/`else`/`match` arm rather than as a statement of the loop. That is the shipped \
             defect: with the pass in the `else` of the routine-refresh gate, a wallet that OPTS \
             INTO background maintenance runs `auto_refresh_due` alone and thereby LOSES the \
             unilateral sever, which is the only remedy an uncooperative SE cannot decline \
             (D40.1). Hoist the call out of the arm:\n\n{body}",
            deadline.iter().map(|(_, d)| *d).collect::<Vec<_>>()
        ));
    }

    // The other direction. Routine re-anchoring is an ECONOMICS choice (B4 pays the re-anchor fee
    // on demand inside `transfer`), so it must never become unconditional either. If it is
    // scheduled here at all, it stays behind its flag.
    let routine = sites(body, "auto_refresh_due(");
    if !routine.is_empty() {
        if let Some((at, _)) = routine.iter().find(|(_, d)| *d == 0) {
            return Err(format!(
                "`auto_refresh_due(` runs unconditionally at byte {at} of the poll loop. Routine \
                 background re-anchoring is an economics choice that silently shrinks a balance; \
                 B4 pays it on demand in `transfer` instead. It must stay behind \
                 `auto_refresh && background_auto_refresh`:\n\n{body}"
            ));
        }
        if !body.contains("auto_refresh && ") {
            return Err(format!(
                "the routine-refresh pass is scheduled but its `auto_refresh && \
                 background_auto_refresh` gate is gone, so the economics half is now governed by \
                 something else — say what:\n\n{body}"
            ));
        }
    }
    Ok(())
}

/// THE SCHEDULING. Whatever the config says, a deadline pass runs every poll — on EVERY path, not
/// merely on the one the reader happens to trace.
#[test]
fn the_background_loop_always_runs_a_deadline_pass() {
    let code = code_only(&read("clients/libs/rust-sdk/src/wallet.rs"));
    let body = method_body(&code, "pub fn start_background(&self)");
    if let Err(why) = deadline_pass_is_unconditional(body) {
        panic!("{why}");
    }
}

/// NON-VACUITY, and the whole reason this file was rewritten. A guard that cannot say no is
/// decoration, and the previous one could not: the second of these cases — the shape actually in
/// the tree — passed it. Each case is source the checker must REJECT; if it accepts any of them it
/// would not have caught the defect it is named for.
#[test]
fn guard_rejects_every_shape_that_leaves_a_path_without_a_deadline_pass() {
    let cases: [(&str, &str); 5] = [
        (
            // The original defect: both jobs behind the economics flag. A default wallet
            // (background_auto_refresh = false) ran neither.
            "both_jobs_behind_the_economics_flag",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                if wallet.inner.config.auto_refresh && wallet.inner.config.background_auto_refresh {
                    let _ = wallet.auto_refresh_due(margin).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
        (
            // ** THE MUTATION THE OLD GUARD SURVIVED. ** The pass exists, the gate has an `else`,
            // the `else` calls it — every conjunct of the old test is satisfied — and a wallet that
            // opted into background maintenance still has no sever.
            "deadline_pass_only_in_the_else_arm",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                if wallet.inner.config.auto_refresh && wallet.inner.config.background_auto_refresh {
                    let _ = wallet.auto_refresh_due(margin).await;
                } else {
                    let _ = wallet.deadline_safety_due(margin).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
        (
            // Hoisted out of the `else` but wrapped in a different flag. Still optional, still a
            // path through the loop with no deadline pass.
            "hoisted_but_re_gated_under_another_flag",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                if wallet.inner.config.auto_exit {
                    let _ = wallet.deadline_safety_due(margin).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
        (
            // TEXT is not a pass. The name appears unconditionally — in a log line and a comment —
            // while the call itself is still in the arm. This is the case a `contains` check on the
            // raw body cannot tell from the real thing.
            "named_in_a_string_and_a_comment_but_called_in_an_arm",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                // every pass runs deadline_safety_due( ) unconditionally
                println!("scheduling deadline_safety_due( ) for {id}");
                if wallet.inner.config.background_auto_refresh {
                    let _ = wallet.auto_refresh_due(margin).await;
                } else {
                    let _ = wallet.deadline_safety_due(margin).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
        (
            // Unconditional in text, unreachable in fact: an early `continue` above it restores a
            // path through the loop that skips the pass.
            "an_early_continue_skips_the_pass",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if wallet.claim().await.is_err() {
                    continue;
                }
                let _ = wallet.deadline_safety_due(margin).await;
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
    ];

    for (tag, src) in cases {
        assert!(
            deadline_pass_is_unconditional(src).is_err(),
            "the checker ACCEPTED `{tag}`, which leaves a path through the poll loop with no \
             deadline pass — it would not have caught the defect it is written for"
        );
    }
}

/// ...and the economics must not swing the other way. Routine background re-anchoring silently
/// shrinks a balance; B4 pays that fee on demand inside `transfer` instead. A guard that only ever
/// pushes toward "run more" would be satisfied by making both passes unconditional.
#[test]
fn guard_rejects_unconditional_routine_re_anchoring() {
    let src = r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = wallet.deadline_safety_due(margin).await;
                let _ = wallet.auto_refresh_due(margin).await;
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#;
    let why = deadline_pass_is_unconditional(src)
        .expect_err("routine re-anchoring became unconditional and the checker allowed it");
    assert!(
        why.contains("auto_refresh_due"),
        "rejected for the wrong reason:\n{why}"
    );
}

/// ...and it must not cry wolf: the corrected shapes have to pass, or the guard just pushes the
/// author back into the arm. Both hoists are legal, and so is dropping the routine pass entirely.
#[test]
fn guard_accepts_the_corrected_shapes() {
    let accepted: [(&str, &str); 3] = [
        (
            "hoisted_above_the_gate",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                let _ = wallet.deadline_safety_due(wallet.inner.config.auto_refresh_margin_blocks).await;
                if wallet.inner.config.auto_refresh && wallet.inner.config.background_auto_refresh {
                    let _ = wallet.auto_refresh_due(wallet.inner.config.auto_refresh_margin_blocks).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
        (
            "hoisted_below_the_gate",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                if wallet.inner.config.auto_refresh && wallet.inner.config.background_auto_refresh {
                    let _ = wallet.auto_refresh_due(wallet.inner.config.auto_refresh_margin_blocks).await;
                }
                let _ = wallet.deadline_safety_due(wallet.inner.config.auto_refresh_margin_blocks).await;
                if wallet.inner.config.auto_exit {
                    let _ = wallet.auto_exit_due(wallet.inner.config.auto_exit_margin_blocks).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
        (
            "routine_pass_dropped_entirely",
            r#"
    pub fn start_background(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = wallet.claim().await;
                let _ = wallet.deadline_safety_due(margin).await;
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        })
    }
"#,
        ),
    ];
    for (tag, src) in accepted {
        if let Err(why) = deadline_pass_is_unconditional(src) {
            panic!("the corrected shape `{tag}` was flagged:\n{why}");
        }
    }
}

/// THE TWO REMEDIES, IN ORDER, AND THE FALLBACK BETWEEN THEM.
#[test]
fn the_cooperative_route_falls_back_to_severing() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    let body = method_body(&code, "pub async fn deadline_safety_due(");
    let scrubbed = scrub(body);

    // ORDERING is pinned by POSITION, not by two independent `contains` checks: "cooperative first,
    // sever second" is a claim about which runs before which, and only the offsets say that.
    let coop = scrubbed
        .find("self.auto_refresh_due(margin_blocks)")
        .unwrap_or_else(|| panic!(
            "the cooperative re-anchor is no longer tried first. It is the cheap remedy and it \
             keeps the coin off-chain; severing costs the coin its off-chain life:\n\n{body}"
        ));
    let sever = scrubbed.find("self.unilateral_exit(").unwrap_or_else(|| panic!(
        "there is no unilateral fallback. Re-anchoring needs one fresh SE co-signature, and under \
         D40.1 the adversary IS the party asked to sign — a defence that can be declined by its \
         adversary is not a defence:\n\n{body}"
    ));
    assert!(
        coop < sever,
        "the sever at byte {sever} precedes the cooperative re-anchor at byte {coop}. The order is \
         the property: severing costs the coin its off-chain life and must only be reached after \
         the cheap cooperative remedy has actually been tried and failed:\n\n{body}"
    );

    // Blindness must not sever. A plain trigger over an unknown carrier destroys its allocation.
    //
    // This pins a REFUSAL, so it pins `return Err` — and it pins it INSIDE the failure arm of the
    // cooperative call, not merely somewhere in the method. `contains("BLIND") &&
    // contains("return Err(")` was satisfiable by a `println!` naming BLIND plus the unrelated
    // `return Err` that reports undefended coins at the end of the pass.
    let arm_at = scrubbed[coop..]
        .find("Err(e) =>")
        .map(|d| coop + d)
        .unwrap_or_else(|| panic!(
            "the cooperative call no longer has an `Err(e) =>` arm, so a BLIND carrier set is \
             handled by something unexamined:\n\n{body}"
        ));
    // Bounded by the `;` that ends the `match` statement — a real symbol of this statement, never a
    // byte count and never the rest of the method.
    let arm = statement(&scrubbed, arm_at);
    assert!(
        arm.contains("return Err(") && body[arm_at..arm_at + arm.len()].contains("BLIND"),
        "an unreadable carrier set no longer REFUSES the pass. It must `return Err` — a log line \
         saying BLIND while execution falls through severs blindly, and a plain trigger over a \
         token carrier destroys the allocation, which is a worse outcome than the deadline this \
         pass exists to beat:\n\n{arm}"
    );

    // The still-due set must be RE-READ, not inferred from the failure list: a coin the cooperative
    // route moved is no longer at risk under its old id. And the re-read must happen AFTER the
    // cooperative pass, or it is not a re-read at all.
    let reread = scrubbed
        .find("coin_near_final(c, tip, margin_blocks)")
        .unwrap_or_else(|| panic!(
            "the second pass no longer re-derives what is still near its deadline. Inferring it \
             from the first pass's failures would sever coins that were successfully re-anchored \
             under a new id:\n\n{body}"
        ));
    assert!(
        coop < reread && reread < sever,
        "the near-deadline set is re-derived at byte {reread}, outside the window between the \
         cooperative pass ({coop}) and the sever ({sever}). Read before the re-anchor it is stale; \
         read after the sever it decides nothing:\n\n{body}"
    );
}

/// NON-VACUITY: the predicate the whole pass rests on must still be keyed on the coin's own
/// `locktime`, which is `L_k` — the clock the prior owners hold.
#[test]
fn the_deadline_predicate_still_reads_the_flat_chains_clock() {
    let code = code_only(&read("clients/libs/rust-sdk/src/refresh.rs"));
    let at = code.find("fn coin_near_final(").expect("`coin_near_final` is gone");
    // Bounded by the next REAL symbol after this free function — the test module that follows it.
    // The old window was a fixed 240 bytes, which is the forbidden shape twice over: it can stop
    // short of the thing being checked, and if the function grows it runs on into whatever is next.
    let end = code[at..]
        .find("\n#[cfg(test)]")
        .map(|d| at + d)
        .expect("no `#[cfg(test)]` after `coin_near_final` to bound its body");
    let body = &code[at..end];
    assert!(
        body.contains("c.locktime") && body.contains("saturating_sub(tip)"),
        "`coin_near_final` no longer measures headroom as `locktime - tip`. That quantity IS the \
         deadline; anything else defends a different clock:\n\n{body}"
    );
    assert!(
        body.contains("<= margin_blocks"),
        "`coin_near_final` no longer compares that headroom against `margin_blocks`, so the pass \
         no longer has a margin — it either fires always or never:\n\n{body}"
    );
}
