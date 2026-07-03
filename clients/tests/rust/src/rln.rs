//! RLN (UTEXO rgb-lightning-node) harness: spawn daemons, unlock against the regtest stack,
//! open channels, create/pay (hodl) invoices. Used by the SSP Lightning-swap E2Es.
//!
//! Binary: build in the RLN repo (`cargo build`); path via env `RLN_BIN` or the default sibling
//! checkout `../rgb-lightning-node/target/debug/rgb-lightning-node`.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BITCOIND_RPC_USER: &str = "user";
const BITCOIND_RPC_PASSWORD: &str = "password";
const BITCOIND_RPC_HOST: &str = "localhost";
const BITCOIND_RPC_PORT: u16 = 18443;
const INDEXER_URL: &str = "127.0.0.1:50001";
const PROXY_ENDPOINT: &str = "rpc://127.0.0.1:3000/json-rpc";

pub fn rln_bin() -> String {
    std::env::var("RLN_BIN").unwrap_or_else(|_| {
        "/Users/gofman/Claude/rgb-lightning-node/target/debug/rgb-lightning-node".to_string()
    })
}

pub struct RlnNode {
    pub api: String,
    pub peer_port: u16,
    child: Child,
}

impl Drop for RlnNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl RlnNode {
    /// Spawn a daemon on the given ports with a fresh (or reused) data dir.
    pub async fn start(data_dir: &str, api_port: u16, peer_port: u16, wipe: bool) -> Result<Self> {
        if wipe {
            let _ = std::fs::remove_dir_all(data_dir);
        }
        std::fs::create_dir_all(data_dir)?;
        let child = Command::new(rln_bin())
            .arg(data_dir)
            .arg("--daemon-listening-port")
            .arg(api_port.to_string())
            .arg("--ldk-peer-listening-port")
            .arg(peer_port.to_string())
            .arg("--network")
            .arg("regtest")
            .arg("--disable-authentication")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("failed to spawn RLN ({}): {e}", rln_bin()))?;
        let node = RlnNode {
            api: format!("http://127.0.0.1:{api_port}"),
            peer_port,
            child,
        };
        node.wait_http().await?;
        Ok(node)
    }

    async fn wait_http(&self) -> Result<()> {
        let client = reqwest::Client::new();
        for _ in 0..120 {
            if let Ok(resp) = client
                .get(format!("{}/nodeinfo", self.api))
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                // Any HTTP answer (incl. "locked" errors) means the daemon is up.
                let _ = resp.status();
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(anyhow!("RLN daemon at {} did not come up", self.api))
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let resp = reqwest::Client::new()
            .post(format!("{}/{path}", self.api))
            .json(&body)
            .timeout(Duration::from_secs(120))
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(anyhow!("RLN /{path} -> {status}: {v}"));
        }
        Ok(v)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let resp = reqwest::Client::new()
            .get(format!("{}/{path}", self.api))
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(anyhow!("RLN /{path} -> {status}: {v}"));
        }
        Ok(v)
    }

    /// First run: /init; every run: /unlock with the regtest chain backend.
    pub async fn init_unlock(&self, password: &str) -> Result<()> {
        let _ = self.post("init", json!({ "password": password })).await; // 403 when already initialized — fine
        self.post(
            "unlock",
            json!({
                "password": password,
                "bitcoind_rpc_username": BITCOIND_RPC_USER,
                "bitcoind_rpc_password": BITCOIND_RPC_PASSWORD,
                "bitcoind_rpc_host": BITCOIND_RPC_HOST,
                "bitcoind_rpc_port": BITCOIND_RPC_PORT,
                "indexer_url": INDEXER_URL,
                "proxy_endpoint": PROXY_ENDPOINT,
                "announce_addresses": [],
                "announce_alias": "sspnode"
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn pubkey(&self) -> Result<String> {
        Ok(self
            .get("nodeinfo")
            .await?
            .get("pubkey")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no pubkey"))?
            .to_string())
    }

    pub async fn onchain_address(&self) -> Result<String> {
        Ok(self
            .post("address", json!({}))
            .await?
            .get("address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no address"))?
            .to_string())
    }

    pub async fn open_channel(&self, peer_pubkey: &str, peer_port: u16, capacity_sat: u64, push_msat: u64) -> Result<()> {
        self.post(
            "openchannel",
            json!({
                "peer_pubkey_and_opt_addr": format!("{peer_pubkey}@127.0.0.1:{peer_port}"),
                "capacity_sat": capacity_sat,
                "push_msat": push_msat,
                "public": true,
                "with_anchors": true
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn usable_channels(&self) -> Result<u64> {
        Ok(self
            .get("nodeinfo")
            .await?
            .get("num_usable_channels")
            .and_then(|v| v.as_u64())
            .unwrap_or(0))
    }

    /// Create a BOLT11 invoice; `payment_hash` (hex) makes it a HODL invoice.
    pub async fn ln_invoice(&self, amt_msat: u64, payment_hash: Option<&str>, expiry_sec: u64) -> Result<String> {
        let mut body = json!({ "amt_msat": amt_msat, "expiry_sec": expiry_sec });
        if let Some(h) = payment_hash {
            body["payment_hash"] = json!(h);
        }
        Ok(self
            .post("lninvoice", body)
            .await?
            .get("invoice")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no invoice"))?
            .to_string())
    }

    /// Pay a BOLT11 invoice; returns the payment_hash.
    pub async fn send_payment(&self, invoice: &str) -> Result<String> {
        Ok(self
            .post("sendpayment", json!({ "invoice": invoice }))
            .await?
            .get("payment_hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no payment_hash in sendpayment response"))?
            .to_string())
    }

    pub async fn invoice_status(&self, invoice: &str) -> Result<String> {
        Ok(self
            .post("invoicestatus", json!({ "invoice": invoice }))
            .await?
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string())
    }

    pub async fn payment_status(&self, payment_hash: &str) -> Result<String> {
        let v = self
            .post("getpayment", json!({ "payment_hash": payment_hash }))
            .await?;
        Ok(v
            .get("payment")
            .and_then(|p| p.get("status"))
            .or_else(|| v.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("Unknown")
            .to_string())
    }

    pub async fn claim_hodl(&self, payment_hash: &str, preimage: &str) -> Result<()> {
        self.post(
            "claimhodlinvoice",
            json!({ "payment_hash": payment_hash, "payment_preimage": preimage }),
        )
        .await?;
        Ok(())
    }
}

/// Start two RLN nodes, fund the first on-chain, and open a usable A→B channel.
pub async fn setup_ln_pair(base_dir: &str) -> Result<(RlnNode, RlnNode)> {
    let a = RlnNode::start(&format!("{base_dir}/rln-a"), 3101, 9835, true).await?;
    let b = RlnNode::start(&format!("{base_dir}/rln-b"), 3102, 9836, true).await?;
    a.init_unlock("sspnodepass-a").await?;
    b.init_unlock("sspnodepass-b").await?;

    // Fund A on-chain so it can open the channel.
    let addr = a.onchain_address().await?;
    crate::bitcoin_core::sendtoaddress(1_000_000, &addr)?;
    let core = crate::bitcoin_core::getnewaddress()?;
    crate::bitcoin_core::generatetoaddress(3, &core)?;
    // Let the node see the funds (btcbalance is a POST and syncs unless skipped).
    let mut funded = false;
    for _ in 0..45 {
        let info = a
            .post("btcbalance", json!({ "skip_sync": false }))
            .await
            .unwrap_or(Value::Null);
        let spendable = info
            .get("vanilla")
            .and_then(|v| v.get("spendable"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if spendable >= 950_000 {
            funded = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !funded {
        return Err(anyhow!("RLN node A never saw its on-chain funds"));
    }

    // Open A -> B with pushed liquidity so both directions can pay.
    let b_pub = b.pubkey().await?;
    a.open_channel(&b_pub, b.peer_port, 900_000, 300_000_000).await?;
    // Confirm the funding tx and wait for the channel to be usable on both ends.
    for _ in 0..60 {
        crate::bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        if a.usable_channels().await.unwrap_or(0) >= 1 && b.usable_channels().await.unwrap_or(0) >= 1 {
            return Ok((a, b));
        }
    }
    Err(anyhow!("channel did not become usable"))
}
