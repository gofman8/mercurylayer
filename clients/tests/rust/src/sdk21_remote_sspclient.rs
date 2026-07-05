//! E2E: the REMOTE SSP client — a real user swaps against a DEPLOYED `mercury-ssp` HTTP server,
//! not an in-process SspService. This is the "easy to use in the SDK" path: the same
//! `wallet.pay_lightning_invoice(&ssp, ..)` / `create_lightning_invoice(&ssp, ..)` calls, with
//! `ssp` an `SspClient` pointing at a URL. Exercises the whole HTTP layer (serialization, the
//! server's `{error:..}` contract, background settle spawn, DB isolation) that in-process tests skip.
//!
//! Run: SDK_E2E=21 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run

use anyhow::{anyhow, Result};
use mercury_spark_sdk::ssp::{RlnClient, Ssp, SspClient};
use mercury_spark_sdk::{SdkConfig, SparkWallet, WalletEvent};
use sha2::{Digest, Sha256};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::{bitcoin_core, rln};

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

struct SspServer(Child);
impl Drop for SspServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let ssp_db = "/tmp/sdk21_ssp.db";
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{ssp_db}{suffix}"));
    }
    let cc = mercuryrustlib::client_config::load().await;

    // merchant = A (also the payer for the receive leg); SSP node = B.
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk21").await?;
    println!("SDK21 - LN pair up");

    // Fund the SSP wallet IN-PROCESS in its own db file, then drop it so the server owns the db.
    {
        let mut cfg = SdkConfig::regtest("sdk21_ssp");
        cfg.database_file = ssp_db.to_string();
        let (ssp_wallet, _) = SparkWallet::initialize(cfg, None).await?;
        let t = prepaid_token(&cc).await?;
        ssp_wallet.add_prepaid_token(&t).await;
        let addr = ssp_wallet.get_deposit_address(100_000).await?;
        bitcoin_core::sendtoaddress(100_000, &addr)?;
        let core = bitcoin_core::getnewaddress()?;
        bitcoin_core::generatetoaddress(3, &core)?;
        let mut waited = 0;
        while ssp_wallet.get_balance().await?.available_sats != 100_000 {
            ssp_wallet.claim().await?;
            waited += 1;
            if waited > 60 {
                return Err(anyhow!("SSP deposit did not confirm"));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        // Split slots for the receive leg (mints an exact coin).
        for _ in 0..2 {
            let t = prepaid_token(&cc).await?;
            ssp_wallet.add_prepaid_token(&t).await;
        }
        println!("SDK21 - SSP wallet funded 100k (db {ssp_db})");
    }

    // Boot the mercury-ssp server binary against that funded wallet + the SSP's RLN node.
    let bin = format!(
        "{}/target/debug/mercury-ssp",
        std::env::var("CARGO_WORKSPACE_DIR").unwrap_or_else(|_| "/Users/gofman/Claude/mercurylayer".to_string())
    );
    let child = Command::new(&bin)
        .env("SSP_WALLET_NAME", "sdk21_ssp")
        .env("SSP_DATABASE_FILE", ssp_db)
        .env("SSP_NETWORK", "regtest")
        .env("SSP_RLN_API", &ssp_node.api)
        .env("SSP_FEE_SATS", "0")
        .env("ROCKET_PORT", "8100")
        .env("ROCKET_ADDRESS", "127.0.0.1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("failed to spawn mercury-ssp ({bin}): {e}"))?;
    let _server = SspServer(child);

    let ssp = SspClient::new("http://127.0.0.1:8100");
    // Wait for the server to come up.
    let mut up = false;
    for _ in 0..60 {
        if ssp.info().await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !up {
        return Err(anyhow!("mercury-ssp server did not come up on :8100"));
    }
    let info = ssp.info().await?;
    println!("SDK21 - remote SSP up: address {}, fee {}", info.ssp_address, info.fee_sats);

    // --- Error contract: a zero-amount invoice must surface as Err (not a bogus quote) -----------
    let zero_inv = merchant_node.ln_invoice(0, None, 3600).await;
    if let Ok(zi) = zero_inv {
        let q = ssp.quote_pay(&zi).await;
        assert!(q.is_err(), "a zero-amount invoice must be rejected by /quote, not return a bogus PayQuote");
        println!("SDK21 - error contract OK: bad /quote surfaced as Err");
    }

    // --- PAY (Mercury -> Lightning) through the REMOTE SSP ----------------------------------------
    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk21_alice"), None).await?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(50_000).await?;
    bitcoin_core::sendtoaddress(50_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 50_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("alice deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let invoice = merchant_node.ln_invoice(25_000_000, None, 3600).await?;
    let preimage = alice.pay_lightning_invoice(&ssp, &invoice).await?;
    let (_, invoice_hash) = RlnClient::new(&merchant_node.api).decode_invoice(&invoice).await?;
    assert_eq!(
        hex::encode(Sha256::digest(hex::decode(&preimage)?)),
        invoice_hash,
        "preimage from the REMOTE SSP is the invoice's"
    );
    assert_eq!(merchant_node.invoice_status(&invoice).await?, "Succeeded");
    println!("SDK21 - PAY via remote SSP: invoice Succeeded, valid preimage \u{2713}");

    // --- RECEIVE (Lightning -> Mercury) through the REMOTE SSP ------------------------------------
    let (bob, _) = SparkWallet::initialize(SdkConfig::regtest("sdk21_bob"), None).await?;
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    let swap = bob.create_lightning_invoice(&ssp, 20_000).await?;
    assert!(swap.statechain_id.is_none(), "a remote receive swap carries no local statechain_id");
    println!("SDK21 - remote SSP issued bob a 20k invoice (server settles in background)");

    // The payer pays; the server's background settle_receive drives the release + HODL claim.
    let payer = RlnClient::new(&merchant_node.api);
    let inv = swap.invoice.clone();
    let pay_task = tokio::spawn(async move { payer.send_payment(&inv).await });

    let claimed = tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TransferClaimed { statechain_ids }) => break statechain_ids,
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim the coin from the remote receive swap"))?;
    bob_bg.abort();
    let _ = pay_task.await;
    println!("SDK21 - bob claimed his coin ({} coin) from the remote receive", claimed.len());
    assert_eq!(bob.get_balance().await?.available_sats, 20_000, "bob owns 20k");

    println!("SDK21 - SUCCESS: both swap directions work against a DEPLOYED mercury-ssp over HTTP via SspClient — the same wallet calls as in-process. Remote SSP is production-usable from the SDK.");
    Ok(())
}
