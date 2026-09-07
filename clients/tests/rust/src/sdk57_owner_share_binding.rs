//! E2E (SDK_E2E=57) — **FATAL-B Stage 1: authoritative sid -> aggregate binding**.
//!
//! The in-ladder split's parent census is defeated by a rogue-key decoy-counter unless the server
//! records an AUTHORITATIVE aggregate per statechain_id (owner_share + enclave_share), UNIQUE, that a
//! receiver can check a coin against — instead of trusting a sender-supplied owner key. This proves the
//! foundation is live, and that it is CONSUMED:
//!   1. a fresh deposit sends the owner signing share, the server records the aggregate, and
//!      /info/statechain returns it EQUAL to the coin's own aggregate x-only;
//!   2. that record is what the receiver's bound verifier reads. The deposit is laddered AT FIRST
//!      MEMPOOL SIGHT (`deposit_coin` hands back a coin whose `T`, `X_0`, `S_0` are already
//!      co-signed), so the coin's census is exactly `num_sigs == 3 == 0 flat + 3 tiers`, and
//!      `verify_bundle_bound(.., 3, 0, authority)` — the authority built from the funding tx ON
//!      CHAIN plus the recorded aggregate — accepts the ladder. It must REFUSE the same ladder with
//!      a flat term of 1 (no phantom `tx1` may be counted), with NO recorded aggregate (fail
//!      closed — the pre-0009 shape), and with a DECOY aggregate that is not the funding output's
//!      key (the rogue-key shape FATAL-B exists to close).
//!
//! Run: SDK_E2E=57 ML_NETWORK=regtest (regtest + lockbox stack, with the migration-0009 server).

use std::env;

use anyhow::{anyhow, Result};

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";
const WALLET: &str = "sdk57_owner";

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = std::fs::remove_dir_all("./rgb-data-sdk57");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // Deposit a coin — the client now sends its owner signing share (user_public_key) in DepositMsg1.
    let coin = deposit_coin(&cc, WALLET).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;

    // The coin's OWN aggregate x-only: create_aggregated_address returns the full compressed aggregate
    // pubkey; its x-only is the last 64 hex chars (drop the 1-byte 02/03 prefix) — exactly what the
    // server stores (hex of aggregate.x_only.serialize()).
    let agg = mercurylib::deposit::create_aggregated_address(&coin, NETWORK.to_string())?;
    if agg.aggregate_pubkey.len() != 66 {
        return Err(anyhow!("unexpected aggregate pubkey format: {}", agg.aggregate_pubkey));
    }
    let coin_aggregate_xonly = agg.aggregate_pubkey[2..].to_lowercase();

    // The server's AUTHORITATIVE record, via /info/statechain.
    let info = mercuryrustlib::utils::get_statechain_info(&sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?;

    let server_aggregate = info
        .aggregate_pubkey
        .clone()
        .ok_or(anyhow!("FATAL-B: server did NOT record an aggregate for a V2-native deposit — owner-share binding not active"))?
        .to_lowercase();

    println!("SDK57 - coin aggregate x-only:   {coin_aggregate_xonly}");
    println!("SDK57 - server-recorded aggregate: {server_aggregate}");

    if server_aggregate != coin_aggregate_xonly {
        return Err(anyhow!(
            "FATAL-B: server aggregate {server_aggregate} != the coin's aggregate {coin_aggregate_xonly} — the sid->aggregate binding is wrong"
        ));
    }

    // ---- 2. The binding is CONSUMED: the deposit-time ladder passes the bound verifier. ---------
    //
    // The receiver never trusts a sender-supplied key. `coin_authority_from_tx0` reads the funding
    // output's value and scriptPubKey from the chain and pairs them with the coordinator's recorded
    // aggregate; `verify_bundle_bound` then checks that the ladder's aggregate address IS that
    // on-chain key AND that the recorded aggregate tweaks to it, and balances the census against
    // the live count with the flat term 0 — a deposit has no `tx1`, so its count is its three tiers.
    let bundle = mercuryrustlib::tesr::load(&cc, WALLET, &sid).await?.ok_or(anyhow!(
        "deposit {sid} has no `tesr-` row — a deposit is laddered at first sight, so there is no \
         ladder here for the binding to be checked against"
    ))?;
    assert_eq!(
        info.num_sigs, 3,
        "a fresh deposit's count must be exactly its deposit-time ladder (0 flat + 3 tiers): a 4 \
         means a flat tx1 is still co-signed at deposit, a 0 means the ladder waited for a block"
    );
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no utxo_txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no utxo_vout"))?;
    assert_eq!(bundle.f_txid, f_txid, "the ladder's trigger spends this coin's funding outpoint");
    assert_eq!(bundle.f_vout, f_vout, "the ladder's trigger spends this coin's funding outpoint");
    let tx0_hex = {
        use electrum_client::ElectrumApi;
        let id: electrum_client::bitcoin::Txid = f_txid.parse()?;
        let tx0 = cc.electrum_client.transaction_get(&id)?;
        hex::encode(electrum_client::bitcoin::consensus::serialize(&tx0))
    };
    let authority = mercuryrustlib::tesr::coin_authority_from_tx0(
        &sid,
        &f_txid,
        f_vout,
        &tx0_hex,
        info.aggregate_pubkey.clone(),
    )?;
    mercuryrustlib::tesr::verify_bundle_bound(&bundle, info.num_sigs, 0, &authority).map_err(|e| {
        anyhow!(
            "FATAL-B: the deposit-time ladder does not pass the bound verifier against the \
             coordinator's recorded aggregate (flat term 0): {e}"
        )
    })?;
    println!(
        "SDK57 - bound verifier accepts the deposit-time ladder against the recorded aggregate: \
         num_sigs {} == 0 flat + 3 tiers",
        info.num_sigs
    );

    // The three refusals that make the acceptance above mean something.
    //  (a) a phantom flat term: 3 != 1 + 3.
    assert!(
        mercuryrustlib::tesr::verify_bundle_bound(&bundle, info.num_sigs, 1, &authority).is_err(),
        "the flat term is ZERO — a census that still counts a phantom tx1 must not balance"
    );
    //  (b) no recorded aggregate: fail CLOSED, by name.
    let unbound = mercuryrustlib::tesr::CoinAuthority {
        se_aggregate_pubkey: None,
        ..authority.clone()
    };
    let unbound_err = mercuryrustlib::tesr::verify_bundle_bound(&bundle, info.num_sigs, 0, &unbound)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        unbound_err.contains("recorded no aggregate"),
        "an authority with NO recorded aggregate must be refused by name (fail-closed), got: {unbound_err:?}"
    );
    //  (c) a DECOY aggregate — a real x-only key that is not the funding output's: the owner's own
    //      share stands in for the rogue key. Refused as a decoy coin.
    if coin.user_pubkey.len() != 66 {
        return Err(anyhow!("unexpected user_pubkey format: {}", coin.user_pubkey));
    }
    let decoy_xonly = coin.user_pubkey[2..].to_lowercase();
    assert_ne!(decoy_xonly, coin_aggregate_xonly, "the decoy must differ from the true aggregate");
    let decoy = mercuryrustlib::tesr::CoinAuthority {
        se_aggregate_pubkey: Some(decoy_xonly),
        ..authority.clone()
    };
    let decoy_err = mercuryrustlib::tesr::verify_bundle_bound(&bundle, info.num_sigs, 0, &decoy)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        decoy_err.contains("does not match the funding output key"),
        "a decoy aggregate must be refused by name as not the funding output's key, got: {decoy_err:?}"
    );

    println!("SDK57 - ✓ PASS: the server records an authoritative sid->aggregate binding equal to the coin's own aggregate, and the receiver's bound verifier consumes it — accepting the deposit-time ladder at num_sigs 3 (0 flat + 3 tiers) and refusing a phantom flat term, a missing record, and a decoy key (FATAL-B Stage 1 live).");
    Ok(())
}
