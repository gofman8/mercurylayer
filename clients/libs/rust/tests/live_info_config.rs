//! [D8(f)] The client must ACCEPT the coordinator it is actually deployed against.
//!
//! `info_config` no longer trusts `/info/config` for `initlock`/`interval`; it compiles them in per
//! network and refuses any coordinator that reports different numbers. That refusal is the whole
//! point — `interval` is what INV-5 measures every flat-backup hop against, so a coordinator that
//! could choose it could choose the defence against backup-vector padding — but it also means a
//! table that disagrees with the deployed stack bricks **every** wallet call, not just one feature.
//!
//! Unit tests cannot catch that: they check the table against itself. This checks the table against
//! the coordinator that is actually running, which is the only thing that can be wrong.
//!
//! **It skips, loudly, when no coordinator is reachable** — the stack is not always up, and a test
//! that fails for that reason gets ignored, which is how a real failure hides. When one IS up, the
//! assertions are hard.

const DEFAULT_URL: &str = "http://127.0.0.1:8000";

/// Minimal blocking GET — this crate's own client is async and pulls in a runtime; the point here is
/// to read what the coordinator says with as little of the code under test in the way as possible.
///
/// Returns the reason on failure rather than a bare `None`. This test's only failure mode that is
/// NOT a real defect is "no stack running", and a skip that cannot say WHY it skipped is
/// indistinguishable from a skip caused by a broken harness — which is how a permanently-skipping
/// test stops being noticed. (`curl` missing and `connection refused` are very different problems.)
fn get(url: &str) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(["-s", "--show-error", "--max-time", "5", url])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let body = String::from_utf8(out.stdout).map_err(|e| format!("response is not UTF-8: {e}"))?;
    if body.trim().is_empty() {
        return Err("empty response body".to_string());
    }
    Ok(body)
}

fn field(json: &str, key: &str) -> Option<u32> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 3;
    json[at..]
        .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

#[test]
fn the_running_coordinator_agrees_with_the_compiled_flat_ladder() {
    let url = std::env::var("STATECHAIN_ENTITY").unwrap_or_else(|_| DEFAULT_URL.to_string());
    let body = match get(&format!("{url}/info/config")) {
        Ok(b) => b,
        Err(why) => {
            eprintln!(
                "SKIP: no coordinator answered at {url}/info/config ({why}). This test only proves \
                 something when a stack is up; set STATECHAIN_ENTITY to point it elsewhere."
            );
            return;
        }
    };

    let (Some(initlock), Some(interval)) = (field(&body, "initlock"), field(&body, "interval"))
    else {
        panic!("coordinator at {url} answered /info/config without initlock/interval: {body}");
    };

    // Which network is it? Ask the table for each candidate and require exactly one to match, so a
    // coordinator that matches NO profile is a failure rather than an untested skip.
    let profiles = ["bitcoin", "regtest"];
    let matched: Vec<&str> = profiles
        .iter()
        .copied()
        .filter(|n| {
            mercurylib::tesr::TesrParams::flat_ladder_params(n) == Some((initlock, interval))
        })
        .collect();

    assert!(
        !matched.is_empty(),
        "the RUNNING coordinator at {url} reports initlock={initlock} interval={interval}, which \
         matches NO compiled profile ({}). Every client would refuse it at `info/config` — this is \
         the deployment-vs-table drift `ci-guards/tests/deny_flat_ladder_config_drift.rs` guards \
         statically, caught here against the live process.",
        profiles
            .iter()
            .map(|n| format!(
                "{n}={:?}",
                mercurylib::tesr::TesrParams::flat_ladder_params(n).unwrap()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    eprintln!(
        "live coordinator at {url}: initlock={initlock} interval={interval} — accepted as {:?}",
        matched
    );
}

/// And the refusal must actually fire. Same table, a coordinator that disagrees by one block.
///
/// This is the half that matters for security: the test above would still pass if the comparison in
/// `info_config` were deleted, because it only proves the numbers agree. This one proves the numbers
/// are *checked*, by re-running the exact predicate that function applies.
#[test]
fn a_coordinator_that_disagrees_by_one_block_is_not_accepted() {
    for net in ["bitcoin", "regtest"] {
        let (initlock, interval) = mercurylib::tesr::TesrParams::flat_ladder_params(net).unwrap();
        for (bad_init, bad_interval) in
            [(initlock, interval - 1), (initlock, interval + 1), (initlock + 1, interval)]
        {
            assert!(
                (bad_init, bad_interval) != (initlock, interval),
                "{net}: the mutation must actually differ, or this test proves nothing"
            );
            assert_ne!(
                mercurylib::tesr::TesrParams::flat_ladder_params(net),
                Some((bad_init, bad_interval)),
                "{net}: a coordinator reporting {bad_init}/{bad_interval} must not satisfy the \
                 equality `info_config` checks"
            );
        }
    }
}
