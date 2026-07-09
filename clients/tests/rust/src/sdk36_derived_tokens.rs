//! E2E (derived deposit tokens): a statechain slot minted by an SE-co-signed flow over an
//! EXISTING statechain (split piece/change, combine outputs, refresh re-anchor) is a DERIVED
//! slot — it re-houses value already inside the SE, adding no on-chain onboarding surface — so it
//! must not consume a (paid, in a token-server deployment) ONBOARDING token. The SE mints free
//! derived tokens itself (`POST /deposit/get_derived_token`), gated on the parent's owner auth
//! (audit-[15] single-use nonce) and a per-parent lifetime cap, marked `derived_from` in its DB.
//!
//! (a) SPLIT NEVER TOUCHES THE POOL: alice deposits 50k (a real onboarding token), then a SENTINEL
//!     token id — syntactically valid, unknown to the SE — is planted in her prepaid pool. Her
//!     off-chain `split_coin` succeeds anyway: its two slots came from the derived-token path (had
//!     the split popped the pool, `deposit/init/pod` would have refused the sentinel with "Token
//!     ID not found"). The piece is then transferred to bob and claimed — a derived-slot coin is
//!     an ordinary, fully transferable coin.
//! (b) REFRESH IS DERIVED TOO: a second flat coin is re-anchored (`refresh`); the fresh slot is
//!     vouched by the OLD statechain id, the sentinel still untouched, and the refreshed coin
//!     confirms with the fee-reduced amount.
//! (c) ONBOARDING STILL CHARGES: `get_deposit_address` (a genuine fresh on-chain slot) pops the
//!     pool — it consumes the sentinel and the SE refuses it ("Token ID not found"), proving (a)
//!     and (b) left the pool alone precisely because they never went through the onboarding path.
//! (d) ENDPOINT GUARDS, adversarially:
//!     d1 a direct `get_derived_tokens(parent, 2)` mints two distinct, immediately-consumable
//!        tokens (one funds a slot via `deposit/init/pod` with no token-server involvement);
//!     d2 `count` above the per-request bound (the configured cap) is refused;
//!     d3 the parent's LIFETIME allowance is enforced (a request that would exceed it is refused
//!        with the issued-so-far count);
//!     d4 a garbage `auth_sig` is refused (401 — fresh-nonce owner auth required);
//!     d5 REPLAY of a once-used, VALID auth_sig is refused (the single-use nonce is consumed);
//!     d6 a valid signature by a NON-owner key (bob's coin) over the parent's challenge is refused.
//!
//! Run: SDK_E2E=36 ML_NETWORK=regtest cargo run
//! (Use an isolated CWD — the suite's wallet.db collides across concurrent runs.)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

/// A syntactically valid uuid the SE has never issued: consuming it MUST fail at deposit/init/pod.
const SENTINEL_TOKEN: &str = "00000000-dead-beef-0000-000000000bad";

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// Deposit `amount` to `w` using a REAL onboarding token, mine, and wait for the CONFIRMED coin.
async fn deposit_confirmed_coin(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    amount: u32,
) -> Result<mercuryrustlib::Coin> {
    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for _ in 0..60 {
        let _ = w.claim().await?;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
        if let Some(c) = rec.coins.iter().rev().find(|c| {
            c.status == CoinStatus::CONFIRMED
                && c.amount == Some(amount)
                && c.aggregated_address.as_deref() == Some(addr.as_str())
        }) {
            return Ok(c.clone());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("deposit of {amount} sats did not confirm for {wallet_name}"))
}

async fn coin_by_id(
    cc: &ClientConfig,
    wallet_name: &str,
    id: &str,
) -> Result<mercuryrustlib::Coin> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    rec.coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(id) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("{wallet_name} has no coin with statechain id {id}"))
}

/// Raw POST to /deposit/get_derived_token with a pre-built auth_sig (for replay/garbage cases).
async fn raw_derived_request(
    cc: &ClientConfig,
    statechain_id: &str,
    auth_sig: &str,
    count: u32,
) -> Result<(u16, String)> {
    let payload = mercurylib::deposit::DerivedTokenRequest {
        statechain_id: statechain_id.to_string(),
        auth_sig: auth_sig.to_string(),
        count,
    };
    let client = cc.get_reqwest_client()?;
    let resp = client
        .post(&format!("{}/deposit/get_derived_token", cc.statechain_entity))
        .json(&payload)
        .send()
        .await?;
    let status = resp.status().as_u16();
    let body = resp.text().await?;
    Ok((status, body))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk36_alice", "./rgb-data-sdk36_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk36_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk36_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    // ===== (a) SPLIT NEVER TOUCHES THE POOL ======================================================
    let coin_a = deposit_confirmed_coin(&cc, &alice, "sdk36_alice", 50_000).await?;
    let id_a = coin_a.statechain_id.clone().ok_or_else(|| anyhow!("deposit has no id"))?;
    // Plant the sentinel: if ANY derived flow below wrongly drew on the onboarding pool, it would
    // consume this bogus token and die inside deposit/init/pod.
    alice.add_prepaid_token(SENTINEL_TOKEN).await;

    let (piece_id, change_id) = alice.split_coin(&id_a, 15_000).await?;
    println!("SDK36 - (a) off-chain split with a poisoned pool succeeded: piece {piece_id}, change {change_id}");
    let piece = coin_by_id(&cc, "sdk36_alice", &piece_id).await?;
    let change = coin_by_id(&cc, "sdk36_alice", &change_id).await?;
    assert_eq!(piece.status, CoinStatus::CONFIRMED);
    assert_eq!(piece.amount, Some(15_000));
    assert_eq!(change.status, CoinStatus::CONFIRMED);

    // The derived-slot piece is an ordinary transferable coin: hand it to bob.
    mercuryrustlib::transfer_sender::execute(
        &cc, &bob_addr, "sdk36_alice", &piece_id, None, false, None,
    )
    .await?;
    let mut bob_got = false;
    for _ in 0..30 {
        let r = bob.claim().await?;
        if r.claimed_transfers > 0 {
            bob_got = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(bob_got, "bob did not claim the derived-slot piece");
    println!("SDK36 - (a) bob claimed the 15k piece minted on a derived slot");

    // ===== (b) REFRESH IS DERIVED TOO ============================================================
    let coin_b = deposit_confirmed_coin(&cc, &alice, "sdk36_alice", 30_000).await?;
    let id_b = coin_b.statechain_id.clone().ok_or_else(|| anyhow!("deposit b has no id"))?;
    let res = alice.refresh(&id_b, None).await?;
    assert_ne!(res.new_statechain_id, id_b);
    assert_eq!(res.new_amount_sats, 30_000 - res.fee_sats);
    let mut refreshed = false;
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, &core)?;
        let _ = alice.claim().await?;
        if let Ok(c) = coin_by_id(&cc, "sdk36_alice", &res.new_statechain_id).await {
            if c.status == CoinStatus::CONFIRMED {
                refreshed = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(refreshed, "refreshed coin did not confirm");
    println!(
        "SDK36 - (b) refresh with a poisoned pool succeeded: {id_b} -> {} ({} sats)",
        res.new_statechain_id, res.new_amount_sats
    );

    // ===== (c) ONBOARDING STILL CHARGES ==========================================================
    // A genuine fresh on-chain slot pops the pool → consumes the sentinel → the SE refuses it.
    let err = alice
        .get_deposit_address(5_000)
        .await
        .expect_err("onboarding with the sentinel token must fail — the pool should still hold it");
    assert!(
        err.to_string().contains("Token ID not found"),
        "expected the sentinel refusal, got: {err}"
    );
    println!("SDK36 - (c) onboarding popped the sentinel and was refused — (a)/(b) never drew on the pool");

    // ===== (d) ENDPOINT GUARDS ===================================================================
    // d1: direct mint against the (now spent, still registered) parent A — the allowance is per
    // statechain LIFETIME, so the split's own 2 tokens count toward it.
    let parent_coin = coin_by_id(&cc, "sdk36_alice", &id_a).await?;
    let toks = mercuryrustlib::deposit::get_derived_tokens(&cc, &parent_coin, &id_a, 2).await?;
    assert_eq!(toks.len(), 2);
    assert_ne!(toks[0], toks[1]);
    // A derived token is confirmed-at-birth: it funds a slot with no token-server involvement.
    let derived_slot_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
        &cc,
        "sdk36_alice",
        &toks[0],
        7_777,
    )
    .await?;
    assert!(!derived_slot_addr.is_empty());
    println!("SDK36 - (d1) direct mint of 2 derived tokens; one consumed for a fresh slot");

    // d2: count above the per-request bound (= the configured cap, default 64).
    let err = mercuryrustlib::deposit::get_derived_tokens(&cc, &parent_coin, &id_a, 1_000)
        .await
        .expect_err("count=1000 must exceed the per-request bound");
    assert!(err.to_string().contains("count must be between"), "got: {err}");

    // d3: lifetime allowance. Parent A has consumed 2 (split) + 2 (d1) = 4 of its 64; asking for
    // 64 more must be refused with the issued-so-far count.
    let err = mercuryrustlib::deposit::get_derived_tokens(&cc, &parent_coin, &id_a, 64)
        .await
        .expect_err("64 more must exceed the lifetime allowance");
    assert!(err.to_string().contains("lifetime derived tokens"), "got: {err}");
    println!("SDK36 - (d2/d3) per-request bound and lifetime allowance enforced");

    // d4: garbage auth.
    let (status, body) = raw_derived_request(&cc, &id_a, "deadbeef:deadbeef", 1).await?;
    assert_eq!(status, 401, "garbage auth must 401, got {status}: {body}");

    // d5: replay of a once-used VALID auth_sig — the single-use nonce is consumed by first use.
    let auth_sig =
        mercuryrustlib::utils::fresh_auth(&cc, &id_a, &parent_coin, "deposit/get_derived_token")
            .await?;
    let (status1, body1) = raw_derived_request(&cc, &id_a, &auth_sig, 1).await?;
    assert_eq!(status1, 200, "first use of a fresh auth must succeed, got {status1}: {body1}");
    let (status2, body2) = raw_derived_request(&cc, &id_a, &auth_sig, 1).await?;
    assert_eq!(status2, 401, "replayed auth_sig must 401, got {status2}: {body2}");
    println!("SDK36 - (d4/d5) garbage auth and nonce replay both refused");

    // d6: a NON-owner's key cannot draw on the parent's allowance: bob signs alice's challenge
    // with HIS coin's auth key — valid schnorr, wrong key.
    let bob_rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk36_bob").await?;
    let bob_coin = bob_rec
        .coins
        .iter()
        .find(|c| c.statechain_id.is_some())
        .cloned()
        .ok_or_else(|| anyhow!("bob has no coin"))?;
    let forged =
        mercuryrustlib::utils::fresh_auth(&cc, &id_a, &bob_coin, "deposit/get_derived_token")
            .await?;
    let (status, body) = raw_derived_request(&cc, &id_a, &forged, 1).await?;
    assert_eq!(status, 401, "non-owner auth must 401, got {status}: {body}");
    println!("SDK36 - (d6) non-owner signature refused");

    println!("SDK36 - ALL ASSERTIONS PASSED: derived slots are free and never draw on the onboarding pool; onboarding still charges; owner-auth + caps enforced");
    Ok(())
}
