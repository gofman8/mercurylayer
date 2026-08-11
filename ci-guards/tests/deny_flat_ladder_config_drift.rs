//! Every deployment config must agree with `TesrParams::flat_ladder_params`. [D8(f)]
//!
//! `interval` (`lh_decrement`) is the yardstick INV-5 measures every flat-backup hop against — the
//! check that stops a sender padding the backup vector with duplicates to inflate `flat_backups` and
//! absorb a hidden co-signed state. Clients therefore no longer take it from `/info/config`; they
//! compile it in and refuse any coordinator that disagrees.
//!
//! That refusal turns a config typo into a **fleet-wide outage** rather than a quiet protocol
//! weakening, which is the right trade — but only if the configs are actually kept in step. When this
//! guard was written, three of the four config sources in the repo disagreed with the table and with
//! each other:
//!
//! | source                     | was      | table |
//! |----------------------------|----------|-------|
//! | `docker-compose-main.yml`  | 50000/6  | 10000/100 |
//! | `docker-compose-test.yml`  | 1100/1   | 1000/10   |
//! | `server/Settings.toml`     | 1000/10  | 10000/100 (testnet runs mainnet, D25) |
//! | live regtest container     | 1000/10  | 1000/10 ✓ |
//!
//! Only the running regtest stack happened to match, which is why the E2Es never caught it. The
//! coordinator now also refuses to BOOT on a mismatch (`server/src/server_config.rs`), so this guard
//! is the cheap early warning: it fails in CI instead of at deploy.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(1).expect("repo root").to_path_buf()
}

/// The table, transcribed ONCE here on purpose.
///
/// `ci-guards` deliberately does not depend on `mercurylib` (it is a leaf crate that scans the tree),
/// so this is a copy — but `flat_ladder_params_tests::every_profile_divides_evenly_into_100_hops` and
/// `known_networks_resolve_and_are_case_insensitive` in `lib/src/tesr.rs` pin the real table to these
/// same numbers. If someone changes the table without changing this, that test and this guard
/// disagree and one of them fails.
fn expected(network: &str) -> Option<(u32, u32)> {
    match network {
        "bitcoin" | "mainnet" | "testnet" | "testnet3" | "testnet4" | "signet" => Some((10_000, 100)),
        "regtest" => Some((1_000, 10)),
        _ => None,
    }
}

/// Pull `key: value` / `key = value` out of a config file, ignoring comments and quotes.
fn scalar(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some(rest) = line.strip_prefix(key) else { continue };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('=')) else { continue };
        let v = rest.trim().trim_matches('"').trim_matches('\'').trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

struct Source {
    file: &'static str,
    network_key: &'static str,
    initlock_key: &'static str,
    interval_key: &'static str,
}

const SOURCES: &[Source] = &[
    Source {
        file: "docker-compose-main.yml",
        network_key: "BITCOIN_NETWORK",
        initlock_key: "LOCKHEIGHT_INIT",
        interval_key: "LH_DECREMENT",
    },
    Source {
        file: "docker-compose-test.yml",
        network_key: "BITCOIN_NETWORK",
        initlock_key: "LOCKHEIGHT_INIT",
        interval_key: "LH_DECREMENT",
    },
    Source {
        file: "docker-compose-lockbox.yml",
        network_key: "BITCOIN_NETWORK",
        initlock_key: "LOCKHEIGHT_INIT",
        interval_key: "LH_DECREMENT",
    },
    Source {
        file: "server/Settings.toml",
        network_key: "network",
        initlock_key: "lockheight_init",
        interval_key: "lh_decrement",
    },
];

#[test]
fn every_deployment_config_matches_the_flat_ladder_table() {
    let root = repo_root();
    let mut checked = 0;
    let mut problems = Vec::new();

    for src in SOURCES {
        let path = root.join(src.file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            problems.push(format!("{}: unreadable — this guard cannot vouch for it", src.file));
            continue;
        };

        // A config that sets neither value inherits from elsewhere; nothing to check.
        let (Some(initlock), Some(interval)) =
            (scalar(&text, src.initlock_key), scalar(&text, src.interval_key))
        else {
            continue;
        };

        let Some(network) = scalar(&text, src.network_key) else {
            problems.push(format!(
                "{}: sets {}/{} but no {} — the ladder it configures cannot be checked against any \
                 network's table",
                src.file, src.initlock_key, src.interval_key, src.network_key
            ));
            continue;
        };
        let network = network.to_ascii_lowercase();

        let (Ok(initlock), Ok(interval)) = (initlock.parse::<u32>(), interval.parse::<u32>()) else {
            problems.push(format!("{}: {initlock}/{interval} are not both integers", src.file));
            continue;
        };

        match expected(&network) {
            None => problems.push(format!(
                "{}: network {network:?} has no flat-ladder entry, so clients will refuse this \
                 coordinator outright",
                src.file
            )),
            Some((want_init, want_interval)) => {
                checked += 1;
                if (initlock, interval) != (want_init, want_interval) {
                    problems.push(format!(
                        "{}: configures {network} at {initlock}/{interval}, but the flat-ladder \
                         table says {want_init}/{want_interval}. `interval` is what INV-5 measures \
                         every backup hop against, so this coordinator would be refused by every \
                         client (and would refuse to boot).",
                        src.file
                    ));
                }
            }
        }
    }

    assert!(
        checked >= 3,
        "only {checked} config(s) actually exercised this guard — the parser or the file list has \
         drifted, and a guard that silently checks nothing is worse than no guard"
    );
    assert!(problems.is_empty(), "flat-ladder config drift:\n  - {}", problems.join("\n  - "));
}

/// The parser must not "pass" by finding nothing. If `scalar` silently returned `None` for real
/// lines, the test above would skip every source and still be green.
#[test]
fn scalar_parses_both_config_dialects() {
    let yaml = "    environment:\n      BITCOIN_NETWORK: regtest\n      LOCKHEIGHT_INIT: 1000\n";
    assert_eq!(scalar(yaml, "BITCOIN_NETWORK").as_deref(), Some("regtest"));
    assert_eq!(scalar(yaml, "LOCKHEIGHT_INIT").as_deref(), Some("1000"));

    let toml = "network = \"testnet\"\nlockheight_init = 10000 # a comment\n";
    assert_eq!(scalar(toml, "network").as_deref(), Some("testnet"));
    assert_eq!(scalar(toml, "lockheight_init").as_deref(), Some("10000"));

    assert_eq!(scalar("# LOCKHEIGHT_INIT: 5\n", "LOCKHEIGHT_INIT"), None, "commented-out lines");
    assert_eq!(scalar("nothing here\n", "LOCKHEIGHT_INIT"), None);
}

/// The drift this guard was written for must actually trip it.
#[test]
fn the_historical_drift_would_have_been_caught() {
    assert_ne!(expected("mainnet"), Some((50_000, 6)), "docker-compose-main.yml as it was");
    assert_ne!(expected("regtest"), Some((1_100, 1)), "docker-compose-test.yml as it was");
    assert_ne!(expected("testnet"), Some((1_000, 10)), "server/Settings.toml as it was");
    // ...and the one that was already correct must still pass.
    assert_eq!(expected("regtest"), Some((1_000, 10)), "the live regtest stack");
}
