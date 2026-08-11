//! The SILENT-DEGRADATION class, enforced instead of reviewed.
//!
//! Three consecutive review rounds found the SAME defect shape in new places, and per-site fixing
//! did not converge: **a failure that presents as a benign empty or idle result.** An empty carrier
//! set reads as "no carriers to protect". An empty UTXO vector reads as "outpoint spent". An absent
//! deadline reads as "nothing is due". An empty branch-witness set reads as "this coin has no exit
//! branch to broadcast". Each of those is one line of Rust that turns an `Err` into a default, and
//! on this codebase — where a coin is protected by racing a deadline — the default is always the
//! answer that stands down.
//!
//! The direction matters, not the spelling, and grep has no type information, so this guard does
//! not try to judge. It pins the reviewed set: `scripts/ci/silent-degradation-allowlist.txt` names
//! every swallow site that exists today together with the argument for why it is safe, and anything
//! not on it is a failure. Writing a new `unwrap_or_default()` on a protection path is then a red
//! test naming this class, at the moment it is written, instead of a finding three rounds later.
//!
//! Two guards, one convention. `clients/libs/rust-sdk/src/wallet.rs` carries its own in-file
//! `silent_degradation_guard` module which is FILE-scoped and SUBJECT-scoped (it fires only when a
//! swallow sits next to a protection-deciding read) and is silenced by an `AUDITED-SWALLOW:` note.
//! This one is REPO-scoped and SPELLING-scoped. It honours the same `AUDITED-SWALLOW` marker, so an
//! author annotates in place once and both guards accept it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/ci-guards, which is deliberately OUTSIDE the scanned directories:
    // this crate spells out the forbidden patterns, so it would trip its own guard from within.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .expect("repo root")
        .to_path_buf()
}

fn script() -> PathBuf {
    repo_root().join("scripts/ci/deny-silent-degradation.sh")
}

fn allowlist() -> PathBuf {
    repo_root().join("scripts/ci/silent-degradation-allowlist.txt")
}

/// Run the guard against `root`. `list` overrides the allowlist (`None` = the tree's own).
fn run_with(root: &Path, list: Option<&Path>) -> (i32, String) {
    let mut cmd = Command::new(script());
    cmd.arg(root);
    if let Some(l) = list {
        cmd.env("SD_ALLOWLIST", l);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", script().display()));
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().expect("script was killed"), text)
}

fn run(root: &Path) -> (i32, String) {
    run_with(root, None)
}

#[test]
fn script_and_allowlist_are_present_and_executable() {
    let script = script();
    assert!(script.is_file(), "missing guard script: {}", script.display());
    assert!(
        allowlist().is_file(),
        "missing allowlist: {}",
        allowlist().display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = script.metadata().unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "guard script is not executable (mode {mode:o})"
        );
    }
}

/// The invariant itself: every swallow spelling under `clients/libs/`, `lib/` and `server/src/` is
/// one that has been read and argued for.
#[test]
fn no_unreviewed_swallow_sites() {
    let (code, output) = run(&repo_root());
    assert_eq!(
        code, 0,
        "un-allowlisted swallow(s) — see the guidance the script printed:\n{output}"
    );
}

/// An allowlist entry without a written reason is the failure mode this guard is meant to replace
/// (a site nobody argued about). Every data line must be preceded by at least one `#` comment.
#[test]
fn every_allowlist_entry_carries_a_reason() {
    let text = std::fs::read_to_string(allowlist()).expect("read allowlist");
    let mut unexplained = Vec::new();
    let mut entries = 0usize;
    let mut reason_lines_since_entry = 0usize;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with('#') {
            // A bare `#` is the separator the generator emits between records, not a reason.
            if line.trim() != "#" {
                reason_lines_since_entry += 1;
            }
            continue;
        }
        entries += 1;
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            fields.len(),
            4,
            "allowlist entry must be <count>\\t<path>\\t<pattern-id>\\t<line>, got: {line:?}"
        );
        assert!(
            fields[0].parse::<u32>().is_ok(),
            "allowlist entry must start with a count, got: {line:?}"
        );
        if reason_lines_since_entry == 0 {
            unexplained.push(line.to_string());
        }
        reason_lines_since_entry = 0;
    }
    assert!(
        entries > 0,
        "the allowlist parsed to zero entries — the format or the TAB separators are broken"
    );
    assert!(
        unexplained.is_empty(),
        "allowlist entries with no reason written above them ({} of {entries}):\n  {}",
        unexplained.len(),
        unexplained.join("\n  ")
    );
}

/// Every spelling the guard claims to cover must actually be caught. A guard that cannot fail is
/// decoration, and this class already survived three rounds of human review.
#[test]
fn guard_catches_every_covered_spelling() {
    // (label, the offending Rust line)
    let planted: &[(&str, &str)] = &[
        (
            "await-unwrap_or_default",
            "let carriers = self.token_carrier_outpoints().await.unwrap_or_default();",
        ),
        (
            "await-unwrap_or",
            "let n = read_branch(&pool).await.unwrap_or(0);",
        ),
        (
            "await-ok",
            "let maybe = load_deadline(&cc).await.ok().map(|d| d + 1);",
        ),
        ("ok-question", "let tip = info_config(&cc).ok()?.initlock;"),
        ("err-continue", "Err(_) => continue,"),
        ("err-return-ok", "Err(_) => return Ok(vec![]),"),
        ("err-empty-vec", "Err(_) => Vec::new(),"),
        ("err-empty-none", "Err(_) => None,"),
        ("err-empty-block", "Err(_) => {}"),
        ("is_err-classifier", "if precheck(&coin).is_err() {"),
    ];

    for dir in ["clients/libs", "lib", "server/src"] {
        for (label, offender) in planted {
            let tmp = std::env::temp_dir().join(format!(
                "ci-guard-sd-{}-{label}",
                dir.replace('/', "_")
            ));
            let _ = std::fs::remove_dir_all(&tmp);
            let nested = tmp.join(dir).join("deep");
            std::fs::create_dir_all(&nested).unwrap();

            // A clean tree passes even with an empty allowlist.
            std::fs::write(nested.join("clean.rs"), "fn main() { let x = 1; }\n").unwrap();
            let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
            assert_eq!(code, 0, "[{dir}/{label}] clean tree was flagged:\n{output}");

            // ...and the offender is caught.
            std::fs::write(
                nested.join("offender.rs"),
                format!("fn boom() {{\n    {offender}\n}}\n"),
            )
            .unwrap();
            let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
            assert_eq!(
                code, 1,
                "[{dir}] planted `{label}` was NOT caught:\n{output}"
            );
            assert!(
                output.contains("offender.rs"),
                "[{dir}/{label}] the offending file is not named:\n{output}"
            );
            assert!(
                output.contains("LESS PROTECTION"),
                "[{dir}/{label}] the failure must explain the class, not just list a line:\n{output}"
            );

            std::fs::remove_dir_all(&tmp).unwrap();
        }
    }
}

/// The multi-line method chain: `.await` on one line and the swallow several lines down. This is the
/// form a single-line grep misses, and it is the form one of the round-3 HIGHs was written in
/// (`consignment_bearing_outpoints` — the carrier enumeration every downstream carrier check
/// depends on).
#[test]
fn guard_catches_a_swallow_several_lines_below_its_await() {
    let tmp = std::env::temp_dir().join("ci-guard-sd-multiline");
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("clients/libs/rust-sdk/src");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("offender.rs"),
        "async fn carriers(pool: &Pool) -> HashSet<String> {\n\
         \x20   let rejected = get_backup_txs(pool, name, key)\n\
         \x20       .await\n\
         \x20       .map(|v| !v.is_empty())\n\
         \x20       .unwrap_or(false);\n\
         \x20   rejected\n\
         }\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(
        code, 1,
        "a swallow three lines below its `.await` was not caught:\n{output}"
    );
    assert!(
        output.contains("chain-after-await"),
        "expected the chain-aware pattern id:\n{output}"
    );

    // The same trailing `.unwrap_or(false)` on a plain `Option` chain (no `.await`, no `map_err`)
    // must NOT be flagged, or the guard is too noisy to survive its own allowlist.
    std::fs::write(
        dir.join("offender.rs"),
        "fn last_n(v: &[u32]) -> u32 {\n\
         \x20   v.iter()\n\
         \x20       .last()\n\
         \x20       .copied()\n\
         \x20       .unwrap_or(0)\n\
         }\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(code, 0, "a plain Option chain was flagged:\n{output}");

    std::fs::remove_dir_all(&tmp).unwrap();
}

/// The assert-continuation exemption must be scoped to the assertion's OWN argument list.
///
/// The exemption exists because rustfmt wraps `assert!(foo(\n    ..).is_err(), "..")` so the
/// condition lands on a later line, where the line-scoped `*assert*` filter cannot see it. It was
/// widened from "the opener line ends in `(`" to a paren-balance test, which is strictly more
/// permissive — so this pins the boundary: once the assertion's parens CLOSE, a swallow on the next
/// line is a swallow again, however close it sits to an assert.
#[test]
fn the_assert_exemption_stops_at_the_assertions_closing_paren() {
    let tmp = std::env::temp_dir().join("ci-guard-sd-assert-scope");
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("lib");
    std::fs::create_dir_all(&dir).unwrap();

    // INSIDE the argument list, wrapped the way rustfmt actually wraps it -> exempt.
    std::fs::write(
        dir.join("offender.rs"),
        "fn t() {\n\
         \x20   assert!(super::verify_sig_count_attestation(\n\
         \x20       \"coin-a\", 4, &nonce, &sig, &xonly, &compressed).is_err(),\n\
         \x20       \"a stale count must not verify\");\n\
         }\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(code, 0, "a wrapped assert condition was flagged as a swallow:\n{output}");

    // The assertion has CLOSED; the next line is ordinary code -> still a hit.
    std::fs::write(
        dir.join("offender.rs"),
        "async fn t(pool: &Pool) -> HashSet<String> {\n\
         \x20   assert!(precondition_holds(pool),\n\
         \x20       \"the caller must have checked this\");\n\
         \x20   read_carriers(pool).await.unwrap_or_default()\n\
         }\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(code, 1, "a real swallow below a closed assert was exempted:\n{output}");
    assert!(output.contains("await-unwrap_or_default"), "expected the swallow's id:\n{output}");

    std::fs::remove_dir_all(&tmp).unwrap();
}

/// A *second*, identical swallow added to a file that already has one allowlisted must fail: the
/// allowlist pins a COUNT, not merely a spelling.
#[test]
fn guard_catches_a_second_copy_of_an_allowlisted_line() {
    let tmp = std::env::temp_dir().join("ci-guard-sd-count");
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("lib/src");
    std::fs::create_dir_all(&dir).unwrap();
    let list = tmp.join("list.txt");
    std::fs::write(&list, "# reviewed: fails toward more work\n1\tlib/src/x.rs\terr-continue\tErr(_) => continue,\n").unwrap();

    std::fs::write(dir.join("x.rs"), "fn a() {\n    Err(_) => continue,\n}\n").unwrap();
    let (code, output) = run_with(&tmp, Some(&list));
    assert_eq!(code, 0, "the allowlisted single occurrence was flagged:\n{output}");

    std::fs::write(
        dir.join("x.rs"),
        "fn a() {\n    Err(_) => continue,\n}\nfn b() {\n    Err(_) => continue,\n}\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(&list));
    assert_eq!(code, 1, "a SECOND copy of an allowlisted swallow was not caught:\n{output}");
    assert!(output.contains("GREW"), "expected a GREW report:\n{output}");

    std::fs::remove_dir_all(&tmp).unwrap();
}

/// A site that was FIXED must not turn the guard red — otherwise the guard punishes the work it
/// exists to cause. It is reported as informational instead.
#[test]
fn a_removed_swallow_is_informational_not_a_failure() {
    let tmp = std::env::temp_dir().join("ci-guard-sd-stale");
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("lib/src");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("x.rs"), "fn a() { let y = 1; }\n").unwrap();
    let list = tmp.join("list.txt");
    std::fs::write(&list, "# reviewed\n1\tlib/src/x.rs\terr-continue\tErr(_) => continue,\n").unwrap();

    let (code, output) = run_with(&tmp, Some(&list));
    assert_eq!(code, 0, "a fixed site must not fail the guard:\n{output}");
    assert!(output.contains("GONE"), "expected the prune hint:\n{output}");

    std::fs::remove_dir_all(&tmp).unwrap();
}

/// The `AUDITED-SWALLOW` escape hatch — the same marker the in-file guard in
/// `clients/libs/rust-sdk/src/wallet.rs` uses, so there is one convention across the repo.
#[test]
fn an_audited_swallow_marker_silences_the_guard() {
    let tmp = std::env::temp_dir().join("ci-guard-sd-marker");
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("clients/libs/rust-sdk/src");
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("x.rs"),
        "async fn f() {\n    let c = carriers().await.unwrap_or_default();\n}\n",
    )
    .unwrap();
    let (code, _) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(code, 1, "the un-annotated swallow should be caught");

    std::fs::write(
        dir.join("x.rs"),
        "async fn f() {\n    // AUDITED-SWALLOW: fails toward MORE work — an empty set re-attempts every coin.\n    let c = carriers().await.unwrap_or_default();\n}\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(code, 0, "an AUDITED-SWALLOW annotation must silence it:\n{output}");

    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Build artifacts, vendored dependencies and non-Rust files must not trip the guard.
#[test]
fn guard_ignores_build_artifacts_and_non_rust_files() {
    let tmp = std::env::temp_dir().join("ci-guard-sd-artifacts");
    let _ = std::fs::remove_dir_all(&tmp);
    for excluded in ["target", "node_modules", "dist", "build"] {
        let dir = tmp.join("clients/libs").join(excluded).join("deep");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("vendored.rs"),
            "async fn f() { let x = g().await.unwrap_or_default(); }\n",
        )
        .unwrap();
    }
    let dir = tmp.join("lib/src");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("notes.md"),
        "we used to write `x.await.unwrap_or_default()` here\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(code, 0, "artifacts or non-Rust files tripped the guard:\n{output}");
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Prose about the class, and test assertions, are not the class.
#[test]
fn guard_ignores_comments_and_assertions() {
    let tmp = std::env::temp_dir().join("ci-guard-sd-prose");
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("server/src/endpoints");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("x.rs"),
        "// the old code said `Err(_) => continue,` and that was the bug\n\
         /// a doc comment mentioning .ok()? must not fire\n\
         #[test]\n\
         fn t() {\n\
         \x20   assert!(split(959).is_err(), \"refused\");\n\
         }\n",
    )
    .unwrap();
    let (code, output) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(code, 0, "prose or assertions tripped the guard:\n{output}");
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// A bad root is a usage error (exit 2), never a silent pass — the guard must not be able to
/// degrade silently itself.
#[test]
fn guard_fails_loudly_on_a_bad_root() {
    let (code, _) = run(Path::new("/nonexistent/mercurylayer"));
    assert_eq!(code, 2, "a missing root must be a usage error");

    let tmp = std::env::temp_dir().join("ci-guard-sd-empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let (code, _) = run_with(&tmp, Some(Path::new("/dev/null")));
    assert_eq!(
        code, 2,
        "a tree with none of the scanned directories must be a usage error"
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}
