//! E2E (SDK_E2E=48) — **deposit-time ladder: a fresh deposit is laddered at FIRST MEMPOOL SIGHT**.
//!
//! There is no flat absolute-locktime backup any more. The SDK `claim()` pass books a fresh deposit
//! the moment its funding transaction is seen in the mempool (`update_coins_ex(.., Defer)`) and, in
//! the SAME pass, establishes and persists its plain TES-R ladder (T, X_0, S_0), exiting to the
//! wallet's seed-derived `backup_address`. Nothing exit-related waits for a confirmation, and no
//! `tx1` is co-signed anywhere. Proven here against the live SE + real bitcoind, in the order that
//! can actually fail:
//!   1. the funding tx is broadcast and deliberately NOT mined; the first `claim()` that books the
//!      coin (IN_MEMPOOL / UNCONFIRMED) leaves a `tesr-<id>` row behind in that very pass — the
//!      ladder exists while the coin is still pre-confirmation;
//!   2. the SE's `num_sigs` is exactly 3 at that moment: T + X + S and NOTHING else. A 4 here would
//!      mean a flat `tx1` was co-signed at sight; a 0 would mean the ladder waited for a block;
//!   3. the coin carries ZERO flat backup rows and `coin.locktime == None` — it has no absolute
//!      calendar of any kind;
//!   4. confirmation changes nothing: the bundle is byte-identical (same trigger), `num_sigs`
//!      stays 3 across two further claims (idempotent — no re-establishment at CONFIRMED), R′
//!      `verify_bundle(.., 3, 0)` accepts it with the flat term 0, and the exit payee is the
//!      wallet's own `backup_address` (recoverable — NOT an out-of-wallet key).
//!
//! Run with SDK_E2E=48 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::wallet::Coin;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

const WALLET: &str = "sdk48_alice";

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// Chain tip height, read from electrum. The regtest is SHARED with other harnesses that mine, so
/// "the deposit is still unconfirmed" is only assertable relative to how many blocks appeared
/// between broadcast and sighting.
fn chain_height(cc: &ClientConfig) -> Result<usize> {
    use electrum_client::ElectrumApi;
    Ok(cc.electrum_client.block_headers_subscribe()?.height)
}

/// The number of FLAT backup rows under a coin's bare statechain id. There is no flat backup any
/// more, so the row is normally ABSENT, and absence is ZERO rows (`try_get_backup_txs` reports it as
/// `Ok(None)`, distinct from a failed read). A row that exists is counted, so a `tx1` minted at
/// sight (the thing this test exists to rule out) shows up as 1, never as "not found".
async fn flat_backup_rows(cc: &ClientConfig, wallet: &str, sid: &str) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet, sid)
        .await
        .map_err(|e| anyhow!("flat backup rows for {sid} could not be read: {e}"))?
        .map_or(0, |rows| rows.len()))
}

async fn num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain_info for {sid}"))?
        .num_sigs)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk48_alice");
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest(WALLET), None).await?;
    // (there is no protocol switch — every deposit is laddered, and laddered at sight)

    let amount = 100_000u32;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(amount as u64).await?;

    // ---- 1. Broadcast, do NOT mine, and claim until the coin is SIGHTED. ------------------------
    //
    // `check_deposit` books the coin the first time electrum lists the funding output (height 0 =
    // mempool), and the SAME claim() pass ladders it. Polling is only for electrs's mempool
    // indexing lag; the assertion is on the pass that booked the coin, not on a later one.
    let tip_at_send = chain_height(&cc)?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let mut sighted: Option<Coin> = None;
    for _ in 0..30 {
        alice.claim().await?;
        let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, WALLET).await?.coins;
        if let Some(c) = coins.iter().find(|c| {
            c.aggregated_address.as_deref() == Some(addr.as_str())
                && c.duplicate_index == 0
                && c.status != CoinStatus::INITIALISED
        }) {
            sighted = Some(c.clone());
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let sighted = sighted.ok_or(anyhow!(
        "the un-mined deposit to {addr} was never sighted: claim() left it INITIALISED for 60s. \
         Either electrs is not serving the mempool, or check_deposit no longer books at first sight."
    ))?;
    let sid = sighted.statechain_id.clone().ok_or(anyhow!("sighted coin has no statechain_id"))?;
    let mined_meanwhile = chain_height(&cc)?.saturating_sub(tip_at_send) as u32;

    // The ladder exists in the pass that BOOKED the coin.
    let bundle_at_sight = mercuryrustlib::tesr::load(&cc, WALLET, &sid)
        .await?
        .ok_or(anyhow!(
            "coin {sid} was booked as {:?} but has NO `tesr-` row: the ladder was not established \
             at first sight — the coin has no exit material at all",
            sighted.status
        ))?;
    // …and the coin is still PRE-CONFIRMATION when it gets it. A CONFIRMED status at first sight is
    // tolerated only if some OTHER miner on this shared regtest produced `confirmation_target`
    // blocks in the window — that is not this code's doing and is reported, not asserted on.
    match &sighted.status {
        CoinStatus::IN_MEMPOOL | CoinStatus::UNCONFIRMED => println!(
            "SDK48 - deposit {sid} sighted as {:?} ({mined_meanwhile} block(s) mined meanwhile) and \
             laddered in the SAME pass: {} tiers, trigger {}",
            sighted.status,
            bundle_at_sight.exit_tiers().len(),
            bundle_at_sight.trigger.txid
        ),
        CoinStatus::CONFIRMED if mined_meanwhile >= cc.confirmation_target => println!(
            "SDK48 - NOTICE: an external miner produced {mined_meanwhile} block(s) between \
             broadcast and sighting, so the coin was already CONFIRMED at first sight; the \
             pre-confirmation half of assertion (1) could not be observed this run"
        ),
        other => {
            return Err(anyhow!(
                "deposit {sid} was booked as {other:?} at first sight with only {mined_meanwhile} \
                 block(s) mined since broadcast (confirmation_target {}): the deposit path is \
                 gating on something other than mempool sight",
                cc.confirmation_target
            ))
        }
    }

    // ---- 2. The enclave count at sight is EXACTLY 3: T + X + S, and no tx1. -------------------
    let sigs_at_sight = num_sigs(&cc, &sid).await?;
    assert_eq!(
        sigs_at_sight, 3,
        "num_sigs at first sight must be exactly the 3 tiers (0 flat + 3): a 4 means a flat tx1 was \
         co-signed at deposit, a 0 means the ladder waited for a confirmation"
    );

    // ---- 3. No flat backup, no calendar. -------------------------------------------------------
    let flat_at_sight = flat_backup_rows(&cc, WALLET, &sid).await?;
    assert_eq!(flat_at_sight, 0, "a laddered deposit must carry ZERO flat backup rows at sight");
    assert_eq!(
        sighted.locktime, None,
        "coin.locktime must be None for life — a laddered coin has no absolute calendar"
    );
    println!("SDK48 - at sight: num_sigs=3, 0 flat backup rows, locktime=None");

    // ---- 4. Confirm. Nothing about the exit material may change. -------------------------------
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut confirmed = false;
    for _ in 0..60 {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats >= amount as u64 {
            confirmed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(confirmed, "deposit did not confirm");
    // Two more claims prove idempotency: a CONFIRMED coin that already has a ladder must NOT be
    // re-established (that would spend three more irreversible co-signs and unbalance the census).
    alice.claim().await?;
    alice.claim().await?;

    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, WALLET)
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid.as_str()) && c.duplicate_index == 0)
        .cloned()
        .ok_or(anyhow!("coin {sid} vanished from the wallet"))?;
    assert_eq!(coin.status, CoinStatus::CONFIRMED, "the sighted coin is the one that confirmed");

    let bundle = mercuryrustlib::tesr::load(&cc, WALLET, &sid)
        .await?
        .ok_or(anyhow!("the ladder established at sight vanished after confirmation"))?;
    assert_eq!(
        bundle.trigger.txid, bundle_at_sight.trigger.txid,
        "confirmation must NOT re-establish: the trigger co-signed at sight is the one on disk"
    );
    assert_eq!(bundle.exit_tiers().len(), 3, "T, X_0, S_0");
    assert_eq!(
        bundle.f_txid.as_str(),
        coin.utxo_txid.as_deref().unwrap_or_default(),
        "the ladder is over THIS coin's funding outpoint"
    );
    assert_eq!(bundle.f_vout, coin.utxo_vout.unwrap_or_default());

    // Exit payee is the wallet's own seed-derived backup_address (recoverable from the mnemonic).
    assert_eq!(
        bundle.owner_exit_address, coin.backup_address,
        "ladder exits to the wallet's backup_address, not an external key"
    );

    // Sound sig-count vs the LIVE SE, still exactly 3 after three further claims; R′ accepts it
    // with the flat term 0 — the census is `se_num_sigs == tiers + superseded`, no flat term.
    let sigs = num_sigs(&cc, &sid).await?;
    assert_eq!(sigs, 3, "0 flat + 3 tiers; idempotent (later claims did not re-establish)");
    mercuryrustlib::tesr::verify_bundle(&bundle, sigs, 0)
        .map_err(|e| anyhow!("R′ rejected the deposit-time ladder with flat term 0: {e}"))?;
    assert!(
        mercuryrustlib::tesr::verify_bundle(&bundle, sigs, 1).is_err(),
        "the flat term is ZERO — a census that still counts a phantom tx1 must not balance"
    );

    assert_eq!(
        flat_backup_rows(&cc, WALLET, &sid).await?,
        0,
        "still ZERO flat backup rows after confirmation"
    );
    assert_eq!(coin.locktime, None, "coin.locktime stays None after confirmation");

    println!(
        "SDK48 - ✓ PASS: deposit laddered at FIRST SIGHT — `tesr-` row present pre-confirmation, \
         num_sigs=3 (0 flat + 3 tiers) at sight and after confirmation, 0 flat backup rows, \
         locktime=None, exits to backup_address, R′ verified with flat term 0, idempotent"
    );
    Ok(())
}
