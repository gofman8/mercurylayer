//! E2E (adversarial): regression tests for the security-review fixes, on the **V2 (TES-R) default**.
//!
//! - Part B (value flow on V2): a non-exact payment out of a laddered coin can no longer be split as
//!   plain BTC [B1], so `transfer()` auto-routes to the IN-LADDER split — `SP` descends from the
//!   trigger, the PIECE child pays the recipient (Model A) and is conveyed to their mailbox, the
//!   CHANGE child pays the sender back. The receiver adopts the piece through the
//!   `verify_child_bundle` census (`branch_txs` empty, `ladder_census_ok`) and is credited the exact
//!   amount. V1's branch model — single_use sub-coins plus the receiver's `terminal_parents` ancestor
//!   guard — does not exist on V2; the full in-ladder payment property (adopt + unilateral exit +
//!   funds at the receiver's own key) is owned by sdk59 and the adversarial census cases by sdk58, so
//!   here this is a value-flow smoke check that the payment lands.
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
//! Parts C and D are **protocol-agnostic SE/lockbox-level guards** (the sealed secnonce in
//! lockbox/src/enclave.cpp + server/src/endpoints/sign.rs; the unlock authorization in
//! server/src/endpoints/transfer_receiver.rs). They are keyed on a coin's statechain_id, hold
//! identically under V1 and V2, and are tested NOWHERE ELSE — they must survive every migration.
//!
//! Off-chain double-spend prevention itself is, on V2, the parent/change/piece terminality plus the
//! receiver-side `verify_child_bundle` census (sdk58) and `verify_bundle` (sdk54/sdk55).
//!
//! Run: SDK_E2E=12 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use std::time::Duration;

use crate::bitcoin_core;

const DEPOSIT: u64 = 60_000;
const PAY: u64 = 20_000;
/// A second, self-deposited coin for bob: the raw-MuSig2 / unlock probes need a LIVE 2-of-2.
const PROBE: u64 = 30_000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    // Runs on the V2 (TES-R) default — no protocol pin. Every deposit below auto-establishes a CSV
    // exit ladder at claim(), so alice's non-exact payment exercises the in-ladder split rather than
    // the V1 branch model, and bob's probe coin is a laddered V2 coin. The SE-level guards in Parts C
    // and D are protocol-agnostic and must hold on V2 exactly as they did on V1.
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk12_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk12_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    // --- fund alice 60k ---
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != DEPOSIT {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    // The coin must actually be laddered, otherwise Part B would silently test the old plain-BTC
    // split path instead of the in-ladder split.
    let alice_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk12_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or_else(|| anyhow!("alice has no confirmed coin"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk12_alice", &alice_sid).await?.is_some(),
        "alice's coin must carry a TES-R ladder (V2 default)"
    );
    println!("SDK12 - alice funded {DEPOSIT} on V2 (ladder established, sid {alice_sid})");

    // The two split child slots are funded by FREE derived tokens (take_derived_tokens), so no
    // prepaid-token top-up is needed for the split any more.

    // --- Part B: a non-exact payment auto-routes to the in-ladder split and credits bob ----------
    let r = alice.transfer(&bob_addr, PAY).await?;
    assert!(
        r.used_split,
        "a {PAY} payment out of a single {DEPOSIT} laddered coin must go through the IN-LADDER split"
    );
    // Poll the balance instead of `claimed_transfers`: adopting a conveyed child bundle is not the
    // V1 branch-transfer claim and need not bump that counter (sdk59's pattern).
    let mut waited = 0;
    while bob.get_balance().await?.available_sats != PAY {
        bob.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("bob did not adopt the conveyed in-ladder split child"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        bob.get_balance().await?.available_sats,
        PAY,
        "bob got {PAY} (in-ladder split child adopted via the verify_child_bundle census)"
    );
    println!("SDK12 - Part B: non-exact payment split IN-LADDER; bob credited {PAY} via verify_child_bundle");

    // --- Part C setup: a coin with a LIVE 2-of-2 for the raw MuSig2 probe ------------------------
    // The child bob just adopted is EXIT-ONLY (Model A conveyance: bob holds no SE co-signing key for
    // it), so it cannot drive /sign/first + /sign/second at all. Give bob his own fresh V2 deposit
    // instead — a normal statechain coin (single_use = false, no spend budget) whose ladder leaves the
    // 2-of-2 live. That is exactly the state in which the secnonce single-use guard must hold.
    let t = prepaid_token(&cc).await?;
    bob.add_prepaid_token(&t).await;
    let probe_addr = bob.get_deposit_address(PROBE).await?;
    bitcoin_core::sendtoaddress(u32::try_from(PROBE)?, &probe_addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while bob.get_balance().await?.available_sats != PAY + PROBE {
        bob.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("bob's probe deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // --- Part C (finding 0): nonce reuse over one /sign/first is refused on the 2nd /sign/second ---
    let bob_wallet = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk12_bob").await?;
    let victim = bob_wallet
        .coins
        .iter()
        .find(|c| {
            c.status == mercuryrustlib::CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.amount == Some(PROBE as u32)
        })
        .ok_or_else(|| anyhow!("bob has no confirmed coin for the nonce-reuse probe"))?
        .clone();
    let victim_sid = victim
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow!("victim has no statechain id"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk12_bob", &victim_sid).await?.is_some(),
        "the nonce-reuse probe must run against a V2 (TES-R laddered) coin"
    );
    println!("SDK12 - probe coin {victim_sid} ({PROBE} sat, laddered, live 2-of-2)");

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
    // fetch_one().unwrap()). Both must now return 403 Forbidden. The endpoint is protocol-agnostic.
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

    let real_id = victim_sid.clone();
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

    println!("SDK12 - SUCCESS: V2 in-ladder split payment credits the receiver; MuSig2 nonce reuse refused on a laddered coin; unauthenticated unlock rejected.");
    Ok(())
}
