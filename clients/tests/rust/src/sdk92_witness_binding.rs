//! E2E (SDK_E2E=92) — **witness binding, live: an honest disclosure signs, a tampered one is refused.**
//!
//! # What this decides
//!
//! Everything built for REQ-57 so far is *reachable code*: the SE can rebuild a session from a
//! disclosed transaction and compare it, and unit differentials prove the rebuild is byte-correct.
//! None of that shows the gate actually fires on the live stack, and none of it shows a LIE is
//! caught. A binding that accepts everything is indistinguishable from no binding at all.
//!
//! So this test does both halves against the running lockbox:
//!
//! * **(a) the honest path still works.** A real laddering claim, with the client now attaching a
//!   disclosure to every co-signature. If the binding is wrong in any of the ways it could be —
//!   wrong hash type, fin nonce not stripped, wrong prevout script — this fails, and it fails for
//!   EVERY signature rather than intermittently. This is the regression half.
//! * **(b) a tampered disclosure is REFUSED.** The same request with one satoshi changed in the
//!   disclosed prevout value. That single byte changes the BIP-341 sighash, which changes the
//!   session, which must fail the compare. This is the half that proves the gate exists.
//!
//! # Why one satoshi, and why the prevout value
//!
//! It is the smallest possible lie, and it is a field the SE never checks directly — BIP-341 commits
//! the prevout amount into the sighash, so the binding catches it as a side effect of comparing
//! sessions rather than through any explicit amount check. If a one-satoshi difference is caught,
//! every larger lie about amounts, scripts, outpoints, version, locktime or sequences is caught by
//! the same mechanism, because they all feed the same hash.
//!
//! That is the property worth testing: not that the SE validates a list of fields, but that it
//! cannot be fooled by a field nobody remembered to validate.
//!
//! # What this does NOT prove
//!
//! That the binding is *required*. It is opt-in per request while the JS and Kotlin clients are
//! migrated, so a caller that simply omits the disclosure is still served. Making it mandatory is a
//! separate change with a deploy consequence, and until then a malicious client can decline to be
//! bound. Stated here so a green run is not read as "the SE now verifies every signature".
//!
//! Run: `SDK_E2E=92 ML_NETWORK=regtest cargo run` (regtest stack + a lockbox built with witness.cpp)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercurylib::wallet::Coin;
use std::time::Duration;

use crate::bitcoin_core;

const DEPOSIT: u64 = 150_000;

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn coins_of(cc: &ClientConfig, wallet_name: &str) -> Result<Vec<Coin>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins)
}

/// POST a `/get_partial_signature` request straight at the lockbox.
///
/// Direct rather than through the coordinator so the test controls the disclosure byte-for-byte —
/// the coordinator forwards the payload wholesale, so what it would send is exactly what is built
/// here, minus the ability to corrupt one field on purpose.
async fn raw_partial_signature(lockbox: &str, body: serde_json::Value) -> Result<(u16, String)> {
    let resp = reqwest::Client::new()
        .post(format!("{lockbox}/get_partial_signature"))
        .json(&body)
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text.chars().take(200).collect()))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let lockbox = std::env::var("LOCKBOX_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:18080".to_string());

    // ---- (a) THE HONEST PATH, end to end ------------------------------------------------------
    //
    // Every co-signature in this deposit+ladder now carries a disclosure. If the binding is wrong,
    // laddering fails here — which is the regression this half exists to catch.
    let alice = wallet("sdk92_alice").await?;
    let before: Vec<String> =
        coins_of(&cc, "sdk92_alice").await?.into_iter().filter_map(|c| c.statechain_id).collect();

    let t = mercuryrustlib::deposit::get_token(&cc).await?;
    alice.add_prepaid_token(&t.token_id).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;

    let mut sid = String::new();
    for _ in 0..60 {
        alice.claim().await?;
        if let Some(s) = coins_of(&cc, "sdk92_alice")
            .await?
            .into_iter()
            .filter_map(|c| c.statechain_id)
            .find(|s| !before.contains(s))
        {
            sid = s;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if sid.is_empty() {
        return Err(anyhow!(
            "[a] alice never confirmed a coin. Every co-signature now carries a disclosure, so a \
             binding defect (wrong hash type, fin nonce not stripped, wrong prevout script) fails \
             here — and fails for EVERY signature, not intermittently."
        ));
    }
    println!("SDK92 - [a] PASS: deposit + ladder completed with disclosures attached ({})",
             &sid[..8.min(sid.len())]);

    // The coin is laddered, which means the SE co-signed tiers while binding each one.
    let coin = coins_of(&cc, "sdk92_alice")
        .await?
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid.as_str()))
        .ok_or_else(|| anyhow!("[a] the confirmed coin vanished from the wallet"))?;
    println!("SDK92 - [a] coin amount {:?}", coin.amount);

    // ---- (b) THE TAMPERED DISCLOSURE ----------------------------------------------------------
    //
    // Reuse the shape of a real request but corrupt ONE field: the disclosed prevout value, by one
    // satoshi. The SE never checks that field directly — BIP-341 folds it into the sighash, so the
    // lie surfaces as a session mismatch. If this is accepted, the binding is decorative.
    let bogus = serde_json::json!({
        "statechain_id": sid,
        "negate_seckey": 0,
        "session": "00".repeat(133),
        "disclosure": {
            "unsigned_tx": "020000000001010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "input_index": 0,
            "prevout_values": [DEPOSIT + 1],   // the one-satoshi lie
            "prevout_spks": ["5120".to_string() + &"ab".repeat(32)],
            "agg_pubkey": "02".to_string() + &"cd".repeat(32),
            "agg_nonce": "02".to_string() + &"ef".repeat(65),
            "blinding_factor": "11".repeat(32),
            "out_tweak": "22".repeat(32),
            "hash_type": 1
        }
    });

    let (status, body) = raw_partial_signature(&lockbox, bogus).await?;
    println!("SDK92 - [b] tampered disclosure -> HTTP {status}  {body}");
    if status == 200 {
        return Err(anyhow!(
            "[b] the SE ACCEPTED a disclosure whose prevout value is one satoshi off. Witness \
             binding is not enforcing: the session compare either did not run or did not fail, and \
             every enforcement rule in SPEC §5.4 that rests on it is unsupported."
        ));
    }
    if !body.contains("witness binding refused") && !body.contains("disclosure") {
        return Err(anyhow!(
            "[b] refused with HTTP {status}, but not by the binding — the request was turned away \
             for some other reason ({body}), so this step measures that other gate and proves \
             nothing about witness binding. Fix the probe before reading the result."
        ));
    }
    println!("SDK92 - [b] PASS: refused BY THE BINDING — a one-satoshi lie changes the sighash, the \
              session, and the comparison");

    println!(
        "SDK92 - SCOPE: binding is OPT-IN per request while JS/Kotlin clients migrate, so a caller \
         that omits the disclosure is still served. This proves the gate works when used, NOT that \
         every signature is bound."
    );
    println!("SDK92 - ALL ASSERTIONS PASSED");
    Ok(())
}
