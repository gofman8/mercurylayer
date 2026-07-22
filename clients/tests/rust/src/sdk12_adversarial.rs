//! E2E (adversarial): regression tests for the security-review fixes.
//!
//! - Part B (fix #2, terminal_parents): an HONEST branch-funded transfer (ancestors named + terminal)
//!   is still accepted — the tightened receiver guard (reject an empty/short ancestor list) must not
//!   break valid off-chain transfers.
//! - Part C (finding 0, MuSig2 nonce reuse): one /sign/first followed by TWO /sign/second over the
//!   SAME server nonce but DIFFERENT messages must be refused on the second call. Reusing a secnonce
//!   over two messages would leak the SE key share and yield two co-signed conflicting spends of one
//!   coin while the finalized-signature counter shows only one (defeats single_use/budget/epoch and
//!   the whole 2-of-2).
//!
//! - Part D (external review finding 1, unauthenticated unlock): /transfer/unlock with a bad
//!   signature and a NULL auth_pub_key must be refused (403), for both a real and an unknown
//!   statechain_id — closing the auth bypass (a garbage sig cleared the receiver-side lock) and the
//!   fetch_one().unwrap() 500 panic on an unknown id.
//!
//! Off-chain double-spend prevention itself (sender skips set_spend_budget → receiver rejects the
//! non-terminal ancestor) is covered by the `terminal_parents_sufficient` unit tests and sdk10.
//!
//! Run: SDK_E2E=12 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use std::time::Duration;

use crate::bitcoin_core;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    // V1-LANE TEST: exercises single_use sub-coins / branch-model adversarial cases (V1 mechanisms).
    // V2 replaces them with the in-ladder split (exit-only children). Pin V1 until V1 is deleted.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk12_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk12_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    // --- fund alice 60k ---
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(60_000).await?;
    bitcoin_core::sendtoaddress(60_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 60_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK12 - alice funded 60k");

    // tokens for the two split sub-coins (bob piece + alice change).
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }

    // --- Part B (fix #2): honest branch transfer is accepted -------------------------------------
    alice.transfer(&bob_addr, 20_000).await?;
    assert_eq!(
        bob.claim().await?.claimed_transfers,
        1,
        "bob must claim his branch-funded piece (honest terminal_parents accepted)"
    );
    assert_eq!(bob.get_balance().await?.available_sats, 20_000, "bob got 20k");
    println!("SDK12 - Part B: honest branch transfer accepted (tightened terminal_parents guard does not break valid transfers)");

    // --- Part C (finding 0): nonce reuse over one /sign/first is refused on the 2nd /sign/second ---
    // bob's received 20k coin is fresh (finalized == 0). Drive the raw MuSig2 rounds manually.
    let bob_wallet = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk12_bob").await?;
    let victim = bob_wallet
        .coins
        .iter()
        .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED)
        .ok_or_else(|| anyhow!("bob has no confirmed coin for the nonce-reuse probe"))?
        .clone();

    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let mut coin = victim.clone();
    let coin_nonce = mercurylib::transaction::create_and_commit_nonces(&coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce);
    coin.public_nonce = Some(coin_nonce.public_nonce);
    coin.blinding_factor = Some(coin_nonce.blinding_factor);
    // ONE /sign/first -> one server nonce (one sealed secnonce in the enclave).
    let server_nonce =
        mercuryrustlib::transaction::sign_first(&cc, &coin_nonce.sign_first_request_payload).await?;
    coin.server_public_nonce = Some(server_nonce);

    let bh = {
        use electrum_client::ElectrumApi;
        cc.electrum_client.block_headers_subscribe_raw()?.height as u32
    };
    let addr_c = bitcoin_core::getnewaddress()?;
    let addr_d = bitcoin_core::getnewaddress()?;

    // First finalize: message m_C (spend to addr_c) over the server nonce -> co-signed.
    let req_c = mercurylib::transaction::get_partial_sig_request(
        &coin, bh, si.initlock, si.interval, 1.0, 0, addr_c, "regtest".to_string(), true,
    )?;
    let sig_c = mercuryrustlib::transaction::sign_second(&cc, &req_c.partial_signature_request_payload).await;
    assert!(sig_c.is_ok(), "first /sign/second must succeed: {:?}", sig_c.err());

    // Second finalize: DIFFERENT message m_D (spend to addr_d) over the SAME server nonce.
    let req_d = mercurylib::transaction::get_partial_sig_request(
        &coin, bh, si.initlock, si.interval, 1.0, 0, addr_d, "regtest".to_string(), true,
    )?;
    let sig_d = mercuryrustlib::transaction::sign_second(&cc, &req_d.partial_signature_request_payload).await;
    assert!(
        sig_d.is_err(),
        "SE MUST refuse a 2nd /sign/second reusing one nonce over a different message (finding 0) — got a second partial sig {:?}",
        sig_d.ok()
    );
    println!("SDK12 - Part C: SE refused the nonce-reuse 2nd /sign/second \u{2713} (finding 0 — no MuSig2 key-share leak)");

    // --- Part D (external review finding 1): /transfer/unlock must not accept a bad signature when
    // auth_pub_key is absent. Before the fix, `!is_current_owner && auth_pub_key.is_some() && ...`
    // short-circuited to false for a null auth_pub_key, so a garbage auth_sig cleared the
    // receiver-side lock (auth bypass), and an unknown statechain_id panicked the handler (500 via
    // fetch_one().unwrap()). Both must now return 403 Forbidden.
    let client = cc.get_reqwest_client()?;
    let unlock = |statechain_id: String| {
        let client = client.clone();
        let url = format!("{}/transfer/unlock", cc.statechain_entity);
        async move {
            client
                .post(&url)
                .json(&serde_json::json!({
                    "statechain_id": statechain_id,
                    "auth_sig": "00".repeat(64), // syntactically-valid but wrong signature
                    "auth_pub_key": serde_json::Value::Null,
                }))
                .send()
                .await
                .map(|r| r.status().as_u16())
        }
    };

    let real_id = victim.statechain_id.clone().ok_or_else(|| anyhow!("victim has no statechain id"))?;
    let status_real = unlock(real_id).await?;
    assert_eq!(
        status_real, 403,
        "bad-sig + null auth_pub_key on a REAL statechain_id must be 403 (auth bypass) — got {status_real}"
    );
    let status_missing = unlock("00000000000000000000000000000000".to_string()).await?;
    assert_eq!(
        status_missing, 403,
        "bad-sig + null auth_pub_key on an UNKNOWN statechain_id must be 403, not a 500 panic — got {status_missing}"
    );
    println!("SDK12 - Part D: /transfer/unlock rejects bad-sig + null auth_pub_key with 403 \u{2713} (finding 1 — no auth bypass, no panic)");

    println!("SDK12 - SUCCESS: honest branch transfers still accepted; MuSig2 nonce reuse refused; unauthenticated unlock rejected.");
    Ok(())
}
