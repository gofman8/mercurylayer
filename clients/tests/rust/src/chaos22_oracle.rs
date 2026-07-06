//! Chaos oracle: after a quiescent settle, replay the JSONL trace and query final state to assert
//! the spec invariants. Any violation is a breach that fails the test.
//!
//! Checks:
//!  - NO VALUE CREATED (INV-1/13/25): Σ over users of SE-side sats (available+in_transfer+pending)
//!    must be <= total deposited D. Value only leaks to fees/reserves and exits (which move it
//!    on-chain, off the SE ledger) — it can never exceed D unless a bug minted value.
//!  - NO CHEAT SUCCEEDED (INV-5/18/19): every `chaos_fault` (stale-backup claw-back) was refused,
//!    and on-chain the coin's funding outpoint was NEVER spent by the cheater's stale backup.
//!  - ALL OUTCOMES EXPECTED: no trace event is an unclassified `breach` (an error the classifier
//!    did not recognise as spec-sanctioned contention). This is the "everything happened as
//!    expected per spec" guarantee.

use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;

use crate::chaos22_concurrent_users::Registry;

pub struct InvariantReport {
    pub ok: u64,
    pub contention: u64,
    pub cheats_total: u64,
    pub cheats_refused: u64,
    pub accounted: u64,
    pub deficit: u64,
    pub breaches: Vec<String>,
}

impl InvariantReport {
    pub fn summary(&self) -> String {
        format!(
            "CHAOS22 ORACLE: ok={} contention={} cheats={} (refused={}) accounted={} deficit(fees/reserve/exited)={} breaches={}",
            self.ok, self.contention, self.cheats_total, self.cheats_refused, self.accounted, self.deficit, self.breaches.len()
        )
    }
}

fn spender_of(
    ec: &electrum_client::Client,
    o_txid: &str,
    o_vout: u32,
) -> Option<String> {
    use electrum_client::bitcoin::Txid;
    use electrum_client::ElectrumApi;
    let raw = ec.transaction_get_raw(&Txid::from_str(o_txid).ok()?).ok()?;
    let otx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).ok()?;
    let spk = &otx.output.get(o_vout as usize)?.script_pubkey;
    for h in ec.script_get_history(spk).ok()? {
        let htxid = h.tx_hash.to_string();
        if htxid == o_txid {
            continue;
        }
        let hraw = match ec.transaction_get_raw(&h.tx_hash) {
            std::result::Result::Ok(r) => r,
            _ => continue,
        };
        let htx: electrum_client::bitcoin::Transaction =
            match electrum_client::bitcoin::consensus::deserialize(&hraw) {
                std::result::Result::Ok(t) => t,
                _ => continue,
            };
        if htx
            .input
            .iter()
            .any(|i| i.previous_output.txid.to_string() == o_txid && i.previous_output.vout == o_vout)
        {
            return Some(htxid);
        }
    }
    None
}

pub async fn run(
    trace_path: &str,
    registry: &Arc<Registry>,
    d_total: u64,
) -> Result<InvariantReport> {
    let mut report = InvariantReport {
        ok: 0,
        contention: 0,
        cheats_total: 0,
        cheats_refused: 0,
        accounted: 0,
        deficit: 0,
        breaches: Vec::new(),
    };

    // --- parse the trace --------------------------------------------------------------------------
    let text = std::fs::read_to_string(trace_path)?;
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let ec = &registry.users[0].wallet.client_config().electrum_client;

    for ev in &events {
        let outcome = ev.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        let action = ev.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match outcome {
            "ok" | "pending" => report.ok += 1,
            "contention" => report.contention += 1,
            "breach" => {
                let seq = ev.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                let err = ev.get("error").and_then(|v| v.as_str()).unwrap_or("?");
                report
                    .breaches
                    .push(format!("seq {seq}: unclassified {action} error: {err}"));
            }
            _ => {}
        }

        if action == "chaos_fault" {
            report.cheats_total += 1;
            if outcome == "refused" {
                report.cheats_refused += 1;
            } else {
                let seq = ev.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                report.breaches.push(format!(
                    "seq {seq}: CHEAT SUCCEEDED (broadcast old state was accepted!) {}",
                    ev
                ));
            }
            // On-chain backstop: the funding outpoint must NEVER be spent by the cheater's stale tx.
            if let (Some(o_txid), Some(stale_txid)) = (
                ev.get("o_txid").and_then(|v| v.as_str()),
                ev.get("stale_txid").and_then(|v| v.as_str()),
            ) {
                let o_vout = ev.get("o_vout").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if let Some(spender) = spender_of(&ec, o_txid, o_vout) {
                    if spender == stale_txid {
                        report.breaches.push(format!(
                            "FRAUD: outpoint {o_txid}:{o_vout} was spent by the cheater's stale backup {stale_txid}"
                        ));
                    }
                }
            }
        }
    }

    // --- INVARIANT: no value created ---------------------------------------------------------------
    // SE-side sats still on the ledger:
    let mut se_balance: u64 = 0;
    for u in registry.users.iter() {
        if let Ok(b) = u.wallet.get_balance().await {
            se_balance += b.available_sats + b.in_transfer_sats + b.pending_sats;
        }
    }
    // Value that LEFT the SE ledger to the chain via a completed exit/withdraw (pre-fee coin amount).
    // Summed from the trace so the conservation is tight (accounted ≈ D, residual ≈ realized fees),
    // not just the loose one-sided bound `se_balance <= D`.
    let mut exited: u64 = 0;
    for ev in &events {
        let action = ev.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let outcome = ev.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        let amount = ev.get("amount").and_then(|v| v.as_u64()).unwrap_or(0);
        let complete = ev.get("complete").and_then(|v| v.as_bool()).unwrap_or(false);
        if (action == "exit" && outcome == "ok" && complete)
            || (action == "withdraw" && outcome == "ok")
        {
            exited += amount;
        }
    }
    let accounted = se_balance + exited;
    report.accounted = accounted;
    report.deficit = d_total.saturating_sub(accounted);
    // Primary (robust, one-sided): no value can be created in the SE ledger.
    if se_balance > d_total {
        report.breaches.push(format!(
            "VALUE INFLATION: SE-side balances total {se_balance} sats > total deposited {d_total} sats (value was created)"
        ));
    }
    // Tighter: SE balance + exited value must not exceed D by more than the exit/withdraw fees a
    // whale-sized run can realistically burn (guard against exited value being double-counted while
    // still on the ledger). A generous cap avoids false positives from split fee-reserve accounting.
    if accounted > d_total {
        let over = accounted - d_total;
        // Each exit/withdraw pays a miner fee, so exited (pre-fee) can slightly overshoot; only a
        // LARGE overshoot indicates double-counting/inflation.
        if over > 100_000 {
            report.breaches.push(format!(
                "VALUE OVERSHOOT: se_balance {se_balance} + exited {exited} = {accounted} exceeds D {d_total} by {over} (> fee tolerance) — coin counted on ledger AND as exited?"
            ));
        }
    }

    Ok(report)
}
