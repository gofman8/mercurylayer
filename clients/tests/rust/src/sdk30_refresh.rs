//! E2E (refresh / re-anchor): move a laddered (TES-R) coin to a FRESH on-chain aggregate with ONE tx.
//!
//! Under TES-R a coin's exit is a chain of RELATIVE-timelock (CSV) tiers hanging off an
//! **un-broadcast** trigger, so an idle coin never ages: nothing is on-chain, nothing decays with
//! wall-clock time, and the ladder costs 0 vB of rent. `refresh` is therefore not a "deadline reset"
//! any more — it is the **re-anchor** primitive: the coin's funding outpoint `F` is cooperatively
//! spent into a brand-new aggregate, which mints a new statechain id and a brand-new ladder, and
//! permanently kills every exit right rooted at the old `F` (they become a double-spend of a spent
//! input). It is also the documented escape hatch for a coin that must leave its current ladder.
//! This proves the **user-pays** mode (fee deducted from the coin).
//!
//! (a) IDLE COINS NEVER AGE, then RE-ANCHOR: alice deposits 50k; `claim()` auto-establishes its
//!     TES-R ladder. Mine 300 blocks: the tip rises 300 and NOTHING about the coin changes — the
//!     exit chain (txids + relative CSVs) is byte-identical, `F` is still unspent (0 vB rent), and
//!     the balance is untouched. Then `refresh` re-anchors it: the old outpoint is spent (old coin
//!     WITHDRAWN, all prior exit rights dead), a NEW statechain id is minted with its OWN fresh
//!     ladder, and the new amount is 50k − fee (fee = 112 sats at 1 sat/vB).
//! (b) STILL SPENDABLE: alice transfers the whole re-anchored coin to bob (exact amount ⇒ the
//!     whole-coin `S'` handover, no split) and bob claims it (balances 0 / new_amount).
//! (c) OPERATOR PAYS (off-chain rebate): a fresh laddered coin is re-anchored via
//!     `refresh_sponsored`, where a funded operator wallet reimburses the fee OFF-CHAIN. The user's
//!     TOTAL balance is preserved (re-anchored `amount − fee` + a `fee` rebate = the original); the
//!     operator bore the on-chain cost. The blind SE holds no funds, so "operator pays" is realized
//!     as this rebate. The operator's rebate is a non-exact spend of a laddered coin, so it is
//!     routed through the IN-LADDER split (`in_ladder_pay`): the piece is conveyed to the user and
//!     the operator keeps the tier-fee-reduced change. Its size is the SMALLEST off-chain-payable
//!     in-ladder child (`min_child_value` = 1306 sat at 2 sat/vB), not the backup-chain `fee + dust`
//!     442 that bounds an un-laddered piece.
//!
//! Run: SDK_E2E=30 ML_NETWORK=regtest cargo run

use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::is_outpoint_spent;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// Deposit `amount` to `w`, mine, and poll claim until the wallet holds the CONFIRMED coin.
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

/// The wallet's coin with a given statechain id (any status).
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

/// Poll `claim()` until the coin's TES-R ladder is persisted — a deposit auto-establishes it
/// during the same claim pass that confirms the coin, so this normally returns on the first look.
async fn wait_ladder(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    id: &str,
) -> Result<mercuryrustlib::tesr::TesrBundle> {
    for _ in 0..30 {
        if let Some(b) = mercuryrustlib::tesr::load(cc, wallet_name, id).await? {
            return Ok(b);
        }
        let _ = w.claim().await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("coin {id} in {wallet_name} never established a TES-R ladder"))
}

/// The full exit chain as (txid, relative-CSV) pairs — the thing that must NOT move with the tip.
fn ladder_fingerprint(b: &mercuryrustlib::tesr::TesrBundle) -> Vec<(String, Option<u16>)> {
    b.exit_tiers().iter().map(|t| (t.txid.clone(), t.csv)).collect()
}

fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

pub async fn execute() -> Result<()> {
    // Runs on TES-R: every deposit auto-establishes a relative-timelock exit ladder
    // at claim(), so `refresh` here re-anchors a LADDERED root coin — the coin's whole exit chain is
    // discarded with its old funding outpoint and rebuilt over the fresh one.
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in [
        "./rgb-data-sdk30_alice",
        "./rgb-data-sdk30_bob",
        "./rgb-data-sdk30_carol",
        "./rgb-data-sdk30_operator",
    ] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    // ===== (a) IDLE COINS NEVER AGE, THEN RE-ANCHOR ==============================================
    let coin0 = deposit_confirmed_coin(&cc, &alice, "sdk30_alice", 50_000).await?;
    let id0 = coin0.statechain_id.clone().ok_or_else(|| anyhow!("deposit has no id"))?;
    let f_txid = coin0.utxo_txid.clone().ok_or_else(|| anyhow!("deposit coin has no funding txid"))?;
    let f_vout = coin0.utxo_vout.ok_or_else(|| anyhow!("deposit coin has no funding vout"))?;
    let bundle0 = wait_ladder(&cc, &alice, "sdk30_alice", &id0).await?;
    let chain0 = ladder_fingerprint(&bundle0);
    let tip0 = tip(&cc)?;
    println!(
        "SDK30 - deposited 50k: coin {id0} with a TES-R ladder ({} tiers, un-broadcast), tip={tip0}",
        chain0.len()
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "a resting laddered coin publishes NOTHING — its funding outpoint F must be unspent"
    );

    // Time passes: mine 300 blocks. Under the pre-TES-R design this ate 300 blocks of the coin's
    // absolute-locktime headroom; under TES-R the tiers are RELATIVE (CSV) and un-broadcast, so the
    // coin does not age.
    bitcoin_core::generatetoaddress(300, &core)?;
    // The electrum index can lag bitcoind by a moment; wait for it to see the new tip.
    let mut tip_aged = tip(&cc)?;
    for _ in 0..30 {
        if tip_aged >= tip0 + 300 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        tip_aged = tip(&cc)?;
    }
    assert!(tip_aged >= tip0 + 300, "300 blocks must have been mined (tip {tip0} -> {tip_aged})");
    let bundle_aged = mercuryrustlib::tesr::load(&cc, "sdk30_alice", &id0)
        .await?
        .ok_or_else(|| anyhow!("the coin lost its ladder while idle"))?;
    assert_eq!(
        ladder_fingerprint(&bundle_aged),
        chain0,
        "TES-R: an IDLE coin never ages — after 300 blocks its exit chain (txids + relative CSVs) is unchanged"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "TES-R: nothing was broadcast while idle — F is still unspent after 300 blocks (0 vB rent)"
    );
    assert_eq!(
        alice.get_balance().await?.available_sats,
        50_000,
        "the idle coin is still whole and spendable after 300 blocks"
    );
    let aged = coin_by_id(&cc, "sdk30_alice", &id0).await?;
    assert_eq!(aged.status, CoinStatus::CONFIRMED, "the idle coin is still CONFIRMED after 300 blocks");
    println!(
        "SDK30 - +300 blocks (tip {tip0} -> {tip_aged}): the SAME coin is UNCHANGED — identical exit chain, F unspent, 50000 sat spendable (idle coins never age)"
    );

    // Re-anchor it (user pays the fee from the coin).
    let res = alice.refresh(&id0, None).await?;
    assert_ne!(res.new_statechain_id, id0, "refresh mints a NEW statechain id");
    assert_eq!(res.old_amount_sats, 50_000);
    assert_eq!(res.fee_sats, 112, "1-in-1-out P2TR at 1 sat/vB = 112 sats");
    assert_eq!(res.new_amount_sats, 50_000 - 112, "refreshed amount = old − fee");
    println!(
        "SDK30 - refresh broadcast: old {id0} -> new {} (amount {} -> {}, fee {}), txid {}",
        res.new_statechain_id, res.old_amount_sats, res.new_amount_sats, res.fee_sats, res.refresh_txid
    );

    // Confirm the re-anchor tx + let the watcher register the fresh coin.
    let mut new_coin = None;
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, &core)?;
        let _ = alice.claim().await?;
        let c = coin_by_id(&cc, "sdk30_alice", &res.new_statechain_id).await;
        if let Ok(c) = c {
            if c.status == CoinStatus::CONFIRMED {
                new_coin = Some(c);
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let new_coin = new_coin.ok_or_else(|| anyhow!("refreshed coin did not confirm"))?;

    // The re-anchored coin is a full-fledged laddered coin: claim() established its OWN fresh
    // ladder over the NEW funding outpoint. (There is no "headroom" to restore under TES-R — the old chain is
    // simply gone with the old outpoint, and this one starts from scratch.)
    let bundle_new = wait_ladder(&cc, &alice, "sdk30_alice", &res.new_statechain_id).await?;
    assert_ne!(
        ladder_fingerprint(&bundle_new),
        chain0,
        "the re-anchored coin runs a BRAND-NEW exit chain, not the old one"
    );
    assert_eq!(
        bundle_new.f_txid, res.refresh_txid,
        "the fresh ladder is rooted at the re-anchor tx (the new funding outpoint)"
    );
    assert_eq!(
        new_coin.amount,
        Some(50_000 - 112),
        "the re-anchored coin holds old − fee"
    );

    // The old coin is spent on-chain (WITHDRAWN); the refresh tx is confirmed and spends coin0's
    // funding outpoint — every exit right rooted at the old F (its trigger/tiers AND its
    // absolute-locktime backup tx) is now a double-spend of a spent input (dead).
    let old = coin_by_id(&cc, "sdk30_alice", &id0).await?;
    assert_eq!(old.status, CoinStatus::WITHDRAWN, "the old coin is spent on-chain");
    let rtxid = res.refresh_txid.parse::<electrum_client::bitcoin::Txid>()?;
    let rtx = cc.electrum_client.transaction_get(&rtxid)?;
    let spends_old = rtx.input.iter().any(|i| {
        i.previous_output.txid.to_string() == old.utxo_txid.clone().unwrap_or_default()
            && Some(i.previous_output.vout) == old.utxo_vout
    });
    assert!(spends_old, "the refresh tx must spend the OLD coin's funding outpoint");
    assert!(
        is_outpoint_spent(&cc, &f_txid, f_vout),
        "the OLD funding outpoint is now spent — the old ladder's trigger can never be mined again"
    );
    println!(
        "ECON refresh old_amount=50000 new_amount={} fee=112 refresh_vb={} old_tiers={} new_tiers={}",
        res.new_amount_sats,
        rtx.vsize(),
        chain0.len(),
        ladder_fingerprint(&bundle_new).len()
    );
    println!(
        "SDK30 - RE-ANCHOR: old coin spent (WITHDRAWN, outpoint gone → every exit right rooted at the old F is dead); fresh coin carries a brand-new TES-R ladder over the re-anchor tx"
    );

    // ===== (b) STILL SPENDABLE ===================================================================
    let new_amount = res.new_amount_sats;
    let r = alice.transfer(&bob_addr, new_amount).await?;
    assert_eq!(r.total_sats, new_amount, "the whole refreshed coin transfers");
    assert!(!r.used_split, "an exact whole-coin amount uses the S' handover, not a split");
    let mut claimed = 0u32;
    for _ in 0..60 {
        claimed += bob.claim().await?.claimed_transfers;
        if claimed >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(bob.get_balance().await?.available_sats, new_amount, "bob received the refreshed coin");
    assert_eq!(alice.get_balance().await?.available_sats, 0, "alice spent the refreshed coin");
    println!("SDK30 - STILL SPENDABLE: refreshed coin transferred to bob ({new_amount} sats), balances 0 / {new_amount}");

    // ===== (c) OPERATOR PAYS (off-chain rebate) ==================================================
    // carol is the user; a funded operator wallet reimburses the on-chain fee off-chain.
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_carol"), None).await?;
    let (operator, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_operator"), None).await?;
    let c0 = deposit_confirmed_coin(&cc, &carol, "sdk30_carol", 40_000).await?;
    let cid = c0.statechain_id.clone().ok_or_else(|| anyhow!("carol deposit has no id"))?;
    // carol's coin must be LADDERED — this is the case that matters: re-anchoring a live laddered
    // root coin.
    let _ = wait_ladder(&cc, &carol, "sdk30_carol", &cid).await?;
    // Fund the operator so it can rebate off-chain.
    let op_coin = deposit_confirmed_coin(&cc, &operator, "sdk30_operator", 10_000).await?;
    let op_sid = op_coin.statechain_id.clone().ok_or_else(|| anyhow!("operator deposit has no id"))?;
    let op_bundle = wait_ladder(&cc, &operator, "sdk30_operator", &op_sid).await?;

    let sp = carol.refresh_sponsored(&cid, &operator, None).await?;
    assert_eq!(sp.fee_sats, 112);
    assert_eq!(sp.new_amount_sats, 40_000 - 112);
    // The sub-dust 112-sat fee can't be rebated exactly off-chain. TWO floors bound the smallest
    // payable rebate and the LARGER binds: the backup-chain floor of an un-laddered piece
    // (dust 330 + backup fee 112 = 442) and, because the operator's coin is LADDERED,
    // `min_child_value` — the rebate is paid by an in-ladder split whose child funds its own
    // extension + state tier (each committed_fee + P2A) before clearing dust: 2*(248+240)+330 = 1306
    // at the default 2 sat/vB. Sizing the rebate at 442 made `refresh_sponsored` fail outright with
    // FeeTooHigh once the operator's coin is laddered.
    let min_payable = mercurylib::tesr::min_child_value(
        mercurylib::tesr::TesrParams::for_network("regtest").committed_fee_rate,
        330,
    );
    assert_eq!(min_payable, 1_306, "the minimum payable in-ladder child at 2 sat/vB");
    assert_eq!(
        sp.rebate_sats, min_payable,
        "operator rebates the smallest OFF-CHAIN-PAYABLE amount ≥ fee (the in-ladder child floor)"
    );
    assert!(sp.rebate_sats >= sp.fee_sats, "the rebate must at least cover the fee the user paid");
    // Confirm the re-anchor + claim the fresh coin AND the incoming rebate.
    let expected_total = 40_000u64 - 112 + sp.rebate_sats;
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, &core)?;
        let _ = carol.claim().await?;
        let reanchored = coin_by_id(&cc, "sdk30_carol", &sp.new_statechain_id)
            .await
            .map(|c| c.status == CoinStatus::CONFIRMED)
            .unwrap_or(false);
        if reanchored && carol.get_balance().await?.available_sats == expected_total {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // The user is made MORE than whole: refreshed (40_000 − 112) + rebate ≥ 40_000.
    let carol_total = carol.get_balance().await?.available_sats;
    assert_eq!(carol_total, expected_total, "refreshed + rebate");
    assert!(carol_total >= 40_000, "operator-paid refresh leaves the user at least whole");
    // The operator bore the on-chain cost. The rebate is a NON-EXACT spend of the
    // operator's laddered 10_000 coin, so it goes through the in-ladder split: `SP` spends
    // `X_m.out[0]` and pays the rebate piece to carol plus the change back to the operator, so the
    // operator keeps exactly `tier_out_total(X_m.out[0], 2) − rebate` — the rebate AND the split tier's
    // committed fee/anchor both come out of the operator's own funds.
    let op_split_total = mercurylib::tesr::tier_out_total(
        op_bundle.current().extension.out_value,
        2,
        op_bundle.fee_rate,
    )
    .ok_or_else(|| anyhow!("operator coin too small to carry an in-ladder split"))?;
    let expected_op_change = op_split_total
        .checked_sub(sp.rebate_sats)
        .ok_or_else(|| anyhow!("operator coin too small to fund the rebate"))?;
    assert_eq!(
        operator.get_balance().await?.available_sats,
        expected_op_change,
        "the operator paid the rebate ({}) + the in-ladder split's tier cost out of its own funds",
        sp.rebate_sats
    );
    let operator_paid = 10_000 - expected_op_change;
    assert!(
        operator_paid >= sp.rebate_sats,
        "the operator's balance must fall by at least the rebate it paid ({operator_paid} < {})",
        sp.rebate_sats
    );
    println!(
        "ECON refresh_sponsored old_amount=40000 new_amount={} fee=112 rebate={} carol_total={carol_total} operator_paid={operator_paid}",
        sp.new_amount_sats, sp.rebate_sats
    );
    println!(
        "SDK30 - OPERATOR PAYS: carol re-anchored; operator rebated the fee off-chain via an in-ladder split piece ({} sat = the smallest off-chain-payable in-ladder child) — carol total {carol_total} ≥ 40_000, operator bore the cost ({operator_paid} sat)",
        sp.rebate_sats
    );

    println!(
        "SDK30 - SUCCESS: on TES-R an IDLE coin never ages — 300 blocks left its un-broadcast exit chain byte-identical, its funding outpoint unspent (0 vB rent) and its 50000 sat spendable. `refresh` is the RE-ANCHOR primitive: ONE on-chain tx spends the old outpoint (WITHDRAWN → every exit right rooted at the old F is permanently dead) and mints a new statechain id carrying a brand-new ladder over the re-anchor tx, which stays an ordinary spendable coin (50000 → {}, handed whole to bob). TWO fee models proven: (1) USER PAYS — the fee comes from the coin; (2) OPERATOR PAYS — a funded sponsor rebates the fee off-chain through an in-ladder split piece sized to the smallest payable in-ladder child so the user ends ≥ whole (40000 → {carol_total}); the operator bore the cost. The blind SE never touches funds in either.",
        res.new_amount_sats
    );
    Ok(())
}
