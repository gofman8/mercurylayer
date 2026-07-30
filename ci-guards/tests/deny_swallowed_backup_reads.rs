//! The silent-degradation class, enforced rather than reviewed.
//!
//! Three review rounds each fixed the swallowed reads they were shown, and the next round found
//! more. The sites had nothing in common but their SHAPE: a failure turned into a default that
//! reads exactly like a calm, real answer — an empty carrier set, an empty branch-witness set, a
//! spend generation of zero, a `None` deadline. Per-site fixing does not converge, so the property
//! is checked on every test run instead.
//!
//! `get_backup_txs` is the sharpest instance and the one this guard pins: the exit branch, the
//! backup transactions, the consignment envelope, the carrier's spend generation and the rejection
//! marker all live behind it, and each of them read as EMPTY means "there is nothing here to
//! protect". It is also genuinely ambiguous — `fetch_one` reports "no such row" and "could not
//! read" through the same `Err` — which is why `unwrap_or_default()` looks reasonable at every one
//! of these call sites and is wrong at all of them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/ci-guards, which lives outside the enforced files on purpose.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .expect("repo root")
        .to_path_buf()
}

fn script() -> PathBuf {
    repo_root().join("scripts/ci/deny-swallowed-backup-reads.sh")
}

fn run(root: &Path) -> (i32, String) {
    let out = Command::new(script())
        .arg(root)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", script().display()));
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().expect("script was killed"), text)
}

/// Enforced file, relative to the repo root — mirrors `ENFORCED_FILES` in the script.
const ENFORCED: &str = "clients/libs/rust-sdk/src/tokens.rs";

#[test]
fn script_is_present_and_executable() {
    let script = script();
    assert!(script.is_file(), "missing guard script: {}", script.display());
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

/// The invariant itself.
#[test]
fn no_swallowed_backup_reads_in_enforced_files() {
    let (code, output) = run(&repo_root());
    assert_eq!(code, 0, "guard script reported swallowed reads:\n{output}");
}

fn plant(body: &str, tag: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("ci-guard-swallow-{tag}"));
    let _ = std::fs::remove_dir_all(&tmp);
    let file = tmp.join(ENFORCED);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, body).unwrap();
    tmp
}

/// A guard that cannot fail is decoration. These are the THREE defects this round actually removed
/// from `tokens.rs`, replanted verbatim — if the guard does not catch each one, it would not have
/// caught the bugs it exists to prevent from coming back.
#[test]
fn guard_catches_each_real_defect_it_was_written_for() {
    let cases = [
        (
            "branch_witness_unwrap_or_default",
            r#"
async fn accept_incoming_tokens() {
    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(
        &self.inner.cc.pool,
        &self.inner.config.wallet_name,
        &format!("branch-{statechain_id}"),
    )
    .await
    .unwrap_or_default();
}
"#,
        ),
        (
            "err_underscore_returns_clean_absence",
            r#"
async fn accept_incoming_tokens() {
    let backups = match mercuryrustlib::sqlite_manager::get_backup_txs(pool, name, sid).await {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
}
"#,
        ),
        (
            "spend_generation_defaults_to_zero",
            r#"
async fn colored_transfer() {
    let parent_backups = mercuryrustlib::sqlite_manager::get_backup_txs(pool, name, &carrier_id)
        .await
        .map(|v| v.len() as u32)
        .unwrap_or(0);
}
"#,
        ),
        (
            "carrier_enumeration_ok_discarded",
            r#"
async fn consignment_bearing_outpoints() {
    let rejected = mercuryrustlib::sqlite_manager::get_backup_txs(pool, name, &marker)
        .await
        .ok()
        .map(|v| !v.is_empty());
}
"#,
        ),
    ];

    for (tag, body) in cases {
        let tmp = plant(body, tag);
        let (code, output) = run(&tmp);
        assert_eq!(code, 1, "planted defect `{tag}` was NOT caught:\n{output}");
        assert!(
            output.contains("tokens.rs"),
            "the offending file is not reported for `{tag}`:\n{output}"
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}

/// ...and it must not cry wolf. The corrected shapes — the helper that names the distinction, and
/// an explicit two-case match — have to pass, or the guard just pushes people back to the swallow.
#[test]
fn guard_accepts_the_corrected_shapes() {
    let body = r#"
async fn corrected() {
    // A swallowed get_backup_txs(...).unwrap_or_default() named in a COMMENT is documentation.
    let raw_branch: Vec<String> = read_backup_rows(pool, name, &format!("branch-{sid}"))
        .await?
        .into_iter()
        .flatten()
        .map(|b| b.tx)
        .collect();

    let rows = match mercuryrustlib::sqlite_manager::get_backup_txs(pool, name, key).await {
        Ok(rows) => Some(rows),
        Err(e) if is_row_not_found(&e) => None,
        Err(e) => return Err(anyhow!("backup rows for '{key}' could not be read: {e}")),
    };

    let backups = mercuryrustlib::sqlite_manager::get_backup_txs(pool, name, &piece_id).await?;
}
"#;
    let tmp = plant(body, "corrected");
    let (code, output) = run(&tmp);
    assert_eq!(code, 0, "the corrected shapes were flagged:\n{output}");
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// A bad root is a usage error (exit 2), never a silent pass — the guard must not be defeated by
/// the very failure mode it is about.
#[test]
fn guard_fails_loudly_on_a_bad_root() {
    let (code, _) = run(Path::new("/nonexistent/mercurylayer"));
    assert_eq!(code, 2, "a missing root must be a usage error");

    let tmp = std::env::temp_dir().join("ci-guard-swallow-empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let (code, _) = run(&tmp);
    assert_eq!(code, 2, "a tree with none of the enforced files must be a usage error");
    std::fs::remove_dir_all(&tmp).unwrap();
}
