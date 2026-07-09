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
//!   broadcasts the SAME pre-signed transactions they can never conflict with each other.
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
            self.token_carrier_outpoints().await?
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
            let branch = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("branch-{id}"),
            )
            .await
            .unwrap_or_default();
            if branch.is_empty() {
                continue; // flat coin: on-chain funding, no ancestor can race it
            }
            let est = self.estimate_exit_cost(&id).await?;
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
/// Returns the statechain_ids acted on this pass. Idempotent: a tx already in the mempool or
/// mined counts as success, so running this from several independent watchtowers (or repeatedly)
/// is safe — they all broadcast the same pre-signed transactions. A genuinely rejected broadcast
/// (e.g. the root already spent by a competing tx — the exit being RACED) fails that entry's
/// remaining txs and is reported in the error list; other entries still proceed.
pub fn watch_pass(
    bundle: &WatchBundle,
    electrum: &electrum_client::Client,
    margin_blocks: u32,
) -> Result<(Vec<String>, Vec<String>)> {
    let tip = electrum.block_headers_subscribe_raw()?.height as u32;
    let mut acted = Vec::new();
    let mut errors = Vec::new();
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
                    errors.push(format!("{}: bad tx hex: {err}", e.statechain_id));
                    ok = false;
                    break;
                }
            };
            match electrum.transaction_broadcast_raw(&raw) {
                Ok(_) => {}
                Err(err) => {
                    let msg = err.to_string();
                    if !tolerable_rebroadcast(&msg) {
                        errors.push(format!("{}: broadcast failed: {msg}", e.statechain_id));
                        ok = false;
                        break;
                    }
                }
            }
        }
        if ok {
            acted.push(e.statechain_id.clone());
        }
    }
    Ok((acted, errors))
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
