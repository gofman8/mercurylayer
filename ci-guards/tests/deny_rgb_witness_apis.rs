//! The `update_witnesses` / `upsert_witness` deny-list, enforced rather than conventional.
//!
//! E7 (the CTES-R gate report (retired)) reproduced, against the live regtest stack, that a single
//! `update_witnesses` call with the plain blockchain resolver permanently and silently archived
//! every rung of a deliberately un-broadcast colored ladder: `succeeded=2`, `failed={}`, no error,
//! and `get_asset_balance` unchanged at the full settled amount. The rgb-lib fork now guards the
//! resolver and ships a repair primitive, but mercury still has no business calling either API, so
//! the "never expose it" property is checked here on every test run.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/ci-guards. This crate lives OUTSIDE the scanned directories on
    // purpose: it names the forbidden APIs, so it would trip its own guard from within `clients/`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .expect("repo root")
        .to_path_buf()
}

fn script() -> PathBuf {
    repo_root().join("scripts/ci/deny-rgb-witness-apis.sh")
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

/// The invariant itself: no mercury code calls the forbidden rgb-lib witness APIs.
#[test]
fn no_rgb_witness_api_call_sites() {
    let (code, output) = run(&repo_root());
    assert_eq!(code, 0, "guard script reported call sites:\n{output}");
}

/// The guard must actually be able to fail, otherwise it is decoration. Plant a call site in a
/// throwaway tree and check the script flags it, in each scanned directory and in each of the two
/// forbidden APIs.
#[test]
fn guard_catches_a_planted_call_site() {
    for dir in ["clients", "lib"] {
        for (i, forbidden) in ["update_witnesses", "upsert_witness"].iter().enumerate() {
            let tmp = std::env::temp_dir().join(format!("ci-guard-deny-{dir}-{i}"));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(tmp.join(dir).join("nested")).unwrap();
            // a clean tree passes
            std::fs::write(tmp.join(dir).join("clean.rs"), "fn main() {}\n").unwrap();
            let (code, output) = run(&tmp);
            assert_eq!(code, 0, "clean tree was flagged:\n{output}");

            // ...and one call site anywhere under it fails
            std::fs::write(
                tmp.join(dir).join("nested").join("offender.rs"),
                format!("fn boom() {{ wallet.{forbidden}(0, vec![]).unwrap(); }}\n"),
            )
            .unwrap();
            let (code, output) = run(&tmp);
            assert_eq!(code, 1, "planted `{forbidden}` was not caught:\n{output}");
            assert!(
                output.contains("offender.rs"),
                "the offending file is not reported:\n{output}"
            );

            std::fs::remove_dir_all(&tmp).unwrap();
        }
    }
}

/// Build artifacts and vendored dependencies must not trip the guard: rgb-lib's own sources define
/// these APIs and are routinely present under `target/`.
#[test]
fn guard_ignores_build_artifacts_and_vendored_code() {
    let tmp = std::env::temp_dir().join("ci-guard-deny-artifacts");
    let _ = std::fs::remove_dir_all(&tmp);
    for excluded in ["target", "node_modules", "dist", "build"] {
        let dir = tmp.join("clients").join(excluded).join("deep");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("vendored.rs"),
            "pub fn update_witnesses() {}\npub fn upsert_witness() {}\n",
        )
        .unwrap();
    }
    let (code, output) = run(&tmp);
    assert_eq!(code, 0, "build artifacts tripped the guard:\n{output}");
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// A bad root is a usage error (exit 2), never a silent pass.
#[test]
fn guard_fails_loudly_on_a_bad_root() {
    let (code, _) = run(Path::new("/nonexistent/mercurylayer"));
    assert_eq!(code, 2, "a missing root must be a usage error");

    let tmp = std::env::temp_dir().join("ci-guard-deny-empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let (code, _) = run(&tmp);
    assert_eq!(code, 2, "a tree with neither clients/ nor lib/ must be a usage error");
    std::fs::remove_dir_all(&tmp).unwrap();
}
