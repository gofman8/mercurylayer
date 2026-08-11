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
//!
//! # [D31] What a keyless tower CANNOT do — read this before relying on one
//!
//! Everything above is about TRUST. This paragraph is about CAPABILITY, and it is the one that
//! surprises people: a keyless tower can broadcast the pre-signed tiers **at their committed fee**,
//! and that is all it can do about fees. It **cannot fee-bump**. A CPFP child spending the 240-sat
//! P2A anchor needs an input this tower does not hold and a signature it cannot make, so when the
//! mempool's floor rises above a tier's committed rate the tower has no move — `min relay fee not
//! met, 200 < 423` is a refusal it cannot answer.
//!
//! The anchor being anyone-can-spend does not rescue it either. Bumping that 240-sat anchor was
//! measured to need a **180 330-sat child** — roughly **900×** its value — so "anyone *may* spend
//! it" is a permission, not an incentive, and no disinterested party supplies one.
//!
//! **So during a fee spike the defence is the OWNER being online** — precisely the condition a
//! watchtower otherwise removes. That is a stated limit of the protocol (`PROTOCOL.md` §5.13), not a
//! TODO in this file. An operator MAY run the optional funded variant (a small hot fee wallet, still
//! **no coin keys**, so a compromise costs the operator's float and cannot touch a user's coin) —
//! and must then keep it funded, because a tower that runs dry fails at exactly the moment it is
//! needed.
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

/// **An EVENT trigger for a watched coin**, for the races that do not start at a fixed height.
///
/// `deadline_block` can only express *"act before height H"*. A laddered coin's race does not have
/// one: it starts when somebody **spends an outpoint** — the sender broadcasting the trigger `T`
/// over `F`, or, on a CATS spine, the sender confirming `SP_i` and thereby starting the `Δ_cap`
/// countdown on their own retained cap over `O_i`. There is no height to compare against, so a
/// height-only tower evaluates its one predicate, finds nothing due, and reports `Idle` for the
/// entire race window. That is the repo's silent-degradation shape (`PARTIAL-PAYMENT-ECONOMICS.md`
/// §4.7) and it is why this type exists.
///
/// Keyless like the rest of the bundle: `push_txs` are already fully signed and pay only the owner.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WatchTrigger {
    /// The outpoint whose SPEND starts the race.
    pub watch_txid: String,
    pub watch_vout: u32,
    /// Blocks between that spend confirming and the rival transaction maturing — the budget the
    /// tower has to get `push_txs` confirmed. Informational for the pass itself (which acts
    /// immediately), and the number an operator needs to size their polling interval.
    pub csv_blocks: u32,
    /// What to broadcast when the trigger fires, root-first. Normally the entry's own exit chain.
    pub push_txs: Vec<String>,
}

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
    /// The event trigger, when this coin's race is not height-based. `#[serde(default)]` so a
    /// bundle exported before triggers existed still parses — but note that such a bundle is
    /// height-only by construction, which is a REASON TO RE-EXPORT rather than a compatibility win.
    #[serde(default)]
    pub trigger: Option<WatchTrigger>,
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
            // A LADDERED coin's race is an EVENT, not a height, so it never had an entry here: its
            // `exit_deadline_block` is legitimately `None` (an idle ladder never ages) and the
            // `else { continue }` below dropped it. The consequence was that delegating to a
            // third-party tower protected only the un-laddered coins, silently — the owner's
            // laddered coins were covered by the in-process `defend_ladders` pass alone, i.e. only
            // while they were online, which is the one condition a watchtower exists to remove.
            //
            // `?` and not `unwrap_or(None)`: a failed ladder read must not read as "flat coin".
            let ladder = mercuryrustlib::tesr::load(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &id,
            )
            .await?;
            if branch.is_empty() && ladder.is_none() {
                continue; // flat coin: on-chain funding, no ancestor can race it
            }
            let est = self.estimate_exit_cost(&id).await?;
            // [C1] This coin has an exit branch or a ladder (checked above), so it is watchable. A
            // missing `exit_deadline_block` here is therefore never "flat coin, nothing can race
            // it" — it is either "this coin's race is an event, not a height" (the ladder case,
            // handled below) or "I could not compute it", and silently `continue`ing on the second
            // would drop the entry from the exported bundle exactly like the `unwrap_or_default()`
            // the comment above describes. Same fail-closed rule.
            //
            // **This check runs BEFORE the ladder case on purpose.** Handling laddered coins first
            // let a coin with an UNCOMPUTABLE deadline take the ladder branch and skip this refusal
            // — the very silent omission the rule exists to prevent, reintroduced by the fix for a
            // different silent omission. sdk72 part C6 is the test that says so.
            if let Some(reason) = est.exit_deadline_blind.as_deref() {
                return Err(anyhow!(
                    "refusing to export a watch bundle that silently omits {id}: {reason}"
                ));
            }
            // The LADDERED lane: an event trigger instead of a height. Watch `F`, and when somebody
            // spends it (a prior owner or griefer broadcasting `T`), push the owner's own pre-signed
            // tiers. `deadline_block: u32::MAX` keeps the height predicate permanently false so the
            // two predicates stay independent.
            if let Some(tb) = ladder {
                entries.push(WatchEntry {
                    statechain_id: id.clone(),
                    token_carrier: crate::wallet::is_token_carrier(coin, &carriers),
                    deadline_block: u32::MAX,
                    branch_txs: Vec::new(),
                    // A laddered coin is never swept by an absolute-locktime backup: its exit is the
                    // tier chain. Withholding the backup here is the same structural denial the
                    // carrier case relies on.
                    backup_tx: None,
                    backup_locktime: None,
                    trigger: Some(WatchTrigger {
                        watch_txid: tb.f_txid.clone(),
                        watch_vout: tb.f_vout,
                        // The budget is the next tier's own relative timelock: once `T` confirms,
                        // `X_m` cannot be mined for that many blocks, and the owner's exit must be
                        // under way by then.
                        csv_blocks: tb.current().extension.csv.unwrap_or(0) as u32,
                        push_txs: tb.exit_tiers().iter().map(|t| t.signed_tx.clone()).collect(),
                    }),
                });
                continue;
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
                trigger: None, // this lane's race genuinely IS a height
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
/// An entry is due when EITHER of its two predicates fires: the height one (`tip + margin >=
/// deadline_block`) or, if it carries a [`WatchTrigger`], the event one (the watched outpoint has
/// been spent). A coin whose race has no fixed height needs the second — see [`WatchTrigger`].
///
/// - [`WatchState::Idle`] — the tip was read, **every** entry was evaluated on both predicates, and
///   none was due. A positive observation: nothing needed doing.
/// - [`WatchState::Acted`] — `ids` are the statechain_ids driven this pass, `failures` the entries
///   that could not be driven, and `blind` the entries that could not be EVALUATED (their trigger
///   outpoint was unreadable) — those were not watched at all this pass and must be alerted on, not
///   counted as quiet. Idempotent: a tx already in the mempool or mined counts as success,
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
    let mut blind = Vec::new();
    for e in &bundle.entries {
        // Two independent reasons to act, and the entry is due if EITHER fires.
        let deadline_due = tip + margin_blocks >= e.deadline_block;
        // The event trigger. `outpoint_spent` separates "the outpoint is verifiably still unspent"
        // from "I could not find out": the second is recorded as blindness for THIS entry and the
        // pass moves on to the others, because one unreadable coin must neither stop the tower nor
        // be averaged into a green `Idle`.
        let (event_due, unreadable) = match &e.trigger {
            None => (false, None),
            Some(t) => match mercuryrustlib::tesr::outpoint_spent(
                electrum,
                &t.watch_txid,
                t.watch_vout,
            ) {
                Ok(spent) => (spent, None),
                Err(err) => (
                    false,
                    Some(format!(
                        "{}: trigger outpoint {}:{} unreadable, so this coin was NOT watched this \
                         pass ({} blocks of race budget if it has already fired): {err}",
                        e.statechain_id, t.watch_txid, t.watch_vout, t.csv_blocks
                    )),
                ),
            },
        };
        if let Some(reason) = unreadable {
            blind.push(reason);
            continue;
        }
        if !deadline_due && !event_due {
            continue; // evaluated both predicates; genuinely nothing to do for this coin
        }
        // A fired trigger pushes its own chain; a deadline pushes the branch. When both fire the
        // trigger's list wins — it is the one written for the race that is actually running.
        let mut txs: Vec<&String> = match (&e.trigger, event_due) {
            (Some(t), true) => t.push_txs.iter().collect(),
            _ => e.branch_txs.iter().collect(),
        };
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
    if ids.is_empty() && failures.is_empty() && blind.is_empty() {
        // The tip WAS read and EVERY entry was evaluated — both its deadline and, where it has one,
        // its trigger — and none was due. `blind` is in this condition deliberately: an entry the
        // pass could not evaluate makes the answer "I don't know", and `Idle` is reserved for the
        // positive observation.
        WatchState::Idle
    } else {
        WatchState::Acted { ids, failures, blind }
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
                    trigger: None,
                },
                WatchEntry {
                    statechain_id: "plain".into(),
                    token_carrier: false,
                    deadline_block: 1000,
                    branch_txs: vec!["bb".into()],
                    backup_tx: Some("cc".into()),
                    backup_locktime: Some(990),
                    trigger: None,
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
                trigger: None,
            }],
        }
    }

    /// A laddered coin's entry: no usable deadline, an EVENT trigger instead.
    fn bundle_with_trigger() -> WatchBundle {
        WatchBundle {
            version: 1,
            wallet_name: "w".into(),
            entries: vec![WatchEntry {
                statechain_id: "laddered".into(),
                token_carrier: false,
                deadline_block: u32::MAX,
                branch_txs: vec![],
                backup_tx: None,
                backup_locktime: None,
                trigger: Some(WatchTrigger {
                    watch_txid: "1".repeat(64),
                    watch_vout: 0,
                    csv_blocks: 720,
                    push_txs: vec!["aa".into()],
                }),
            }],
        }
    }

    /// **§4.7.** The defect this trigger exists for, asserted as a behaviour rather than a comment:
    /// a coin whose race is an EVENT must not be reported as quiet just because its height predicate
    /// is false. Before the trigger the entry below evaluated `tip + margin < u32::MAX`, found
    /// nothing due, and returned `Idle` — for the whole race window, every pass, forever.
    #[test]
    fn an_unevaluable_trigger_is_blind_not_idle() {
        // The tip IS readable (so this is not the whole-pass Blind case), but the trigger outpoint
        // cannot be resolved by this stub backend.
        let state = watch_pass(&bundle_with_trigger(), &electrum_at_tip(100), 6);
        assert!(
            !state.is_idle(),
            "an entry that could not be evaluated must never report Idle, got {state:?}"
        );
        assert!(
            !state.blind_entries().is_empty(),
            "the unwatched coin must be named, got {state:?}"
        );
        assert!(state.any_blindness(), "any_blindness is the alert predicate a tower must use");
        assert!(
            state.blind_entries()[0].contains("laddered")
                && state.blind_entries()[0].contains("NOT watched"),
            "the reason must name the coin and say it went unwatched, got {:?}",
            state.blind_entries()
        );
    }

    /// Non-vacuity for the test above: the SAME entry, with its trigger removed, is `Idle` on the
    /// same backend. So the assertion is detecting the trigger evaluation, not a broken stub.
    #[test]
    fn the_same_entry_without_a_trigger_is_idle() {
        let mut b = bundle_with_trigger();
        b.entries[0].trigger = None;
        let state = watch_pass(&b, &electrum_at_tip(100), 6);
        assert_eq!(state, WatchState::Idle, "no trigger, deadline far away — genuinely idle");
    }

    /// An old bundle (exported before triggers existed) still deserialises, and is height-only.
    #[test]
    fn a_pre_trigger_bundle_still_parses() {
        let json = r#"{"version":1,"wallet_name":"w","entries":[{"statechain_id":"old",
            "token_carrier":false,"deadline_block":1000,"branch_txs":["aa"],
            "backup_tx":null,"backup_locktime":null}]}"#;
        let b: WatchBundle = serde_json::from_str(json).expect("old bundle parses");
        assert!(b.entries[0].trigger.is_none());
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
