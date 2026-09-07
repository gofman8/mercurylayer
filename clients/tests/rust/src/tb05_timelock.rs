//! TB05 — the pending-transfer lock, cancellation, and the STALE-STATE DEFENCE on the ONE coin
//! shape.
//!
//! This file used to end by broadcasting the previous owner's flat backup, watching the node refuse
//! it as `non-final` until its ABSOLUTE locktime, and then watching it confirm once the calendar
//! ran out: the stale-state defence was a chain of absolute-locktime backups decrementing by
//! `interval` per hop. There is no such backup any more. A coin's only exit material is its TES-R
//! ladder — `T → X_0 → S` — established at first sight of the deposit, and its stale states are
//! defended by RELATIVE timelocks (BIP-68): every state the owner ever replaced is disclosed at a
//! STRICTLY HIGHER CSV than the live one, so the live state matures first and wins the outpoint.
//! Nothing on the coin ever matures on its own — `coin.locktime` is `None` for life, there is no
//! `<sid>` flat backup row, and the legacy `broadcast_backup_tx` entry point refuses the coin by
//! name.
//!
//! Steps [1]–[9] are the lock and cancellation semantics, unchanged in substance:
//!   * a coin with an outstanding conveyed co-sign cannot be conveyed again (the local rival-state
//!     guard, `refuse_outstanding_conveyance`, fires BEFORE any SE call — sdk85 [2] pins the same
//!     refusal; the coordinator's "coin has an open transfer" lock is behind it and is what fires
//!     when a transfer was opened without a co-sign, sdk85 [1]);
//!   * a self-addressed transfer can be cancelled by the wallet that holds both keys; cancelling
//!     twice is idempotent; a sender-only cancel of a transfer conveyed to SOMEONE ELSE is refused by
//!     name and leaves the lock in place.
//!
//! Steps [10]–[13] measure the defence that replaced the calendar, on the coin exactly as [1]–[9]
//! leave it — conveyed to wallet2 (unclaimed), with a cancelled self-transfer in its history:
//!   * [10] wallet1 holds ZERO flat backup rows and `locktime == None`; `broadcast_backup_tx` is
//!     refused by name;
//!   * [11] the retained ladder's census balances with the flat term ZERO
//!     (`num_sigs == tiers + superseded + conveyed`), and the conveyed recipient state `S'` sits at a
//!     strictly LOWER CSV than EVERY rival over the same outpoint — the original owner state, the
//!     cancelled self-transfer's state, and the owner's post-cancel replacement;
//!   * [12] ON CHAIN: after `T` and `X_0` confirm, `S'` is refused as `non-BIP68-final` one block
//!     early and accepted at its CSV — while every stale state is still refused as
//!     `non-BIP68-final` at that height. The recipient's funds land at the recipient's key;
//!   * [13] once `S'` has confirmed, no stale state can EVER confirm, even after every one of their
//!     CSVs has elapsed: their prevout is gone.
//!
//! Run: the legacy sequence (tb01..tv01) on the regtest stack.

use std::{env, process::Command, thread, time::Duration};

use anyhow::{anyhow, Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};
use crate::sdk40_tesr_consensus::{broadcast, is_outpoint_spent, mine, se_num_sigs, tx_exists, wait_for_address};

/// The FLAT backup rows under the bare statechain id — where `tx1` and every per-hop backup used to
/// live. `None` (no row) and `Some(empty)` are both zero; a failed READ is an error, never zero.
async fn flat_backup_rows(client_config: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&client_config.pool, wallet_name, statechain_id)
        .await?
        .map(|rows| rows.len())
        .unwrap_or(0))
}

/// Broadcast a signed tier and return the node's refusal text, or `None` if it was accepted.
fn refusal_of(client_config: &ClientConfig, signed_tx: &str) -> Option<String> {
    broadcast(client_config, signed_tx).err().map(|e| format!("{e:#}"))
}

pub async fn old_state_broadcasted(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let amount = 10000;

    // Create first deposit address

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let deposit_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    let _ = bitcoin_core::sendtoaddress(amount, &deposit_address)?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    println!("TB05 - [1] deposit funded: {} sats sent to {} and {} blocks mined; waiting for electrs to index", amount, &deposit_address, remaining_blocks);

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    println!("TB05 - [1] electrs indexed the deposit of {} sats at {}", amount, &deposit_address);

    let wallet1_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet1.name).await?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::CONFIRMED).unwrap();
    let statechain_id_1 = new_coin.statechain_id.as_ref().unwrap();

    // The deposit pass laddered the coin: three co-signs, a ladder row, no flat row, no calendar.
    let deposit_ladder = mercuryrustlib::tesr::load(client_config, &wallet1.name, statechain_id_1)
        .await?
        .ok_or_else(|| anyhow!("TB05 - [2] the deposit booked SC={statechain_id_1} CONFIRMED with NO `tesr-` ladder row — a coin without a ladder has no exit material"))?;
    assert_eq!(se_num_sigs(client_config, statechain_id_1).await?, 3, "TB05 - [2] a fresh deposit is exactly T + X_0 + S_0 on the enclave (no tx1)");
    assert_eq!(flat_backup_rows(client_config, &wallet1.name, statechain_id_1).await?, 0, "TB05 - [2] no flat backup row at deposit");
    assert!(new_coin.locktime.is_none(), "TB05 - [2] a laddered coin has no absolute calendar");

    println!("TB05 - [2] wallet1 booked CONFIRMED coin SC={} ({} sats) from the deposit; ladder T={} already signed, num_sigs 3", &statechain_id_1[..8], amount, &deposit_ladder.trigger.txid[..8]);

    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet1_transfer_adress, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;

    assert!(result.is_ok(), "transfer_sender::execute failed: {:?}", result.as_ref().err());

    println!("TB05 - [3] CONVEY #1: wallet1 conveyed SC={} to its OWN transfer address (never claimed) -> open transfer", &statechain_id_1[..8]);

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    // The coin now has an OPEN transfer (to wallet1's own address, never claimed). Conveying it
    // co-signed a receiver-paying state S' and recorded it in the retained ladder as an OUTSTANDING
    // conveyed state, so a second conveyance is refused LOCALLY, by the rival-state guard, before
    // any SE call: building another state now would produce a rival that ties with or loses to the
    // one already handed out, and neither would be disclosed to the next receiver. That guard is
    // load-bearing — it is what stops a still-owner sender from co-signing a rival state while a
    // conveyed recipient holds claimable material — so assert it is really there before asserting
    // it can be released. (The coordinator's own "coin has an open transfer" lock sits BEHIND it and
    // is what fires when a transfer was opened without a co-sign — sdk85 [1] pins that one.)
    let blocked = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;

    assert!(blocked.is_err(), "a coin with an outstanding conveyed state must not be conveyable again");
    let blocked_msg = format!("{:#}", blocked.unwrap_err());
    assert!(
        blocked_msg.contains("outstanding conveyed state"),
        "expected the local rival-state guard (refuse_outstanding_conveyance) to refuse; got: {blocked_msg}"
    );
    // ...and it refused BEFORE co-signing anything: still exactly the deposit's three plus the one
    // conveyed S'.
    assert_eq!(
        se_num_sigs(client_config, statechain_id_1).await?, 4,
        "TB05 - [4] the refused second convey must not have reached the SE: 3 tiers + 1 conveyed S'"
    );

    println!("TB05 - [4] rival-state guard HELD: second convey of SC={} to wallet2 refused locally (outstanding conveyed state), no co-sign spent", &statechain_id_1[..8]);

    // Cancel the abandoned transfer. It WAS conveyed (transfer_sender posts the mailbox message), so
    // the recorded recipient must co-sign the release — and here wallet1 is that recipient, because
    // it addressed the transfer to itself. This is not a sender-only cancel; it is the same consent
    // rule, satisfied by a wallet that holds both keys.
    let cancel_result = mercuryrustlib::transfer_sender::cancel(&client_config, &wallet1.name, &statechain_id_1.clone()).await;

    assert!(cancel_result.is_ok(), "transfer_sender::cancel failed: {:?}", cancel_result.as_ref().err());
    assert_eq!(
        cancel_result.unwrap(),
        mercuryrustlib::transfer_sender::CancelOutcome::Cancelled
    );

    println!("TB05 - [5] CANCEL #1 (self-addressed transfer, wallet1 is both sender and recipient): SC={} -> Cancelled", &statechain_id_1[..8]);

    // Cancelling twice is idempotent, not an error: cancellation is irreversible, so a client that
    // retries after a dropped response must not be told something different the second time.
    let again = mercuryrustlib::transfer_sender::cancel(&client_config, &wallet1.name, &statechain_id_1.clone()).await;
    assert!(again.is_ok(), "a repeated cancel must be idempotent: {:?}", again.as_ref().err());
    assert_eq!(
        again.unwrap(),
        mercuryrustlib::transfer_sender::CancelOutcome::AlreadyCancelled
    );

    println!("TB05 - [6] CANCEL #2 (idempotent retry of the same cancel): SC={} -> AlreadyCancelled", &statechain_id_1[..8]);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;

    // The lock is released: the coin is spendable again. The cancellation folded the orphaned S'
    // into the ladder's DISCLOSED superseded states and co-signed a replacement owner state one rung
    // below it, so this conveyance builds wallet2's S' one rung below THAT — every rival is still
    // disclosed, and the new recipient cannot tie with the cancelled one.
    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;

    assert!(result.is_ok(), "transfer_sender::execute failed: {:?}", result.as_ref().err());

    println!("TB05 - [7] lock RELEASED by the cancel: CONVEY #2 sent SC={} to WALLET2 (unclaimed)", &statechain_id_1[..8]);

    // ADVERSARIAL, on the live stack: the coin is now conveyed to WALLET2, which has not claimed it.
    // wallet1 must not be able to take it back — it does not hold wallet2's recipient key, and
    // without that key nothing distinguishes "wallet2 has not downloaded the message" from "wallet2
    // downloaded it and is about to claim". A sender-only cancel here is exactly the two-victim
    // break (convey to Bob, cancel, convey to Carol), so it must be refused BY NAME.
    let refused = mercuryrustlib::transfer_sender::cancel(&client_config, &wallet1.name, &statechain_id_1.clone()).await;

    assert!(refused.is_err(), "a sender must NOT be able to cancel a transfer conveyed to someone else");
    let refused_msg = refused.unwrap_err().to_string();
    assert!(
        refused_msg.contains("the recipient must co-sign the cancellation"),
        "expected the recipient-consent refusal; got: {refused_msg}"
    );

    println!("TB05 - [8] CANCEL #3 (sender-only, transfer conveyed to wallet2) REFUSED for SC={} with \"{}\"", &statechain_id_1[..8], refused_msg);

    // ... and the refusal left the transfer intact: the coin is still locked, so wallet1 cannot
    // co-sign a rival state while wallet2 holds claimable material.
    let wallet1_second_address = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet1.name).await?;
    let still_locked = mercuryrustlib::transfer_sender::execute(&client_config, &wallet1_second_address, &wallet1.name, &statechain_id_1.clone(), None, force_send, None).await;
    assert!(still_locked.is_err(), "a refused cancel must leave the pending-transfer lock in place");
    let still_locked_msg = format!("{:#}", still_locked.unwrap_err());
    assert!(
        still_locked_msg.contains("outstanding conveyed state"),
        "TB05 - [9] the re-convey must be refused by the rival-state guard, not incidentally: {still_locked_msg}"
    );

    println!("TB05 - [9] refused cancel left the lock INTACT: wallet1 still cannot re-convey SC={} to itself", &statechain_id_1[..8]);

    // =============================================================================================
    // [10] THERE IS NO FLAT BACKUP TO BROADCAST. The rows the old [10]-[12] read do not exist, the
    // coin carries no calendar, and the legacy entry point refuses the coin by name.
    // =============================================================================================
    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let flat = flat_backup_rows(client_config, &wallet1.name, statechain_id_1).await?;
    assert_eq!(
        flat, 0,
        "TB05 - [10] wallet1 holds {flat} flat backup row(s) for SC={statechain_id_1}. There is no flat \
         absolute-locktime backup on any coin: none at deposit, none at either conveyance, none at \
         the cancellation."
    );
    let owner_coin = wallet1.coins.iter()
        .find(|c| c.statechain_id.as_deref() == Some(statechain_id_1.as_str()) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("wallet1 lost its coin record"))?;
    assert!(owner_coin.locktime.is_none(), "TB05 - [10] coin.locktime must be None for life, got {:?}", owner_coin.locktime);
    let legacy = mercuryrustlib::broadcast_backup_tx::execute(client_config, &wallet1.name, statechain_id_1, Some(core_wallet_address.clone()), None).await;
    assert!(legacy.is_err(), "TB05 - [10] broadcast_backup_tx must refuse a laddered coin: there is nothing flat to broadcast");
    let legacy_msg = format!("{:#}", legacy.unwrap_err());
    assert!(
        legacy_msg.contains("there is no flat backup transaction to broadcast"),
        "TB05 - [10] the refusal must be BY NAME (the ladder is the coin's exit), not a missing-row accident: {legacy_msg}"
    );

    println!("TB05 - [10] wallet1: 0 flat rows, locktime None; broadcast_backup_tx refused by name: {legacy_msg}");

    // =============================================================================================
    // [11] THE LADDER'S OWN RECORD OF EVERY STATE IT EVER CO-SIGNED. The retained bundle carries the
    // live owner state, every superseded state (the original S_0 and the cancelled self-transfer's
    // S'), and the OUTSTANDING conveyed state S'_w2 wallet2 now holds. The census balances with the
    // flat term ZERO, and S'_w2 sits strictly below every rival over the same outpoint.
    // =============================================================================================
    let retained = mercuryrustlib::tesr::load(client_config, &wallet1.name, statechain_id_1)
        .await?
        .ok_or_else(|| anyhow!("TB05 - [11] wallet1's retained ladder row vanished"))?;
    assert_eq!(retained.trigger.txid, deposit_ladder.trigger.txid, "TB05 - [11] one T over F, for the life of the coin");
    assert_eq!(retained.levels.len(), 1, "TB05 - [11] no rollover happened: one level");
    let num_sigs = se_num_sigs(client_config, statechain_id_1).await?;
    let accounted = retained.exit_tiers().len()
        + retained.superseded_states.len()
        + retained.superseded_extensions.len()
        + retained.conveyed_states.len();
    assert_eq!(
        num_sigs as usize, accounted,
        "TB05 - [11] the census must balance with the flat term ZERO: num_sigs {num_sigs} vs {} live tiers + {} \
         superseded states + {} superseded extensions + {} outstanding conveyed states — every enclave \
         co-sign is a disclosed ladder tier, none is a flat backup",
        retained.exit_tiers().len(), retained.superseded_states.len(), retained.superseded_extensions.len(), retained.conveyed_states.len()
    );
    assert_eq!(retained.conveyed_states.len(), 1, "TB05 - [11] exactly one outstanding conveyance: wallet2's S'");
    assert_eq!(
        retained.superseded_states.len(), 2,
        "TB05 - [11] two disclosed superseded states: the original S_0 and the cancelled self-transfer's S'"
    );
    let trigger = retained.trigger.clone();
    let x0 = retained.current().extension.clone();
    let csv_e = x0.csv.ok_or_else(|| anyhow!("X_0 has no CSV"))?;
    let conveyed = retained.conveyed_states[0].clone();
    let csv_conveyed = conveyed.csv.ok_or_else(|| anyhow!("the conveyed S' declares no CSV"))?;
    let mut stale: Vec<(String, mercuryrustlib::tesr::TesrTier)> = Vec::new();
    stale.push(("owner's post-cancel replacement state".to_string(), retained.current().state.clone()));
    for (j, s) in retained.superseded_states.iter().enumerate() {
        stale.push((format!("superseded state {j}"), s.clone()));
    }
    let mut max_stale_csv: u16 = 0;
    for (what, s) in stale.iter() {
        let csv = s.csv.ok_or_else(|| anyhow!("{what} declares no CSV"))?;
        assert!(
            csv_conveyed < csv,
            "TB05 - [11] the conveyed recipient state (CSV {csv_conveyed}) must sit STRICTLY BELOW the {what} \
             (CSV {csv}) over the same outpoint — otherwise a stale state could mature first and take the coin back"
        );
        max_stale_csv = max_stale_csv.max(csv);
    }
    let recipient_key = mercurylib::tesr::payee_address(&wallet2_transfer_adress, "regtest").map_err(|e| anyhow!("{e:?}"))?;

    println!(
        "TB05 - [11] census balances at num_sigs {num_sigs} (flat term 0); X_0 csv {csv_e}; wallet2's S' csv {csv_conveyed} < every stale state {:?}",
        stale.iter().map(|(w, s)| format!("{w}: {:?}", s.csv)).collect::<Vec<_>>()
    );

    // =============================================================================================
    // [12] ON CHAIN: relative timelocks are the defence. Start the clock by broadcasting T, walk to
    // X_0, and watch the node enforce BIP-68 on every state: S'_w2 is refused ONE block early and
    // accepted at its CSV, at which height every stale state is still `non-BIP68-final`.
    // =============================================================================================
    let f_txid = owner_coin.utxo_txid.clone().ok_or_else(|| anyhow!("coin has no F txid"))?;
    let f_vout = owner_coin.utxo_vout.ok_or_else(|| anyhow!("coin has no F vout"))?;
    assert!(!is_outpoint_spent(client_config, &f_txid, f_vout), "TB05 - [12] nothing is on chain yet: F unspent, no clock running");
    let _ = broadcast(client_config, &trigger.signed_tx)?;
    mine(1)?;
    assert!(tx_exists(client_config, &trigger.txid), "TB05 - [12] T must confirm");
    let early_x = refusal_of(client_config, &x0.signed_tx)
        .ok_or_else(|| anyhow!("TB05 - [12] X_0 was ACCEPTED with 1 confirmation of T; it needs {csv_e} (BIP-68)"))?;
    assert!(early_x.contains("non-BIP68-final"), "TB05 - [12] X_0's early refusal must be the relative timelock, not an unrelated error: {early_x}");
    mine((csv_e - 1) as u32)?;
    let _ = broadcast(client_config, &x0.signed_tx)?;
    mine(1)?;
    assert!(tx_exists(client_config, &x0.txid), "TB05 - [12] X_0 must confirm once T has {csv_e} confirmations");
    println!("TB05 - [12] T and X_0 confirmed; X_0 was refused at 1 conf ({early_x}) and accepted at {csv_e}");

    // X_0 has 1 confirmation. Bring it to csv_conveyed - 1: one block early, S'_w2 is refused.
    if csv_conveyed > 2 {
        mine((csv_conveyed - 2) as u32)?;
    }
    let early_s = refusal_of(client_config, &conveyed.signed_tx)
        .ok_or_else(|| anyhow!("TB05 - [12] wallet2's S' was ACCEPTED one block before its CSV {csv_conveyed}"))?;
    assert!(early_s.contains("non-BIP68-final"), "TB05 - [12] S' early refusal must be the relative timelock: {early_s}");
    mine(1)?;
    // X_0 now has exactly csv_conveyed confirmations: the recipient's state matures; every stale
    // state, at a strictly higher CSV, is still refused by the node — that is the whole defence.
    for (what, s) in stale.iter() {
        let r = refusal_of(client_config, &s.signed_tx)
            .ok_or_else(|| anyhow!(
                "TB05 - [12] THE STALE-STATE DEFENCE FAILED: the {what} (CSV {:?}) was ACCEPTED at {csv_conveyed} \
                 confirmations of X_0 — a stale state could race the recipient", s.csv
            ))?;
        assert!(
            r.contains("non-BIP68-final"),
            "TB05 - [12] the {what} must be refused by the RELATIVE TIMELOCK — only that proves the defence; a dead \
             backend refuses everything and proves nothing. Got: {r}"
        );
        println!("TB05 - [12] {what} (csv {:?}) refused at {csv_conveyed} confs of X_0: {r}", s.csv);
    }
    let _ = broadcast(client_config, &conveyed.signed_tx)
        .map_err(|e| anyhow!("TB05 - [12] wallet2's S' must be ACCEPTED at exactly {csv_conveyed} confirmations of X_0: {e:#}"))?;
    mine(1)?;
    assert!(tx_exists(client_config, &conveyed.txid), "TB05 - [12] wallet2's S' must confirm");
    assert!(is_outpoint_spent(client_config, &x0.txid, x0.payload_vout), "TB05 - [12] X_0's payload output consumed by wallet2's S'");
    wait_for_address(client_config, &recipient_key, conveyed.out_value as u32)
        .await
        .map_err(|e| anyhow!("TB05 - [12] the recipient's funds must land at the RECIPIENT's key {recipient_key}: {e}"))?;

    println!("TB05 - [12] wallet2's S' accepted at csv {csv_conveyed}, confirmed, {} sat landed at wallet2's key {}", conveyed.out_value, &recipient_key[..12.min(recipient_key.len())]);

    // =============================================================================================
    // [13] AND THE STALE STATES ARE DEAD FOREVER — not merely late. Run every one of their CSVs out
    // and they are still refused: their prevout is gone. Under the old calendar the previous owner's
    // backup CONFIRMED once its locktime passed; under the ladder it never can.
    // =============================================================================================
    mine(max_stale_csv as u32)?;
    for (what, s) in stale.iter() {
        let r = refusal_of(client_config, &s.signed_tx)
            .ok_or_else(|| anyhow!("TB05 - [13] the {what} CONFIRMED after its CSV elapsed — the stale state won the outpoint"))?;
        println!("TB05 - [13] {what} still refused after {max_stale_csv} more blocks (prevout spent): {r}");
    }

    Ok(())

}

pub async fn execute() -> Result<()> {

    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output().expect("failed to execute process");

    env::set_var("ML_NETWORK", "regtest");

    let client_config = mercuryrustlib::client_config::load().await;

    let wallet1 = mercuryrustlib::wallet::create_wallet(
        "wallet1",
        &client_config).await?;

    mercuryrustlib::sqlite_manager::insert_wallet(&client_config.pool, &wallet1).await?;

    let wallet2 = mercuryrustlib::wallet::create_wallet(
        "wallet2",
        &client_config).await?;

    mercuryrustlib::sqlite_manager::insert_wallet(&client_config.pool, &wallet2).await?;

    old_state_broadcasted(&client_config, &wallet1, &wallet2).await?;

    println!("TB05 - Timelock tests completed successfully: relative-timelock stale-state defence on the ladder; no flat backup, no calendar");

    Ok(())
}
