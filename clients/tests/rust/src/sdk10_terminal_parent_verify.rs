//! E2E (adversarial, G1): the RECEIVER independently verifies that every structural ancestor of
//! an off-chain sub-coin is TERMINAL at the SE before accepting it — so a malicious sender cannot
//! hand over a sub-coin whose parent it could still double-spend.
//!
//!  - POSITIVE: an honest split (parent made terminal) transfers and is claimed.
//!  - NEGATIVE: a transfer whose claimed ancestor is NOT terminal at the SE is rejected by the
//!    receiver (validation fails, the coin is not booked).
//!
//! Run: SDK_E2E=10 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use std::time::Duration;

use crate::bitcoin_core;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn deposit(alice: &UtexoWallet, cc: &mercuryrustlib::client_config::ClientConfig, amount: u64) -> Result<()> {
    let t = prepaid_token(cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(amount).await?;
    bitcoin_core::sendtoaddress(u32::try_from(amount)?, &addr)?;
    Ok(())
}

fn sc_id_by_amount(record: &mercuryrustlib::Wallet, amount: u64) -> Option<String> {
    record
        .coins
        .iter()
        .find(|c| {
            c.status == mercuryrustlib::CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.amount.unwrap_or_default() as u64 == amount
        })
        .and_then(|c| c.statechain_id.clone())
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk10_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk10_bob"), None).await?;
    let bob_address = bob.get_utexo_address().await?;

    // Three deposits: C=40k (split, positive), D=30k (split, negative), E=20k (never split).
    for amount in [40_000u64, 30_000, 20_000] {
        deposit(&alice, &cc, amount).await?;
    }
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 90_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposits did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    // pooled tokens for the two splits (2 slots each).
    for _ in 0..4 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let record = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk10_alice").await?;
    let c_id = sc_id_by_amount(&record, 40_000).ok_or_else(|| anyhow!("no 40k coin"))?;
    let d_id = sc_id_by_amount(&record, 30_000).ok_or_else(|| anyhow!("no 30k coin"))?;
    let e_id = sc_id_by_amount(&record, 20_000).ok_or_else(|| anyhow!("no 20k coin"))?;
    println!("SDK10 - coins: C={c_id} D={d_id} E={e_id}");

    // --- POSITIVE: honest split of C -> P1; parent C becomes terminal; transfer + claim OK -----
    let (p1_id, _c1) = alice.split_coin(&c_id, 15_000).await?;
    let (_, _, c_terminal) = mercuryrustlib::lightning_latch::get_spend_budget(&cc, &c_id).await?;
    assert!(c_terminal, "parent C must be terminal after the split");
    let (_, _, e_terminal) = mercuryrustlib::lightning_latch::get_spend_budget(&cc, &e_id).await?;
    assert!(!e_terminal, "un-split coin E must NOT be terminal");

    // Direct hand-over of the honest sub-coin (parents=[C], C terminal):
    mercuryrustlib::transfer_sender::execute(&cc, &bob_address, "sdk10_alice", &p1_id, None, false, None).await?;
    let claimed = bob.claim().await?;
    assert_eq!(claimed.claimed_transfers, 1, "honest terminal-parent sub-coin is claimed");
    println!("SDK10 - POSITIVE: honest split (C terminal) -> P1 claimed by bob");

    // --- NEGATIVE: split D -> P2, then TAMPER P2's claimed ancestor to E (non-terminal) --------
    let (p2_id, _c2) = alice.split_coin(&d_id, 15_000).await?;
    // Malicious sender substitutes a non-terminal ancestor into the sub-coin's parent record.
    let tamper = mercuryrustlib::BackupTx {
        tx_n: 1,
        tx: e_id.clone(),
        client_public_nonce: String::new(),
        server_public_nonce: String::new(),
        client_public_key: String::new(),
        server_public_key: String::new(),
        blinding_factor: String::new(),
        rgb_consignment: None,
        rgb_blinding: None,
    };
    mercuryrustlib::sqlite_manager::update_backup_txs(
        &cc.pool,
        "sdk10_alice",
        &format!("parents-{p2_id}"),
        &vec![tamper],
    )
    .await?;
    println!("SDK10 - tampered P2's claimed ancestor -> E (which is NOT terminal)");

    let bob_before = bob.get_balance().await?.available_sats;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_address, "sdk10_alice", &p2_id, None, false, None).await?;
    let claimed = bob.claim().await?;
    assert_eq!(
        claimed.claimed_transfers, 0,
        "receiver MUST reject a sub-coin claiming a non-terminal ancestor"
    );
    let bob_after = bob.get_balance().await?.available_sats;
    assert_eq!(bob_before, bob_after, "no funds booked from the rejected transfer");
    println!("SDK10 - NEGATIVE: transfer claiming non-terminal ancestor E was REJECTED by the receiver");

    println!("SDK10 - SUCCESS: the receiver independently enforces terminal ancestors — honest sub-coins are accepted, a sub-coin whose claimed parent is still spendable is rejected. G1 closed.");
    Ok(())
}
