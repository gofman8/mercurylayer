use std::{env, process::Command, thread};
use anyhow::{anyhow, Result, Ok};

// When `RLN_REGTEST` points at the rgb-lightning-node `regtest.sh` script, bitcoind is driven
// through that regtest harness (bitcoind + electrs:50001 + proxy:3000) instead of the Mercury
// token-servers `esplora-container`. This lets the RGB e2e run against the rgb-lightning-node
// regtest as requested, without breaking the existing tests (which use the default path).
fn rln_script() -> Option<String> {
    env::var("RLN_REGTEST").ok().filter(|s| !s.is_empty())
}

fn rln_bitcoind_container() -> String {
    env::var("RLN_BITCOIND_CONTAINER")
        .unwrap_or_else(|_| "rgb-lightning-node-bitcoind-1".to_string())
}

fn run(cmd: &mut Command) -> Result<String> {
    let output = cmd.output().map_err(|e| anyhow!("failed to spawn command: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(anyhow!(
            "command failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub fn get_container_id() -> Result<String> {
    // First, get the container ID by running the docker ps command
    let output = Command::new("docker")
        .arg("ps")
        .arg("-qf")
        // .arg("name=lnd_docker-bitcoind-1")
        .arg("name=esplora-container")
        .output()
        .expect("Failed to execute docker ps command");

    // Convert the output to a string and trim whitespace
    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if container_id.is_empty() {
        return Err(anyhow!("No container found with the name esplora-container"));
    }

    Ok(container_id)
}

pub fn execute_bitcoin_command(bitcoin_command: &str) -> Result<String> {
    let container_id = get_container_id()?;

    let output = Command::new("docker")
        .arg("exec")
        .arg(&container_id)
        .arg("sh")
        .arg("-c")
        .arg(bitcoin_command)
        .output()
        .expect("Failed to execute docker exec command");

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string().trim().to_string());
    } else {
        return Err(anyhow!("Command execution failed:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
}

pub fn sendtoaddress(amount_in_sats: u32, address: &str) -> Result<String> {

    let amount = amount_in_sats as f64 / 100_000_000.0;

    if let Some(script) = rln_script() {
        return run(Command::new(&script)
            .arg("sendtoaddress")
            .arg(address)
            .arg(format!("{:.8}", amount)));
    }

    let bitcoin_command = format!(
        "cli sendtoaddress {} {}", address, amount
    );

    execute_bitcoin_command(&bitcoin_command)
}

pub fn generatetoaddress(num_blocks: u32, address: &str) -> Result<String> {

    if let Some(script) = rln_script() {
        // regtest.sh mines to its own "miner" wallet; the destination address is irrelevant here.
        let res = run(Command::new(&script).arg("mine").arg(num_blocks.to_string()));
        thread::sleep(std::time::Duration::from_secs(1));
        return res;
    }

    let bitcoin_command = format!(
        "cli generatetoaddress {} {}", num_blocks, address
    );

    let res = execute_bitcoin_command(&bitcoin_command);

    // The command may take some time to execute
    thread::sleep(std::time::Duration::from_secs(1));

    res
}

pub fn getnewaddress() -> Result<String> {

    if rln_script().is_some() {
        return run(Command::new("docker")
            .arg("exec")
            .arg("-u")
            .arg("blits")
            .arg(rln_bitcoind_container())
            .arg("bitcoin-cli")
            .arg("-regtest")
            .arg("-rpcwallet=miner")
            .arg("getnewaddress"));
    }

    let bitcoin_command = format!(
        "cli getnewaddress"
    );

    execute_bitcoin_command(&bitcoin_command)
}

/// Broadcast a fully signed transaction. Returns its txid, or the node's own refusal.
///
/// The node's reason is propagated verbatim rather than summarised: `min relay fee not met`,
/// `dust`, `non-BIP68-final` and `missing-inputs` are four completely different findings, and a
/// caller told only "broadcast failed" learns nothing from any of them.
pub fn sendrawtransaction(tx_hex: &str) -> Result<String> {
    // Straight to `bitcoin-cli`, not through `regtest.sh`: that script exposes only
    // `mine`/`sendtoaddress`/`start`/`stop`, and an unknown subcommand fails with an empty message —
    // which reads as "the broadcast was rejected" when nothing was ever offered to the node.
    if rln_script().is_some() {
        return run(Command::new("docker")
            .arg("exec").arg("-u").arg("blits")
            .arg(rln_bitcoind_container())
            .arg("bitcoin-cli").arg("-regtest")
            .arg("sendrawtransaction").arg(tx_hex));
    }
    execute_bitcoin_command(&format!("cli sendrawtransaction {}", tx_hex))
}

/// A confirmed transaction as the node sees it — used to check what actually landed ON CHAIN rather
/// than what we believe we signed.
pub fn getrawtransaction_verbose(txid: &str) -> Result<serde_json::Value> {
    let raw = if rln_script().is_some() {
        run(Command::new("docker")
            .arg("exec").arg("-u").arg("blits")
            .arg(rln_bitcoind_container())
            .arg("bitcoin-cli").arg("-regtest")
            .arg("getrawtransaction").arg(txid).arg("true"))?
    } else {
        execute_bitcoin_command(&format!("cli getrawtransaction {} true", txid))?
    };
    Ok(serde_json::from_str(&raw)?)
}

/// A fresh P2TR address's x-only key, for use as an exit key.
///
/// Returned as the 32-byte key rather than the address because that is the form the SE's leaf
/// registry stores and the form `build_collapse_tx` pays to. Converting at the boundary once beats
/// carrying two representations of the same key.
pub fn getnewaddress_p2tr_xonly() -> Result<String> {
    let addr = if rln_script().is_some() {
        run(Command::new("docker")
            .arg("exec").arg("-u").arg("blits")
            .arg(rln_bitcoind_container())
            .arg("bitcoin-cli").arg("-regtest").arg("-rpcwallet=miner")
            .arg("getnewaddress").arg("").arg("bech32m"))?
    } else {
        execute_bitcoin_command("cli getnewaddress \"\" bech32m")?
    };
    let info = if rln_script().is_some() {
        run(Command::new("docker")
            .arg("exec").arg("-u").arg("blits")
            .arg(rln_bitcoind_container())
            .arg("bitcoin-cli").arg("-regtest").arg("-rpcwallet=miner")
            .arg("getaddressinfo").arg(&addr))?
    } else {
        execute_bitcoin_command(&format!("cli getaddressinfo {}", addr))?
    };
    let v: serde_json::Value = serde_json::from_str(&info)?;
    let spk = v["scriptPubKey"].as_str().ok_or_else(|| anyhow!("no scriptPubKey for {addr}"))?;
    if spk.len() != 68 || !spk.starts_with("5120") {
        return Err(anyhow!("{addr} is not P2TR (scriptPubKey {spk})"));
    }
    Ok(spk[4..].to_string())
}
