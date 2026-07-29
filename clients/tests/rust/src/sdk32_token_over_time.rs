//! E2E (tokens over time, V2/TES-R): what happens to statechain tokens if you ISSUE or RECEIVE them
//! and then do NOTHING for a long time (a "year" of blocks, far beyond any deployed horizon)?
//! Answers, empirically: are they lost? can you still send them, exit unilaterally, or exit
//! cooperatively?
//!
//! Under the V2 default a plain BTC coin NEVER ages (its TES-R ladder is a relative CSV on an
//! un-broadcast trigger — 0 vB rent while idle), and an RGB **carrier** is never laddered at all:
//! terminal-freeze (PROTOCOL.md §5.10 rule 1), because RGB rides the signed-once colored-split model
//! and a plain tier spend of a carrier would destroy the allocation. So, for tokens:
//!
//! (A) ISSUED tokens (flat carrier), long idle: the carrier is TERMINAL-FROZEN — `tesr::load` → None,
//!     it carries no ladder, only the signed-once deposit backup that is its plain exit. The tokens
//!     are NOT lost and are STILL sendable: a token transfer is a colored SPLIT
//!     (`create_colored_split_tx`), a path that never touches the ladder, so no amount of idling can
//!     make an allocation un-sendable. Movement needs the SE (a plain unilateral exit is REFUSED — it
//!     would destroy the allocation); issued tokens have NO ancestor, so there is NO clawback risk.
//! (B) RECEIVED tokens (colored sub-coin), long idle: still NOT lost, and terminal-frozen too.
//!     UNILATERAL materialization works forever — the colored exit branch is nLockTime **0**
//!     (INV-4 / review H5: a branch must always be broadcastable now and always mature before any
//!     deposit-anchored parent backup), so broadcasting it settles the allocation on-chain at any
//!     height, provided the shared root is still unspent. A single 1,500-sat received piece is below
//!     the carrier floor, so it can't be re-sent alone (hold / combine / exit).
//! (C) The residual DANGER for received tokens. V2 closes the SE side and only the SE side: the
//!     colored split sets the carrier's spend budget to 1, so afterwards the carrier is TERMINAL at
//!     the SE and the sender can never obtain a FRESH co-signed sweep. What V2 does not remove is the
//!     sender's ONE pre-signed deposit backup — TES-R deliberately never covers a carrier, so that
//!     backup is still a plain absolute-locktime tx and it matures at `deposit_height + initlock`.
//!     Past that a malicious sender could sweep the shared root and claw the tokens back — UNLESS you
//!     materialized first (which you can do from the very first block, your branch being locktime-0).
//!     A received holder must exit before the root deadline; the background `auto_exit_due` pass
//!     materializes a due carrier, but only for a wallet that is running the watcher.
//!
//! Run: SDK_E2E=32 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
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
fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}
/// Mine `n` blocks in batches and wait until electrs's tip catches up (so later reads are current).
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
fn parse_tx(h: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(h)?)?)
}
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    Ok(!cc.electrum_client.script_list_unspent(spk)?.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}
async fn coin_of(cc: &ClientConfig, name: &str, id: &str) -> Result<mercuryrustlib::Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?
        .coins.into_iter().find(|c| c.statechain_id.as_deref() == Some(id) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("{name} has no coin {id}"))
}

pub async fn execute() -> Result<()> {
    // No protocol pin: this runs on the V2 (TES-R) default. That is the point of the test — under V2
    // an idle plain coin never ages, and an RGB carrier is deliberately kept OFF the ladder
    // (terminal-freeze), so "what happens to tokens left alone for a year?" has a V2-specific answer.
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk32_alice", "./rgb-data-sdk32_bob", "./rgb-data-sdk32_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;

    let alice_cfg = SdkConfig::regtest("sdk32_alice");
    // (single protocol: every plain deposit is laddered; RGB carriers deliberately are not)
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk32_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk32_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // ===== (A) ISSUED tokens (flat carrier) idle for a "year" ====================================
    add_tokens(&cc, &alice, 1).await?;
    let asset = alice.issue_token("YR", "Year Token", 0, 1000).await?;
    let carrier = wait_carrier(&cc, &alice, "sdk32_alice", &core, &asset, 1000).await?;
    let carrier_id = carrier.statechain_id.clone().ok_or_else(|| anyhow!("carrier has no id"))?;
    // Terminal-freeze invariant (PROTOCOL.md §5.10 rule 1; same check as sdk52): an RGB carrier must
    // NEVER be anchored on the renewable T/X/S ladder — a plain tier spend would destroy the
    // allocation. Its only plain exit is the signed-once deposit backup; the asset moves by colored
    // split. So "does the carrier age?" is the wrong question here — it has no ladder to age.
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk32_alice", &carrier_id).await?.is_none(),
        "the RGB carrier {carrier_id} must NOT carry a TES-R ladder (terminal-freeze §5.10 rule 1)"
    );
    let l0 = carrier.locktime.ok_or_else(|| anyhow!("carrier has no deposit-backup locktime"))?;
    println!("SDK32 - alice issued 1000 {asset} on a flat carrier; carrier is TERMINAL-FROZEN (no TES-R ladder), deposit-backup locktime L0={l0}, initlock={initlock}");

    // Do nothing for a "year": advance the chain far past every horizon (initlock + 500 blocks).
    let tip_yr = mine_and_sync(&cc, &core, initlock + 500)?;
    alice.claim().await?;
    assert_eq!(token_balance(&alice, &asset).await?, 1000, "the tokens are NOT lost after long inactivity");
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk32_alice", &carrier_id).await?.is_none(),
        "idling must not ladder the carrier — claim() keeps honouring terminal-freeze after {} blocks", initlock + 500
    );
    println!("SDK32 - (A) after ~{} idle blocks (tip={tip_yr}) alice still holds all 1000 {asset}, and the carrier is STILL terminal-frozen — an idle allocation simply does not age", initlock + 500);

    // A PLAIN unilateral exit of the (still-current) issued carrier is REFUSED — it would sweep the
    // sats and destroy the allocation. Issued tokens are anchored on-chain and move only via the SE
    // (colored) path; they are never lost and have NO clawback risk (no ancestor above them).
    let plain_exit = alice.unilateral_exit(Some(vec![carrier_id.clone()]), None).await;
    assert!(plain_exit.is_err(), "a plain unilateral exit of a token carrier must be refused (carrier guard)");
    println!("SDK32 - (A) a plain unilateral exit of the issued carrier is refused (would destroy tokens); issued tokens move only via the SE, are never lost, and have NO clawback risk");

    // COOPERATIVE SEND still works after the long idle — the colored-split path never touches a
    // ladder, so there is nothing about the passage of time that can block it.
    add_tokens(&cc, &alice, 3).await?;
    let r = alice.transfer_tokens(&asset, &bob_addr, 250).await?;
    assert!(r.used_split, "a token transfer is an off-chain colored SPLIT");
    let bob_piece = r.coins[0].statechain_id.clone();
    wait_token_balance(&bob, &asset, 250).await?;
    assert_eq!(token_balance(&alice, &asset).await?, 750, "alice sent 250 from a long-idle carrier");
    assert_eq!(token_balance(&bob, &asset).await?, 250, "bob booked the 250-unit colored piece");
    // Terminal-freeze holds on the RECEIVING side too: the piece is a colored sub-coin, never a
    // laddered coin. Its exit is the locktime-0 branch asserted in (B), not a T/X/S tier.
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk32_bob", &bob_piece).await?.is_none(),
        "bob's received colored sub-coin must NOT be laddered either (terminal-freeze)"
    );
    println!("SDK32 - (A) COOPERATIVE SEND works after a year: alice→bob 250 (balances 750/250), both sides terminal-frozen (no ladder on either carrier) — long inactivity does NOT block sending");

    // ===== (B) RECEIVED tokens (colored sub-coin) idle for another "year" ========================
    // bob's 250-piece branch = the colored split tx; its root is alice's carrier outpoint.
    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk32_bob", &format!("branch-{bob_piece}")).await?;
    assert_eq!(branch.len(), 1, "bob's piece is a depth-1 sub-coin (branch = the colored split)");
    // INV-4 / review H5: an off-chain colored branch is built with nLockTime 0, so it is
    // UNCONDITIONALLY broadcastable now and always sits below any deposit-anchored parent backup —
    // the latest state (the child) always wins the exit race. This is what makes "materialization
    // works forever" true no matter how long bob idles.
    assert_eq!(
        mercurylib::utils::get_blockheight(&branch[0]).unwrap_or(u32::MAX),
        0,
        "bob's exit branch must be locktime-0 (always spendable, always ahead of the sender's backup)"
    );
    let split_tx = parse_tx(&branch[0].tx)?;
    let root = split_tx.input[0].previous_output;

    let tip_yr2 = mine_and_sync(&cc, &core, initlock + 500)?;
    bob.claim().await?;
    assert_eq!(token_balance(&bob, &asset).await?, 250, "bob's received tokens are NOT lost after long inactivity");
    // Non-load-bearing note: the piece also carries a deposit-style backup row, whose absolute
    // locktime is long past. It is irrelevant either way — bob's exit is the locktime-0 branch above,
    // which is spendable at ANY height.
    let bob_l2 = coin_of(&cc, "sdk32_bob", &bob_piece).await?.locktime.unwrap_or(0);
    println!("SDK32 - (B) after another ~{} idle blocks (tip={tip_yr2}) bob still holds 250 {asset}; his piece's plain backup row sits at L={bob_l2}, but his real exit is the locktime-0 colored branch", initlock + 500);

    // A single 1,500-sat received piece is below the carrier floor → cannot be re-sent alone.
    let resend = bob.transfer_tokens(&asset, &carol_addr, 100).await;
    assert!(resend.is_err(), "a lone 1,500-sat received piece is too small to re-send (hold / combine / exit)");
    println!("SDK32 - (B) a lone received piece can't be re-sent (below the carrier floor): {:?}", resend.err().map(|e| e.to_string()));

    // UNILATERAL materialization works forever: the branch is locktime-0. Broadcast it → the tokens
    // settle on-chain (the root is still unspent because alice — the sender — did not claw back).
    assert!(!is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "the shared root is still unspent (honest sender)");
    for row in &branch {
        cc.electrum_client.transaction_broadcast_raw(&hex::decode(&row.tx)?)?;
    }
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "materializing spends the shared root");
    for _ in 0..20 {
        let _ = bob.claim().await?;
        if token_balance(&bob, &asset).await? == 250 {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(token_balance(&bob, &asset).await?, 250, "250 units materialize on-chain (unilateral exit preserves the tokens)");
    println!("SDK32 - (B) UNILATERAL materialization works after a year: bob broadcast the locktime-0 branch, spent the root, and settled 250 {asset} on-chain — tokens preserved without the SE");

    // ===== (C) the residual clawback WINDOW for received tokens =================================
    // V2 closes the SE half of this and only the SE half. The colored split set the carrier's spend
    // budget to 1 (tokens.rs), so the carrier is now TERMINAL at the SE: alice can never obtain a
    // FRESH co-signature to sweep the shared root (this is exactly the property bob's receiver-side
    // `verify_terminal_parents` demanded before booking the piece). What V2 does NOT remove is her ONE
    // pre-signed deposit backup: TES-R deliberately never covers a carrier (terminal-freeze), so that
    // backup is still a plain absolute-locktime tx and it matured long ago. A MALICIOUS sender could
    // therefore have swept the root before bob materialized — clawing the tokens back. bob was safe
    // because he exited, and he could exit at ANY height (his branch is locktime-0). This is why a
    // received holder must materialize before the root deadline.
    let (sig_budget, finalized, terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &carrier_id).await?;
    assert!(
        terminal,
        "the carrier must be TERMINAL at the SE after its one colored split (budget {sig_budget:?}, finalized {finalized}) — the sender must never be able to obtain a FRESH co-signed sweep of the shared root"
    );
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk32_alice", &carrier_id).await?.is_none(),
        "the carrier stays terminal-frozen (no TES-R ladder) — which is precisely why the sender's pre-signed deposit backup remains the residual clawback vector"
    );
    let alice_backup = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk32_alice", &carrier_id).await?;
    let alice_bk_lock = alice_backup.first().map(|b| mercurylib::utils::get_blockheight(b).unwrap_or(0)).unwrap_or(0);
    let tip_c = tip(&cc)?;
    assert!(alice_bk_lock <= tip_c, "the SENDER's carrier backup matured (L0={alice_bk_lock} <= tip={tip_c}) — clawback was possible");
    println!("SDK32 - (C) CLAWBACK WINDOW (residual under V2): the carrier is TERMINAL at the SE (budget {sig_budget:?}, finalized {finalized} — no fresh co-signed sweep possible) and un-laddered, but the sender's single pre-signed deposit backup matured at {alice_bk_lock} (tip {tip_c}); a malicious sender could have swept the root had bob stayed idle — received tokens MUST be materialized before the root deadline (the background auto_exit_due pass does this only for a wallet running the watcher)");

    println!("SDK32 - SUCCESS: tokens are NEVER LOST by inactivity on V2. ISSUED tokens (flat carrier): the carrier is terminal-frozen — never laddered, so it never ages — and stays fully sendable via the colored-split path, which no passage of time can block; no clawback risk (no ancestor), but a plain unilateral exit would destroy the allocation and is refused. RECEIVED tokens (colored sub-coin): also terminal-frozen; unilateral materialization works forever because the exit branch is locktime-0, and it preserves the allocation as long as the shared root is unspent — but a lone piece can't be re-sent. Residual risk: V2 makes the carrier TERMINAL at the SE (no fresh co-signed sweep), yet the sender's one pre-signed deposit backup still matures, so a received holder should exit promptly. Cooperative operations (with the SE) work throughout.");
    Ok(())
}
