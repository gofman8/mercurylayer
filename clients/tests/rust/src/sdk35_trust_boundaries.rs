//! E2E (trust boundaries): the trust relationships a user actually has — watchtower, sender,
//! receiver — exercised adversarially. Companion to [docs/utexo/TRUST-MODEL.md].
//!
//! WATCHTOWER TRUST = NONE (custody-free delegation):
//! (A) KEYLESS: bob exports a `WatchBundle` — fully-signed exit material only. The test asserts the
//!     JSON carries no key material, and that a "watchtower" holding ONLY the bundle + an electrum
//!     connection (no wallet, no DB, no SE, no keys — a fresh raw electrum client) protects both of
//!     bob's off-chain coins: a plain sats sub-coin AND a received token carrier (whose entry
//!     structurally has NO backup tx, so even a malicious watchtower cannot sweep/destroy tokens).
//! (B) MULTIPLE: far from the deadline the pass is a no-op; near it, watchtower #1 materializes
//!     both coins; an INDEPENDENT watchtower #2 (same bundle, separate electrum connection) runs
//!     right after and is harmlessly idempotent. Early broadcast is safe: bob keeps his sats coin
//!     and all 250 tokens — the pre-signed txs pay only bob.
//! (C) CLAWBACK DEFEATED: past the deadline, the malicious sender's matured stale backups (both the
//!     token carrier's and the split parent's) FAIL to broadcast — the roots are already spent.
//!
//! SENDER→RECEIVER TRUST = NONE (two independent guards, receiver's is the real one):
//! (D) FLOORED HANDOVER REJECTED twice over: with auto-refresh OFF, carol's coin ages past its full
//!     ladder horizon (locktime ≤ tip ⟹ every previous owner's backup is broadcastable — an
//!     already-raceable coin). (D1) her HONEST client refuses to even send it ("coin is expired",
//!     transfer_sender.rs — a client-side courtesy a malicious sender can delete). (D2) simulating
//!     that malicious sender — falsifying the stored locktime in carol's own DB so her client's
//!     guard passes — the handover goes out, and DAVE's receiver validation independently rejects
//!     the real (floored) backup locktime (`LocktimeTooLow`, lib receiver.rs): dave books nothing.
//!     The receiver never trusts the sender's ladder; he verifies it. Auto-refresh (sdk33) exists
//!     precisely so honest senders never hit either guard.
//!
//! Run: SDK_E2E=35 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{watch_pass, SdkConfig, UtexoWallet, WatchBundle};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}
async fn add_tokens(cc: &ClientConfig, w: &UtexoWallet, n: usize) -> Result<()> {
    for _ in 0..n {
        let t = prepaid_token(cc).await?;
        w.add_prepaid_token(&t).await;
    }
    Ok(())
}
async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances().await?.into_iter().find(|t| t.asset_id == asset).map(|t| t.balance).unwrap_or(0))
}
async fn wait_token_balance(w: &UtexoWallet, asset: &str, want: u64) -> Result<()> {
    for _ in 0..60 {
        w.claim().await?;
        if token_balance(w, asset).await? == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("settled balance of {asset} did not reach {want}"))
}
async fn wait_carrier(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str, asset: &str, units: u64) -> Result<mercuryrustlib::Coin> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        if token_balance(w, asset).await? >= units {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?;
            if let Some(c) = rec.coins.iter().rev().find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(10_000)) {
                return Ok(c.clone());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: {units} of {asset} carrier did not confirm"))
}
async fn deposit_confirmed_coin(cc: &ClientConfig, w: &UtexoWallet, wallet_name: &str, amount: u32) -> Result<mercuryrustlib::Coin> {
    let addr = w.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for _ in 0..60 {
        let _ = w.claim().await?;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
        if let Some(c) = rec.coins.iter().rev().find(|c| {
            c.status == CoinStatus::CONFIRMED && c.amount == Some(amount) && c.aggregated_address.as_deref() == Some(addr.as_str())
        }) {
            return Ok(c.clone());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("deposit of {amount} sats did not confirm for {wallet_name}"))
}
fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}
fn mine_and_sync(cc: &ClientConfig, core: &str, n: u32) -> Result<u32> {
    let start = tip(cc)?;
    let target = start + n;
    let mut mined = 0;
    while mined < n {
        let batch = (n - mined).min(200);
        bitcoin_core::generatetoaddress(batch, core)?;
        mined += batch;
    }
    for _ in 0..120 {
        if tip(cc)? >= target {
            return Ok(tip(cc)?);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("electrs did not catch up to height {target}"))
}
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    Ok(!cc.electrum_client.script_list_unspent(spk)?.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}
pub async fn execute() -> Result<()> {
    // V2DEF-5: pin V1 — receiver validation asserts V1 LocktimeTooLow (V2 uses CSV, not decrementing locktime). V1-lane regression test under the V2 default.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk35_alice", "./rgb-data-sdk35_bob", "./rgb-data-sdk35_carol", "./rgb-data-sdk35_dave"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk35_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk35_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    // carol: auto_refresh OFF so her coin genuinely floors for scenario (D).
    let mut carol_cfg = SdkConfig::regtest("sdk35_carol");
    carol_cfg.auto_refresh = false;
    let (carol, _) = UtexoWallet::initialize(carol_cfg, None).await?;
    let (dave, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk35_dave"), None).await?;
    let dave_addr = dave.get_utexo_address().await?;

    // carol's coin is deposited FIRST so the aging below floors it by scenario (D).
    let carol_coin = deposit_confirmed_coin(&cc, &carol, "sdk35_carol", 30_000).await?;
    let carol_l0 = carol_coin.locktime.ok_or_else(|| anyhow!("carol coin has no locktime"))?;

    // alice: issue tokens + fund sats.
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    add_tokens(&cc, &alice, 4).await?;
    let asset = alice.issue_token("TB", "Trust Bound", 0, 1000).await?;
    let carrier = wait_carrier(&cc, &alice, "sdk35_alice", &core, &asset, 1000).await?;
    let carrier_id = carrier.statechain_id.clone().ok_or_else(|| anyhow!("carrier has no id"))?;
    let sats_coin = deposit_confirmed_coin(&cc, &alice, "sdk35_alice", 50_000).await?;
    let sats_parent_id = sats_coin.statechain_id.clone().ok_or_else(|| anyhow!("no id"))?;

    // bob receives: 250 tokens (received carrier) + 20_000 sats via split (plain off-chain sub-coin).
    let rt = alice.transfer_tokens(&asset, &bob_addr, 250).await?;
    let bob_token_piece = rt.coins[0].statechain_id.clone();
    wait_token_balance(&bob, &asset, 250).await?;
    let rs = alice.transfer(&bob_addr, 20_000).await?;
    assert!(rs.used_split, "20k from a 50k coin must arrive as a split piece (off-chain sub-coin)");
    let mut got = 0u32;
    for _ in 0..60 {
        got += bob.claim().await?.claimed_transfers;
        if got >= 1 { break; }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(bob.get_balance().await?.available_sats, 20_000, "bob holds the 20k sats sub-coin");
    println!("SDK35 - setup: bob received a plain 20k sats sub-coin + 250 {asset} on a received carrier (both off-chain)");

    // ===== (A) KEYLESS bundle ====================================================================
    let bundle_json = bob.export_watch_bundle().await?;
    let rec_bob = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk35_bob").await?;
    assert!(!bundle_json.contains(&rec_bob.mnemonic), "the watch bundle must NOT contain the wallet mnemonic");
    for kw in ["mnemonic", "seckey", "secret", "private"] {
        assert!(!bundle_json.to_ascii_lowercase().contains(kw), "no key-material field named '{kw}' in the bundle");
    }
    let bundle: WatchBundle = serde_json::from_str(&bundle_json)?;
    assert_eq!(bundle.entries.len(), 2, "exactly bob's two off-chain coins are watched");
    let carrier_entry = bundle.entries.iter().find(|e| e.statechain_id == bob_token_piece).ok_or_else(|| anyhow!("no carrier entry"))?;
    assert!(carrier_entry.token_carrier && carrier_entry.backup_tx.is_none(),
        "the carrier entry is branch-only: even a MALICIOUS watchtower cannot sweep (destroy) the tokens");
    let plain_entry = bundle.entries.iter().find(|e| !e.token_carrier).ok_or_else(|| anyhow!("no plain entry"))?;
    assert!(plain_entry.backup_tx.is_some() && plain_entry.backup_locktime.is_some(), "the plain entry carries the (owner-paying) backup");
    let d = carrier_entry.deadline_block.min(plain_entry.deadline_block);
    println!("SDK35 - (A) KEYLESS: bundle exported — 2 entries, zero key material, carrier entry structurally backup-free; earliest deadline D={d}");

    // Two INDEPENDENT keyless watchtowers: raw electrum connections, no wallet/DB/SE/keys.
    let wt1 = electrum_client::Client::new("tcp://localhost:50001")?;
    let wt2 = electrum_client::Client::new("tcp://localhost:50001")?;

    // ===== (B) far = no-op; near = materialize; second watchtower idempotent =====================
    let (acted0, errs0) = watch_pass(&bundle, &wt1, 20)?;
    assert!(acted0.is_empty() && errs0.is_empty(), "far from D the watchtower must do nothing (acted {acted0:?})");
    println!("SDK35 - (B) far from D: watch_pass is a no-op");

    mine_and_sync(&cc, &core, 150)?;
    let headroom = d as i64 - tip(&cc)? as i64;
    assert!(headroom > 0, "still before the deadline when the watchtowers fire (headroom {headroom})");
    let margin = (headroom + 30) as u32;
    let (acted1, errs1) = watch_pass(&bundle, &wt1, margin)?;
    assert!(errs1.is_empty(), "watchtower #1 must not error: {errs1:?}");
    assert!(acted1.contains(&bob_token_piece) && acted1.len() == 2, "watchtower #1 protects BOTH coins (got {acted1:?})");
    let (acted2, errs2) = watch_pass(&bundle, &wt2, margin)?;
    assert!(errs2.is_empty(), "watchtower #2 (same pre-signed txs) must be harmlessly idempotent: {errs2:?}");
    assert_eq!(acted2.len(), 2, "watchtower #2 re-broadcasts idempotently");
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Roots spent by bob's branches; bob keeps everything (early broadcast is safe — txs pay bob).
    let token_branch = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk35_bob", &format!("branch-{bob_token_piece}")).await?;
    let token_split: electrum_client::bitcoin::Transaction = electrum_client::bitcoin::consensus::deserialize(&hex::decode(&token_branch[0].tx)?)?;
    let token_root = token_split.input[0].previous_output;
    assert!(is_outpoint_spent(&cc, &token_root.txid.to_string(), token_root.vout)?, "the token root was spent by the materialization");
    for _ in 0..20 {
        let _ = bob.claim().await?;
        if token_balance(&bob, &asset).await? == 250 { break; }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(token_balance(&bob, &asset).await?, 250, "bob keeps all 250 tokens after keyless materialization");
    assert_eq!(bob.get_balance().await?.available_sats, 20_000, "bob keeps his 20k sats (branch pays HIM; watchtower had no keys)");
    println!("SDK35 - (B) near D (headroom {headroom} ≤ margin {margin}): keyless watchtower #1 materialized both coins; independent watchtower #2 idempotent; bob keeps 20k sats + 250 {asset} — early broadcast is SAFE, delegation is custody-free");

    // ===== (C) CLAWBACK DEFEATED =================================================================
    mine_and_sync(&cc, &core, d.saturating_sub(tip(&cc)?) + 20)?;
    for (who, id) in [("token carrier", &carrier_id), ("split parent", &sats_parent_id)] {
        let backups = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk35_alice", id).await?;
        let stale = backups.iter().max_by_key(|b| b.tx_n).ok_or_else(|| anyhow!("no backup for {id}"))?;
        let lock = mercurylib::utils::get_blockheight(stale)?;
        assert!(tip(&cc)? > lock, "the sender's {who} backup is mature (tip > {lock})");
        let res = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&stale.tx)?);
        assert!(res.is_err(), "the sender's matured {who} backup must FAIL — its root is already spent");
    }
    println!("SDK35 - (C) CLAWBACK DEFEATED: both of the sender's matured stale backups (token carrier + split parent) fail to broadcast — the keyless watchtower spent the roots in time");

    // ===== (D) FLOORED HANDOVER REJECTED by BOTH independent guards =============================
    let tip_d = tip(&cc)?;
    assert!(tip_d > carol_l0, "carol's coin has floored by now (L0={carol_l0} < tip={tip_d})");

    // (D1) The HONEST sender's own client refuses (transfer_sender.rs "coin is expired").
    let honest = carol.transfer(&dave_addr, 30_000).await;
    let msg = honest.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
    assert!(honest.is_err() && msg.contains("expired"),
        "an honest client must refuse to hand over a floored coin (got: {msg})");
    println!("SDK35 - (D1) honest sender guard: carol's own client refused the floored coin ({msg})");

    // (D2) A MALICIOUS sender deletes that client-side guard. Simulate it: falsify the stored
    // locktime in carol's OWN wallet DB (her client now believes the coin is fresh — exactly what a
    // hacked client does), and send. The pre-signed backup's REAL locktime is untouched (it is a
    // signed tx), so dave's receiver-side validation must reject it (LocktimeTooLow).
    {
        let mut rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk35_carol").await?;
        for c in rec.coins.iter_mut() {
            if c.statechain_id.as_deref() == carol_coin.statechain_id.as_deref() {
                c.locktime = Some(tip_d + 10_000); // the lie
            }
        }
        mercuryrustlib::sqlite_manager::update_wallet(&cc.pool, &rec).await?;
    }
    let sent = carol.transfer(&dave_addr, 30_000).await?;
    assert_eq!(sent.total_sats, 30_000, "the malicious sender's handover goes out (their own guard deleted)");
    let mut dave_claimed = 0u32;
    for _ in 0..12 {
        dave_claimed += dave.claim().await?.claimed_transfers;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(dave_claimed, 0, "dave's receiver validation must REFUSE the floored coin (LocktimeTooLow)");
    assert_eq!(dave.get_balance().await?.available_sats, 0, "dave books nothing — he never accepts an already-raceable coin");
    println!("SDK35 - (D2) MALICIOUS sender (falsified local locktime, sender guard bypassed): the handover went out, and dave's receiver validation refused the coin anyway (claimed 0, balance 0) — the receiver verifies the sender's ladder, he never trusts it");

    println!("SDK35 - SUCCESS: trust boundaries hold. WATCHTOWER: delegation is custody-free — the watch bundle carries zero key material (carrier entries are structurally sweep-proof), a keyless third party with only electrum access protects sats AND tokens, multiple independent watchtowers are idempotent, early broadcast only settles the owner's coins to the owner, and the malicious sender's matured backups all fail. SENDER→RECEIVER: no trust — the honest client refuses to send a floored coin, and even a malicious sender who bypasses that guard is rejected by the receiver's independent validation (LocktimeTooLow). See docs/utexo/TRUST-MODEL.md for the full matrix.");
    Ok(())
}
