//! A minimal **Bitcoin Core RPC route**, existing for exactly one call: `submitpackage`. [D31, #123]
//!
//! # Why a second backend at all
//!
//! Everything else in this client speaks electrum, and that is fine for everything else. But a
//! TES-R tier that has fallen under the relay floor can only enter a mempool as a **1P1C package**,
//! and **electrum has no `submitpackage` equivalent** — there is no version of `blockchain.
//! transaction.broadcast` that submits a parent and child atomically. The WP1 spike's finding was
//! blunt about the consequence: *no `submitpackage` caller exists anywhere in the tree*, so the
//! rescue that was measured to work in Core could not be invoked by this repo's own code.
//!
//! This module is that caller. It is deliberately tiny: one endpoint, one method, no wallet, no
//! block scanning, no attempt to become a second chain source. Everything the client already does
//! through electrum keeps going through electrum.
//!
//! # Opt-in, and why it must be
//!
//! A Core RPC endpoint is a **credentialled** connection to a node the user runs. It is not
//! something to configure by default, guess at, or fall back to: `SdkConfig::core_rpc` is `None`
//! unless the operator sets it, and with it unset the bump path reports that it is unavailable
//! rather than silently doing nothing. Under [D31] the party holding those credentials is the
//! **owner** (or an operator running the optional funded-tower variant) — never a keyless tower,
//! which has no funding input to spend and so has nothing to submit.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// Where to reach a Bitcoin Core node, and how to authenticate.
///
/// Credentials live here rather than in a URL because a userinfo-bearing URL (`http://u:p@host`)
/// leaks into logs, error strings and metrics the moment anything prints the endpoint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoreRpcConfig {
    /// e.g. `http://127.0.0.1:18443`. No credentials in here.
    pub url: String,
    pub user: String,
    pub password: String,
}

impl CoreRpcConfig {
    pub fn new(url: impl Into<String>, user: impl Into<String>, password: impl Into<String>) -> Self {
        Self { url: url.into(), user: user.into(), password: password.into() }
    }
}

/// Per-transaction result inside a `submitpackage` response.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PackageTxResult {
    pub txid: Option<String>,
    pub error: Option<String>,
    /// Present when the transaction was accepted; absent when it was not.
    #[serde(default)]
    pub fees: Option<serde_json::Value>,
}

/// What Core says about the package as a whole.
#[derive(Clone, Debug, Deserialize)]
pub struct SubmitPackageResult {
    /// `"success"` when the package was accepted. Anything else is a refusal with a reason.
    pub package_msg: String,
    #[serde(default)]
    #[serde(rename = "tx-results")]
    pub tx_results: serde_json::Value,
}

impl SubmitPackageResult {
    /// Core reports package-level failure in a FIELD, not in the JSON-RPC error channel, so a caller
    /// that only checks for an RPC error reads a rejected package as a successful submission. That is
    /// exactly the silent-degradation shape this codebase treats as a defect, so the check lives here
    /// rather than in each call site.
    pub fn accepted(&self) -> bool {
        self.package_msg == "success"
    }
}

/// `submitpackage([parent_hex, child_hex])` — submit a 1P1C package.
///
/// Order matters and is the caller's responsibility: Core requires the package to be sorted so that
/// a parent precedes its child.
///
/// Returns `Err` for transport/auth/RPC failures **and** for a package Core refused, because a
/// refusal is the case this whole path exists to detect. The refusal text is preserved verbatim —
/// `min relay fee not met, 200 < 423` and `TRUC-violation … would have too many ancestors` are the
/// two that actually occur, and both tell the operator precisely what to change.
pub fn submit_package(
    cfg: &CoreRpcConfig,
    parent_hex: &str,
    child_hex: &str,
) -> Result<SubmitPackageResult> {
    #[derive(Serialize)]
    struct Req<'a> {
        jsonrpc: &'a str,
        id: &'a str,
        method: &'a str,
        params: Vec<serde_json::Value>,
    }

    #[derive(Deserialize)]
    struct Resp {
        result: Option<SubmitPackageResult>,
        error: Option<serde_json::Value>,
    }

    let body = Req {
        jsonrpc: "1.0",
        id: "utexo-submitpackage",
        method: "submitpackage",
        params: vec![serde_json::json!([parent_hex, child_hex])],
    };

    let client = reqwest::blocking::Client::builder()
        // A rescue is time-critical — the tier is racing a CSV — so a hung node must surface fast
        // rather than block the exit walk.
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow!("could not build the Core RPC client: {e}"))?;

    let resp = client
        .post(&cfg.url)
        .basic_auth(&cfg.user, Some(&cfg.password))
        .json(&body)
        .send()
        .map_err(|e| {
            // Name the endpoint but never the credentials.
            anyhow!(
                "Core RPC at {} is unreachable ({e}). The package broadcast needs a Bitcoin Core \
                 node: electrum has no `submitpackage`, so there is no fallback for this call.",
                cfg.url
            )
        })?;

    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() && text.trim().is_empty() {
        return Err(anyhow!("Core RPC at {} returned HTTP {status} with no body", cfg.url));
    }

    let parsed: Resp = serde_json::from_str(&text).map_err(|e| {
        anyhow!("Core RPC at {} returned unparseable JSON (HTTP {status}): {e}; body: {text}", cfg.url)
    })?;

    if let Some(err) = parsed.error {
        if !err.is_null() {
            return Err(anyhow!("`submitpackage` was refused by Core: {err}"));
        }
    }

    let result = parsed
        .result
        .ok_or_else(|| anyhow!("Core RPC returned neither a result nor an error: {text}"))?;

    if !result.accepted() {
        return Err(anyhow!(
            "Core accepted the CONNECTION but REFUSED the package: package_msg = {:?}; per-tx: {}. \
             This is the outcome the package path exists to surface — the two that occur in practice \
             are a fee floor (`min relay fee not met`) and TRUC's ancestor limit.",
            result.package_msg,
            result.tx_results
        ));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused package must not read as success. Core reports this in a FIELD, so a caller that
    /// only checks the RPC error channel would treat a rejection as a submission — the failure mode
    /// that makes a stuck coin look rescued.
    #[test]
    fn a_refused_package_is_not_accepted() {
        let refused = SubmitPackageResult {
            package_msg: "transaction failed".to_string(),
            tx_results: serde_json::json!({}),
        };
        assert!(!refused.accepted());

        let ok = SubmitPackageResult {
            package_msg: "success".to_string(),
            tx_results: serde_json::json!({}),
        };
        assert!(ok.accepted());
    }

    /// Credentials must not be carried in the URL, where they would leak into every error string
    /// this module produces (all of which name the endpoint).
    #[test]
    fn credentials_are_separate_from_the_url() {
        let c = CoreRpcConfig::new("http://127.0.0.1:18443", "user", "hunter2");
        assert!(!c.url.contains("hunter2"), "the password must never sit in the URL");
        assert!(!c.url.contains('@'), "no userinfo in the endpoint");
    }

    /// The unreachable-endpoint error must explain that there is no fallback — a reader who assumes
    /// electrum can cover this call will look for a bug that does not exist.
    #[test]
    fn an_unreachable_node_says_there_is_no_electrum_fallback() {
        // Port 1 is reserved and never listening.
        let cfg = CoreRpcConfig::new("http://127.0.0.1:1", "u", "p");
        let err = submit_package(&cfg, "00", "00").unwrap_err().to_string();
        assert!(err.contains("unreachable"), "{err}");
        assert!(
            err.contains("electrum has no `submitpackage`"),
            "the error must say why there is no fallback: {err}"
        );
        assert!(!err.contains("\"p\""), "must not echo credentials: {err}");
    }
}
