//! Self-hostable, **keyless** watchtower support.
//!
//! An off-chain coin's only defence is broadcasting its pre-signed exit material before an
//! ancestor's stale backup matures (see `UtexoWallet::auto_exit_due`, the in-process watchtower).
//! Everything a watchtower must broadcast is **already fully signed** — the exit branch and the
//! backup transactions need no keys to use and pay only to the owner. So watching can be delegated
//! to any machine or third party WITHOUT trusting it with custody:
//!
//! - [`UtexoWallet::export_watch_bundle`] exports a [`WatchBundle`]: for every off-chain coin, its
//!   exit branch (locktime-free, root-first), its clawback deadline, and — for plain coins only —
//!   its latest backup tx. **No private material**: no mnemonic, no key shares, no RGB seed. The
//!   worst a malicious/buggy watchtower can do with it is broadcast EARLY, which settles the
//!   owner's coin on-chain to the owner (safe; costs only the off-chain-ness), or not act at all
//!   (the same risk as running no watchtower).
//! - [`watch_pass`] runs one watch iteration from a bundle + an electrum connection alone: no
//!   wallet database, no SE, no keys. Run it on a cron anywhere; run SEVERAL independently —
//!   broadcasts are idempotent (an already-known/mined tx is success), and since every watchtower
//!   broadcasts the SAME pre-signed transactions they can never conflict with each other. It
//!   reports a typed [`WatchState`], so a tower that could not READ the chain is distinguishable
//!   from one that read it and found nothing due.
//!
//! Token-carrier coins are exported WITHOUT their backup tx: a carrier must only ever be
//! MATERIALIZED (branch-only) — an RGB-unaware backup sweep would destroy the allocation — so the
//! bundle structurally prevents a watchtower from doing the destructive thing.
//!
//! Re-export the bundle after any transfer/claim/split (new coins = new exit material), like the
//! recovery bundle.

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercurylib::wallet::CoinStatus;

use crate::wallet::UtexoWallet;

/// The **shared** watch-pass vocabulary, defined once in `mercuryrustlib::tesr` and used by both
/// towers: the laddered (TES-R) pass and the deadline pass below. Re-exported here so a caller that
/// only uses the keyless bundle API never has to reach into the lower crate.
///
/// Its reason for existing (external review F4): `Idle` ("I looked, nothing to do") and `Blind`
/// ("I could not look") must not both be an empty `Vec<String>`.
pub use mercuryrustlib::tesr::WatchState;

/// One watched coin: everything needed to protect it, nothing needed to steal it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WatchEntry {
    pub statechain_id: String,
    /// True if the coin carries an RGB allocation: protect by MATERIALIZING (branch only);
    /// `backup_tx` is deliberately absent so no watchtower can sweep (destroy) the tokens.
    pub token_carrier: bool,
    /// Deposit-anchored exit-race deadline (`H_deposit + initlock`): the height from which an
    /// ancestor's stale backup could be broadcast. The branch must be on-chain before it.
    pub deadline_block: u32,
    /// The pre-signed exit branch, root-first, fully signed, locktime-free (raw tx hex).
    pub branch_txs: Vec<String>,
    /// The coin's latest pre-signed backup tx (raw hex) — plain coins only. Completes the sats
    /// exit once its locktime matures.
    pub backup_tx: Option<String>,
    /// `backup_tx`'s nLockTime (broadcastable from this height).
    pub backup_locktime: Option<u32>,
}

/// A keyless watchtower bundle: watch entries for every off-chain coin of a wallet.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WatchBundle {
    pub version: u32,
    /// Informational only (labels alerts); grants nothing.
    pub wallet_name: String,
    pub entries: Vec<WatchEntry>,
}

impl UtexoWallet {
    /// Export the [`WatchBundle`] for this wallet's CONFIRMED off-chain coins (flat coins have no
    /// exit branch and no ancestor race, so there is nothing to watch for them). The bundle is
    /// serialized JSON, safe to hand to an untrusted watchtower: it contains only fully-signed
    /// transactions and public metadata. Fails CLOSED for a token wallet whose carriers cannot be
    /// enumerated (a carrier mis-exported as plain would hand the watchtower a token-destroying
    /// backup).
    pub async fn export_watch_bundle(&self) -> Result<String> {
        // One consistent snapshot (same rationale as export_recovery_bundle, audit [27]).
        let _guard = self.inner.wallet_lock.lock().await;
        let record = self.record().await?;
        let carriers = if self.inner.config.rgb_data_dir.is_some()
            && self.inner.config.rgb_proxy_url.is_some()
        {
            self.unspendable_as_btc_outpoints().await?
        } else {
            std::collections::HashSet::new()
        };

        let mut entries = Vec::new();
        for coin in record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        {
            let Some(id) = coin.statechain_id.clone() else { continue };
            // F4: propagate a storage failure. `unwrap_or_default()` here turned an unreadable
            // wallet DB into "no exit branch", i.e. into the flat-coin case — the coin was then
            // silently DROPPED from the bundle and the watchtower it was handed to never watched
            // it, while the export reported success. A bundle that is quietly missing entries is
            // worse than no bundle at all, so this fails CLOSED.
            //
            // [class] ...and the fix must not swing the other way either: `get_backup_txs` is a
            // `fetch_one`, so it also returns `Err` for a row that is simply ABSENT — the ordinary
            // flat coin. Bare `?` on it would make this export fail for every plain deposit.
            // `read_exit_branch` is the read that separates the two: `Ok(vec![])` is a verified
            // absence, `Err` is a real fault.
            let branch = self.read_exit_branch(&id).await?;
            if branch.is_empty() {
                continue; // flat coin: on-chain funding, no ancestor can race it
            }
            let est = self.estimate_exit_cost(&id).await?;
            // [C1] `branch` is non-empty here (checked above), so this coin HAS a deadline. A
            // missing `exit_deadline_block` at this point is therefore never "flat coin, nothing
            // can race it" — it is "I could not compute it", and silently `continue`ing on it would
            // drop the entry from the exported bundle exactly like the `unwrap_or_default()` the
            // comment above describes. Same fail-closed rule.
            if let Some(reason) = est.exit_deadline_blind.as_deref() {
                return Err(anyhow!(
                    "refusing to export a watch bundle that silently omits {id}: {reason}"
                ));
            }
            let Some(deadline) = est.exit_deadline_block else { continue };
            let carrier = crate::wallet::is_token_carrier(coin, &carriers);
            let (backup_tx, backup_locktime) = if carrier {
                (None, None) // structurally deny the token-destroying sweep
            } else {
                let backups = mercuryrustlib::sqlite_manager::get_backup_txs(
                    &self.inner.cc.pool,
                    &self.inner.config.wallet_name,
                    &id,
                )
                .await?;
                let latest = backups
                    .iter()
                    .max_by_key(|b| b.tx_n)
                    .ok_or_else(|| anyhow!("no backup tx stored for {id}"))?;
                (Some(latest.tx.clone()), Some(mercurylib::utils::get_blockheight(latest)?))
            };
            entries.push(WatchEntry {
                statechain_id: id,
                token_carrier: carrier,
                deadline_block: deadline,
                branch_txs: branch.iter().map(|b| b.tx.clone()).collect(),
                backup_tx,
                backup_locktime,
            });
        }

        Ok(serde_json::to_string_pretty(&WatchBundle {
            version: 1,
            wallet_name: self.inner.config.wallet_name.clone(),
            entries,
        })?)
    }
}

/// One keyless watch iteration over a bundle: for every entry within `margin_blocks` of its
/// deadline, broadcast the exit branch (and, for a plain coin whose backup locktime has matured,
/// the backup too). Needs ONLY an electrum connection — no wallet, no database, no SE, no keys.
///
/// Returns a typed [`WatchState`] (external review F4):
///
/// - [`WatchState::Idle`] — the tip was read and **every** entry is comfortably ahead of its
///   deadline. A positive observation: nothing needed doing.
/// - [`WatchState::Acted`] — `ids` are the statechain_ids driven this pass, `failures` the entries
///   that could not be driven. Idempotent: a tx already in the mempool or mined counts as success,
///   so running this from several independent watchtowers (or repeatedly) is safe — they all
///   broadcast the same pre-signed transactions. A genuinely rejected broadcast (e.g. the root
///   already spent by a competing tx — the exit being RACED) fails that entry's remaining txs and
///   lands in `failures`; other entries still proceed.
/// - [`WatchState::Blind`] — **the chain tip could not be read**, so the pass could not evaluate a
///   single deadline. This used to be an `Err` that a cron wrapper could log and forget next to an
///   empty-but-successful pass; it is now the same explicit state the laddered tower reports, and
///   it is NOT idle.
pub fn watch_pass(
    bundle: &WatchBundle,
    electrum: &electrum_client::Client,
    margin_blocks: u32,
) -> WatchState {
    let tip = match electrum.block_headers_subscribe_raw() {
        Ok(h) => h.height as u32,
        Err(err) => {
            return WatchState::Blind {
                reason: format!(
                    "chain backend unreadable: no tip, so no deadline could be evaluated and \
                     nothing was watched: {err}"
                ),
            }
        }
    };
    let mut ids = Vec::new();
    let mut failures = Vec::new();
    for e in &bundle.entries {
        if tip + margin_blocks < e.deadline_block {
            continue; // comfortably ahead of the deadline
        }
        let mut txs: Vec<&String> = e.branch_txs.iter().collect();
        if let (Some(bk), Some(lock)) = (&e.backup_tx, e.backup_locktime) {
            if tip >= lock {
                txs.push(bk);
            }
        }
        let mut ok = true;
        for tx_hex in txs {
            let raw = match hex::decode(tx_hex) {
                Ok(r) => r,
                Err(err) => {
                    failures.push(format!("{}: bad tx hex: {err}", e.statechain_id));
                    ok = false;
                    break;
                }
            };
            match electrum.transaction_broadcast_raw(&raw) {
                Ok(_) => {}
                Err(err) => {
                    let msg = err.to_string();
                    if !tolerable_rebroadcast(&msg) {
                        failures.push(format!("{}: broadcast failed: {msg}", e.statechain_id));
                        ok = false;
                        break;
                    }
                }
            }
        }
        if ok {
            ids.push(e.statechain_id.clone());
        }
    }
    if ids.is_empty() && failures.is_empty() {
        // The tip WAS read and no entry was inside its margin — genuinely nothing to do.
        WatchState::Idle
    } else {
        WatchState::Acted { ids, failures }
    }
}

/// True iff a broadcast error means the tx is ALREADY in the mempool or mined — the idempotent
/// re-broadcast case every additional watchtower hits, safe to treat as success. Real rejections
/// (double-spend, conflict, missing inputs) return false and must surface.
fn tolerable_rebroadcast(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    ["already in block chain", "already in utxo set", "txn-already-known", "already in mempool", "already have transaction"]
        .iter()
        .any(|needle| m.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The bundle round-trips and, for a carrier entry, structurally contains no backup tx — the
    // token-destroying sweep is impossible for ANY watchtower holding only the bundle.
    #[test]
    fn bundle_roundtrip_and_carrier_has_no_backup() {
        let b = WatchBundle {
            version: 1,
            wallet_name: "w".into(),
            entries: vec![
                WatchEntry {
                    statechain_id: "carrier".into(),
                    token_carrier: true,
                    deadline_block: 1000,
                    branch_txs: vec!["aa".into()],
                    backup_tx: None,
                    backup_locktime: None,
                },
                WatchEntry {
                    statechain_id: "plain".into(),
                    token_carrier: false,
                    deadline_block: 1000,
                    branch_txs: vec!["bb".into()],
                    backup_tx: Some("cc".into()),
                    backup_locktime: Some(990),
                },
            ],
        };
        let json = serde_json::to_string(&b).unwrap();
        let back: WatchBundle = serde_json::from_str(&json).unwrap();
        assert!(back.entries[0].token_carrier && back.entries[0].backup_tx.is_none());
        assert_eq!(back.entries[1].backup_locktime, Some(990));
        // No key-material fields exist on the types at all.
        assert!(!json.contains("mnemonic") && !json.contains("seckey") && !json.contains("private"));
    }

    /// An electrum backend that is REACHABLE but unusable: completes the TCP handshake, then hangs
    /// up on every request. `Client::new` performs no handshake, so the client builds and each RPC
    /// fails at the transport.
    fn dead_electrum() -> electrum_client::Client {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        electrum_client::Client::new(&format!("tcp://127.0.0.1:{port}")).expect("connect")
    }

    /// A minimal electrum server that answers `blockchain.headers.subscribe` with `tip` and nothing
    /// else — enough for the deadline tower to evaluate a bundle whose entries are all comfortably
    /// ahead, which is the `Idle` positive control.
    fn electrum_at_tip(tip: u32) -> electrum_client::Client {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let peer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(peer);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    // Echo back the request's own id with a fixed tip.
                    let id = line
                        .split("\"id\":")
                        .nth(1)
                        .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    let resp = format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"height\":{tip},\"hex\":\"00\"}}}}\n"
                    );
                    if stream.write_all(resp.as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    line.clear();
                }
            }
        });
        electrum_client::Client::new(&format!("tcp://127.0.0.1:{port}")).expect("connect")
    }

    fn bundle_due_at(deadline_block: u32) -> WatchBundle {
        WatchBundle {
            version: 1,
            wallet_name: "w".into(),
            entries: vec![WatchEntry {
                statechain_id: "plain".into(),
                token_carrier: false,
                deadline_block,
                branch_txs: vec!["aa".into()],
                backup_tx: None,
                backup_locktime: None,
            }],
        }
    }

    /// **F4.** A tower that cannot read the chain tip evaluated NO deadline and watched NOTHING. It
    /// must say so. Before this it returned an `Err` that sat next to an equally empty successful
    /// pass — and the laddered tower next door returned a bare `vec![]`, so neither tower could tell
    /// a caller which of the two had happened.
    #[test]
    fn a_blind_tower_reports_blind_not_idle() {
        let state = watch_pass(&bundle_due_at(100), &dead_electrum(), 6);
        assert!(state.is_blind(), "an unreadable tip must report Blind, got {state:?}");
        assert!(!state.is_idle(), "Blind must never satisfy is_idle");
        assert!(state.ids().is_empty() && state.failures().is_empty());
        assert!(
            state.blind_reason().unwrap().contains("no tip"),
            "the reason must name what could not be read, got {:?}",
            state.blind_reason()
        );
    }

    /// The positive control: with a READABLE backend and every entry comfortably ahead of its
    /// deadline, the very same "nothing happened" is reported as `Idle`. The two callers of this
    /// API can now act on the difference.
    #[test]
    fn a_seeing_tower_with_nothing_due_reports_idle() {
        let state = watch_pass(&bundle_due_at(10_000), &electrum_at_tip(100), 6);
        assert_eq!(state, WatchState::Idle, "tip read, nothing due — that is Idle, not Blind");
        assert!(!state.is_blind());
    }

    /// An entry INSIDE its margin whose material cannot be broadcast is reported as a failure, not
    /// as a quiet success — the tower saw the chain, so this is `Acted`, never `Blind` or `Idle`.
    #[test]
    fn a_due_entry_that_cannot_be_driven_is_a_named_failure() {
        // tip 100, deadline 100 => inside the margin; the fake server answers the broadcast with a
        // tip-shaped result, which is not a txid, so the entry fails.
        let state = watch_pass(&bundle_due_at(100), &electrum_at_tip(100), 6);
        assert!(!state.is_blind() && !state.is_idle(), "the chain WAS read, got {state:?}");
        assert!(
            !state.failures().is_empty() || !state.ids().is_empty(),
            "a due entry must be accounted for one way or the other, got {state:?}"
        );
    }

    #[test]
    fn rebroadcast_tolerance_is_narrow() {
        assert!(tolerable_rebroadcast("Transaction already in block chain"));
        assert!(tolerable_rebroadcast("txn-already-known"));
        assert!(tolerable_rebroadcast("outputs already in utxo set"));
        assert!(!tolerable_rebroadcast("bad-txns-inputs-missingorspent"));
        assert!(!tolerable_rebroadcast("txn-mempool-conflict"));
        assert!(!tolerable_rebroadcast("inputs already spent")); // rejection, not idempotence
    }
}
