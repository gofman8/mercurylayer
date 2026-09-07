//! E2E (security, honest about the trust floor): the **fresh double-sign** case — a *malicious SE*
//! that co-signs a second, conflicting spend of a coin's funding UTXO `F`.
//!
//! Under TES-R a coin's ladder hangs off a **trigger** `T` that spends `F` and is itself
//! *locktime-free*: it carries no absolute locktime and no CSV, because the relative timelocks live
//! on the extension/state tiers hanging BELOW it, and an idle coin never ages (nothing is broadcast,
//! so no clock runs). A freshly SE-co-signed RIVAL trigger over the same `F` is therefore exactly as
//! final as the owner's own — the CSV tier chain breaks no tie and gives NO advantage. The contest
//! degrades to a plain on-chain race (first-seen / highest-fee wins). The ladder shape changed from
//! the pre-TES-R design; this floor did not.
//!
//! We model the malicious SE with a NORMAL (non-single-use, no-budget) coin, for which the honest SE
//! already permits re-signing — `server/src/endpoints/sign.rs`'s single-use / spend-budget /
//! epoch-deadline gates simply do not fire — so obtaining two conflicting co-signatures of one coin
//! exercises exactly the code path a gate-ignoring SE would take. The point: two valid conflicting
//! spends of `F` exist, and only the one that confirms first wins. This is the irreducible single-SE
//! trust floor that neither the ladder (the TES-R CSV tiers — the coin's ONLY exit material; there
//! is no absolute-locktime backup chain before, beside or after it any more), single_use, nor the
//! terminal/budget query can close (only threshold signing can).
//!
//! The coin is laddered by the SDK's `claim()` at the FIRST MEMPOOL SIGHTING of `F`
//! (`update_coins_ex(.., Defer)` books it `IN_MEMPOOL`; the same pass's establish loop signs
//! T → X_0 → S_0). Before mounting the attack this test pins that shape, because it is the premise
//! the attack is measured against: the coin is `IN_MEMPOOL` AND has its `tesr-<sid>` row, the
//! enclave count is EXACTLY 3 (no `tx1`), there is no `<sid>` flat row and no `locktime`, and the
//! exported watch bundle already lists the coin as an event-watch on `F` (liveness allowlist L1 admits
//! `IN_MEMPOOL`). Confirmation ADOPTS that ladder (`num_sigs` still 3), and the honest census
//! `verify_bundle(b, 3, 0)` balances. After the two rival co-signs `num_sigs == 5` and that same
//! census FAILS — which is how a receiver detects the SE's hidden rivals. Detection, not prevention:
//! on chain the two triggers are a plain race.
//!
//! Not to be confused with sdk12, which proves the INVERSE property: the SE cannot double-sign behind
//! the receiver's back (nonce atomicity). Here the SE is *willing*, and nothing client-side stops it.
//!
//! Run: SDK_E2E=15 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{se_num_sigs, wait_for_address};

const NETWORK: &str = "regtest";
/// The two rival triggers must differ as *transactions*. A txid covers no witness data, so two
/// co-signs of the SAME tier bytes would be one and the same tx — they are built at different
/// committed fee rates (and to different destinations) so they are genuinely distinct conflicts.
/// `FEE_X > FEE_Y` also makes the winner unambiguous: the loser is neither first nor a valid RBF.
const FEE_X: f64 = 5.0;
const FEE_Y: f64 = 3.0;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    // No protocol pin: this runs on the real TES-R default. The coin below is laddered by
    // claim() in the pass that first sees its funding tx in the mempool, which is exactly the
    // point — the CSV ladder IS in place from the first moment, the coin never ages while idle, and
    // none of that buys the honest owner anything against a freshly co-signed rival trigger over the
    // same funding UTXO.
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk15_alice"), None).await?;

    // Fund a NORMAL statechain coin (single_use=false, no budget) -> the SE permits re-signing, which
    // is exactly what a malicious SE ignoring its own terminality gates would do for an off-chain node.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(40_000).await?;
    bitcoin_core::sendtoaddress(40_000, &addr)?;

    // --- FIRST SIGHT: F is in the mempool, nothing mined. ONE claim() pass books the coin
    // IN_MEMPOOL and ladders it in the same pass (the SDK lane's counterpart of sdk40's PART 0). ---
    wait_for_address(&cc, &addr, 40_000).await?;
    alice.claim().await?;
    let seen = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk15_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.amount == Some(40_000) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("claim() did not book the 40 000-sat deposit"))?;
    assert_eq!(
        seen.status,
        CoinStatus::IN_MEMPOOL,
        "F is unconfirmed, so the first pass must book the coin IN_MEMPOOL (got {:?})",
        seen.status
    );
    let sid = seen.statechain_id.clone().ok_or_else(|| anyhow!("booked coin has no statechain_id"))?;
    let at_sight = mercuryrustlib::tesr::load(&cc, "sdk15_alice", &sid).await?.ok_or_else(|| {
        anyhow!(
            "claim() booked {sid} IN_MEMPOOL with no `tesr-{sid}` row — the SDK must ladder a deposit in \
             the pass that first sees it; a coin without a ladder has no exit material at all"
        )
    })?;
    assert_eq!(at_sight.exit_tiers().len(), 3, "the at-sight ladder is T → X_0 → S_0");
    assert_eq!(
        se_num_sigs(&cc, &sid).await?,
        3,
        "the enclave count at first sight is EXACTLY the three tiers: no tx1 was co-signed"
    );
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, "sdk15_alice", &sid).await?.is_none(),
        "no `<sid>` flat backup row exists for a laddered deposit"
    );
    assert!(seen.locktime.is_none(), "a laddered coin has no absolute calendar (locktime None), got {:?}", seen.locktime);
    // L1: the wallet's exported watch bundle covers the coin ALREADY — as an event-watch on F with
    // no backup and no height — because the liveness allowlist admits IN_MEMPOOL. A tower handed
    // this bundle now would defend the coin before F has a single confirmation.
    let wb: mercury_utexo_sdk::WatchBundle = serde_json::from_str(&alice.export_watch_bundle().await?)?;
    let entry = wb.entries.iter().find(|e| e.statechain_id == sid).ok_or_else(|| {
        anyhow!("the IN_MEMPOOL coin {sid} is missing from the exported watch bundle — the liveness allowlist must admit a coin from its first mempool sighting")
    })?;
    assert!(entry.trigger.is_some(), "the IN_MEMPOOL coin's watch entry is an event-watch on F (it carries a trigger)");
    assert!(entry.backup_tx.is_none() && entry.backup_locktime.is_none(), "a laddered coin exports NO flat backup");
    assert_eq!(entry.deadline_block, u32::MAX, "a laddered coin has no height deadline");
    println!("SDK15 - {sid} seen IN_MEMPOOL and laddered in the same claim() pass: num_sigs=3, no flat row, no locktime, already in the watch bundle");

    // --- Confirm. Confirmation ADOPTS the ladder signed at sight; it does not sign a second one. ---
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 40_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 { return Err(anyhow!("deposit did not confirm")); }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk15_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid.as_str()) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("coin {sid} vanished from the wallet"))?;
    assert_eq!(coin.status, CoinStatus::CONFIRMED, "F confirmed (got {:?})", coin.status);
    assert!(coin.locktime.is_none(), "locktime stays None for life, got {:?}", coin.locktime);
    let ladder = mercuryrustlib::tesr::load(&cc, "sdk15_alice", &sid)
        .await?
        .ok_or_else(|| anyhow!("the confirmed coin {sid} lost its ladder row"))?;
    assert_eq!(
        ladder.trigger.txid, at_sight.trigger.txid,
        "confirmation must ADOPT the ladder signed at sight, not establish a second one"
    );
    assert_eq!(se_num_sigs(&cc, &sid).await?, 3, "num_sigs is still exactly 3 after confirmation");
    // The honest census balances with the flat term 0 — the premise every receiver relies on.
    mercuryrustlib::tesr::verify_bundle(&ladder, 3, 0)
        .map_err(|e| anyhow!("the honest deposit ladder must pass the census with flat term 0: {e}"))?;
    println!("SDK15 - funded a normal coin {sid} (SE permits re-signing); its TES-R ladder is the one signed at sight and its census balances (3 == 3 + 0)");

    let f_txid = coin.utxo_txid.clone().ok_or_else(|| anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or_else(|| anyhow!("no F vout"))?;
    let f_value = coin.amount.ok_or_else(|| anyhow!("no F value"))? as u64;
    let agg = coin.aggregated_address.clone().ok_or_else(|| anyhow!("no aggregated_address"))?;
    let thief_addr = bitcoin_core::getnewaddress()?;

    // --- The malicious SE blind-co-signs TWO conflicting TRIGGERS over the SAME F -----------------
    // Both spend F, both are locktime-free (a trigger has neither nLockTime nor a CSV — those bind
    // only the tiers beneath it), so neither carries any handicap relative to the other:
    //   tx_X = the owner-shaped trigger paying P2TR(A), fee-bumped (the honest owner's head start).
    //   tx_Y = a thief-shaped trigger paying an address the owner does not control — what a willing
    //          SE, alone or colluding with a previous owner, can always produce.
    let mut c1 = coin.clone();
    let t_x = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg, NETWORK, FEE_X)?;
    let tx_x = mercuryrustlib::tesr::cosign_tier(&cc, &mut c1, t_x.tx_hex.clone(), f_value, NETWORK).await?;
    let mut c2 = coin.clone(); // fresh nonce state
    let t_y = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &thief_addr, NETWORK, FEE_Y)?;
    let tx_y = mercuryrustlib::tesr::cosign_tier(&cc, &mut c2, t_y.tx_hex.clone(), f_value, NETWORK).await?;
    let txx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&tx_x)?)?;
    let txy: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&tx_y)?)?;
    // Both spend the SAME coin outpoint -> they conflict.
    assert_eq!(txx.input[0].previous_output, txy.input[0].previous_output,
        "both fresh co-signs must spend the same coin outpoint (they conflict)");
    // ...and they are genuinely two different transactions, not one tx signed twice.
    assert_ne!(txx.txid(), txy.txid(), "the two fresh co-signs must be distinct transactions");
    println!("SDK15 - the SE produced TWO conflicting fresh TRIGGER co-signs: tx_X {} and tx_Y {} (same input {})",
        txx.txid(), txy.txid(), txx.input[0].previous_output);

    // --- The census SEES them. Every co-sign is counted, and there is no flat term to hide one
    // behind: the honest ladder discloses 3 tiers against 5 issued, so the exact-equality census a
    // receiver runs (`verify_bundle`) now REFUSES it. That is detection — a receiver would not accept
    // this coin — not prevention: on chain the two triggers are still a plain race. ---
    let n = se_num_sigs(&cc, &sid).await?;
    assert_eq!(n, 5, "both rival co-signs are counted: 3 tiers + 2 fresh triggers, no tx1 term");
    assert!(
        mercuryrustlib::tesr::verify_bundle(&ladder, n, 0).is_err(),
        "the honest ladder must FAIL the census once the SE has issued co-signs it does not disclose (3 disclosed vs {n} issued)"
    );
    println!("SDK15 - census after the attack: {n} issued vs 3 disclosed — verify_bundle refuses the honest bundle (a receiver would detect the hidden rivals)");

    // --- On-chain it is a plain race: only the FIRST-seen confirms; the other is rejected ---------
    // Once tx_X is mined the outcome is fee-independent: tx_Y's input simply no longer exists.
    let bx = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&tx_x)?);
    assert!(bx.is_ok(), "first-broadcast conflicting spend is accepted: {bx:?}");
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let by = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&tx_y)?);
    assert!(by.is_err(), "the conflicting spend loses the race (already-spent input): {by:?}");
    println!("SDK15 - tx_X won the race (confirmed); tx_Y REJECTED (its input is already spent). Only ONE of the two co-signed conflicts can settle — and it won only by being first with the higher fee, not by anything the ladder did.");

    println!("SDK15 - SUCCESS (documents the trust floor): a malicious SE CAN produce two conflicting fresh co-signatures of one funding UTXO. On a laddered coin the rival is a TRIGGER, which is locktime-free — the TES-R CSV tier chain constrains only the tiers BELOW a trigger, so two fresh triggers over F start their clocks on equal terms and the ladder gives NO advantage. It is a first-seen/highest-fee RACE. The honest party's only defence is speed + fee (be first / bump via the P2A anchor). This is the irreducible single-SE trust — closed only by threshold-signing the SE, not by any client-side layer (not the TES-R CSV ladder, not single_use, not the terminal/spend-budget query).");
    Ok(())
}
