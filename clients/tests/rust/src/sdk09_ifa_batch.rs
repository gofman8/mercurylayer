//! E2E (G3): issuer parity on RGB rails — IFA (inflatable) issuance, on-chain mint (inflate), and
//! a batch multi-recipient token transfer in one colored split.
//!
//! alice issues 1000 IFT (with 500 inflation-right), mints +500 (on-chain inflate bound to a new
//! statechain coin), then batch-sends 200 to bob and 300 to carol in ONE off-chain colored split.
//! Both receivers validate their own consignment amount (G2) and book it.
//!
//! Run: SDK_E2E=9 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::bitcoin_core;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

fn token_balance(b: &mercury_utexo_sdk::Balance, asset: &str) -> u64 {
    b.tokens.iter().find(|t| t.asset_id == asset).map(|t| t.balance).unwrap_or(0)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk9_alice", "./rgb-data-sdk9_bob", "./rgb-data-sdk9_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    // Background miner: the on-chain inflate needs blocks to confirm during mint's poll.
    let mining = Arc::new(AtomicBool::new(true));
    let miner = {
        let m = mining.clone();
        let core = core.clone();
        std::thread::spawn(move || {
            while m.load(Ordering::Relaxed) {
                let _ = bitcoin_core::generatetoaddress(1, &core);
                std::thread::sleep(Duration::from_secs(2));
            }
        })
    };
    let stop_miner = || mining.store(false, Ordering::Relaxed);

    let mk = |name: &str| {
        let mut cfg = SdkConfig::regtest(name);
        cfg.rgb_data_dir = Some(format!("./rgb-data-{name}"));
        cfg
    };
    let (alice, _) = UtexoWallet::initialize(mk("sdk9_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(mk("sdk9_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(mk("sdk9_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    // Fund alice's RGB engine for issuance + mint witness txs.
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(500_000, &rgb_fund)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // --- Issue an inflatable token: 1000 now + 500 inflation-right ------------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset = alice
        .issue_inflatable_token("IFT", "Inflatable Token", 0, 1000, vec![500])
        .await?;
    println!("SDK09 - issued IFA {asset} (1000 + 500 inflation-right)");

    let mut waited = 0;
    loop {
        alice.claim().await?;
        if token_balance(&alice.get_balance().await?, &asset) >= 1000 {
            break;
        }
        waited += 1;
        if waited > 60 {
            stop_miner();
            return Err(anyhow!("issued supply did not settle"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK09 - alice holds 1000 IFT on a statechain coin");

    // --- Mint: realize +500 inflation-right (on-chain inflate) and bind to a new coin ----------
    let (mint_txid, minted) = alice.mint_tokens(&asset, vec![500]).await?;
    assert_eq!(minted, 500);
    let mut waited = 0;
    loop {
        alice.claim().await?;
        if token_balance(&alice.get_balance().await?, &asset) >= 1500 {
            break;
        }
        waited += 1;
        if waited > 60 {
            stop_miner();
            return Err(anyhow!("minted supply did not settle"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK09 - minted +500 IFT (inflate tx {mint_txid}); alice now holds 1500 IFT");

    // The RGB balance settles immediately (register synthesizes rows), but the two carrier deposit
    // coins must be electrum-CONFIRMED before they can be spent by the colored split. Wait for both.
    let mut waited = 0;
    while alice.get_balance().await?.available_sats < 20_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            stop_miner();
            return Err(anyhow!("token carrier coins did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK09 - both token carrier coins confirmed");

    // --- Batch transfer: 200 -> bob, 300 -> carol in ONE colored split -------------------------
    let mut bob_events = bob.subscribe();
    let mut carol_events = carol.subscribe();
    let bob_bg = bob.start_background();
    let carol_bg = carol.start_background();
    for _ in 0..4 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }

    let results = alice
        .batch_transfer_tokens(&asset, &[(bob_addr.clone(), 200), (carol_addr.clone(), 300)])
        .await?;
    assert_eq!(results.len(), 2);
    println!("SDK09 - batch split: 200 -> bob, 300 -> carol (one colored tx)");

    async fn wait_token(events: &mut tokio::sync::broadcast::Receiver<WalletEvent>) -> Result<u64> {
        tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                match events.recv().await {
                    Ok(WalletEvent::TokenTransferClaimed { amount, .. }) => break Ok(amount),
                    Ok(_) => continue,
                    Err(e) => break Err(anyhow!("event stream closed: {e}")),
                }
            }
        })
        .await
        .map_err(|_| anyhow!("no token claim in time"))?
    }
    let bob_amt = wait_token(&mut bob_events).await?;
    let carol_amt = wait_token(&mut carol_events).await?;
    bob_bg.abort();
    carol_bg.abort();

    assert_eq!(bob_amt, 200, "bob booked the consignment-derived 200");
    assert_eq!(carol_amt, 300, "carol booked the consignment-derived 300");
    assert_eq!(token_balance(&bob.get_balance().await?, &asset), 200);
    assert_eq!(token_balance(&carol.get_balance().await?, &asset), 300);
    let alice_bal = token_balance(&alice.get_balance().await?, &asset);
    assert_eq!(alice_bal, 1000, "alice keeps 1500 - 500 sent = 1000 IFT");
    println!("SDK09 - balances: alice 1000, bob 200, carol 300 IFT");

    stop_miner();
    let _ = miner.join();
    println!("SDK09 - SUCCESS: IFA issuance + on-chain mint (inflate bound to a coin) + batch multi-recipient off-chain transfer; each receiver booked its consignment-verified amount. G3 closed.");
    Ok(())
}
