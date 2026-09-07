//! TV01 — the PREVIOUS owner cannot take a transferred coin back; the CURRENT owner can exit
//! unilaterally. On the ONE coin shape.
//!
//! This file used to prove that property with the flat backup chain: wallet1 sent the coin, so its
//! retained backup was stale and the node refused it as `non-final` (absolute locktime), while
//! wallet2's backup broadcast fine 990 blocks later once its own locktime had passed. That chain
//! no longer exists. A coin's only exit material is its TES-R ladder, and a transfer replaces the
//! sender's owner state `S_0` with a receiver-paying state `S'` at a STRICTLY LOWER relative
//! timelock (BIP-68), disclosing `S_0` to the receiver as superseded. Nothing on the coin carries
//! an absolute calendar, and the legacy `broadcast_backup_tx` entry point refuses the coin by name
//! for BOTH parties.
//!
//! What is measured, on the live stack:
//!   * the deposit ladders the coin (3 co-signs, no flat row, `locktime == None`); the transfer
//!     co-signs exactly ONE more tier (`S'`) and no per-hop backup — the sender still has zero
//!     flat rows afterwards;
//!   * the RECEIVED ladder exits to wallet2's own key, its census balances with the flat term ZERO
//!     (`num_sigs == 4 == 3 tiers + 1 superseded`), and the sender's `S_0` is disclosed in it at
//!     a strictly HIGHER CSV than wallet2's `S'`;
//!   * `broadcast_backup_tx::execute` refuses the coin by name for the previous owner AND for the
//!     current one — there is no flat backup on either side;
//!   * ON CHAIN: after `T` and `X_0` confirm, wallet2's `S'` is refused as `non-BIP68-final` one
//!     block early and accepted at its CSV — at which height the previous owner's `S_0` is still
//!     refused as `non-BIP68-final`. Wallet2's funds land at wallet2's key;
//!   * once `S'` has confirmed, `S_0` can NEVER confirm, even after its own CSV has elapsed: the
//!     previous owner is harmless for the life of the chain, not merely until a calendar runs out.
//!
//! Run: the legacy sequence (tb01..tv01) on the regtest stack.

use std::{env, process::Command, thread, time::Duration};

use anyhow::{anyhow, Result, Ok};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus, Wallet};

use crate::{bitcoin_core, electrs};
use crate::sdk40_tesr_consensus::{broadcast, is_outpoint_spent, mine, se_num_sigs, tx_exists, wait_for_address};

/// The FLAT backup rows under the bare statechain id. `None` (no row) and `Some(empty)` are both
/// zero; a failed READ is an error, never zero.
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

/// The legacy flat-backup broadcast must refuse a laddered coin BY NAME, whoever holds it.
async fn assert_flat_broadcast_refused(client_config: &ClientConfig, wallet_name: &str, statechain_id: &str, to: &str, who: &str) -> Result<()> {
    let result = mercuryrustlib::broadcast_backup_tx::execute(client_config, wallet_name, statechain_id, Some(to.to_string()), None).await;
    assert!(
        result.is_err(),
        "TV01 - broadcast_backup_tx for the {who} ({wallet_name}) must be REFUSED: SC={statechain_id} has no flat \
         backup, its exit is the ladder. It was accepted."
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("there is no flat backup transaction to broadcast"),
        "TV01 - the {who}'s refusal must be BY NAME (the ladder is the coin's exit), not a missing-row accident: {msg}"
    );
    println!("TV01 - broadcast_backup_tx refused for the {who} ({wallet_name}) by name: {msg}");
    Ok(())
}

async fn w1_transfer_to_w2(client_config: &ClientConfig, wallet1: &Wallet, wallet2: &Wallet) -> Result<()> {

    let amount = 1000;

    let token_response = mercuryrustlib::deposit::get_token(client_config).await?;

    let token_id = crate::utils::handle_token_response(client_config, &token_response).await?;

    let deposit_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(&client_config, &wallet1.name, &token_id, amount).await?;

    let _ = bitcoin_core::sendtoaddress(amount, &deposit_address)?;

    let core_wallet_address = bitcoin_core::getnewaddress()?;
    let remaining_blocks = client_config.confirmation_target;
    let _ = bitcoin_core::generatetoaddress(remaining_blocks, &core_wallet_address)?;

    // It appears that Electrs takes a few seconds to index the transaction
    let mut is_tx_indexed = false;

    while !is_tx_indexed {
        is_tx_indexed = electrs::check_address(client_config, &deposit_address, amount).await?;
        thread::sleep(Duration::from_secs(1));
    }

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet1.name).await?;
    let wallet1: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet1.name).await?;
    let new_coin = wallet1.coins.iter().find(|&coin| coin.aggregated_address == Some(deposit_address.clone()) && coin.status == CoinStatus::CONFIRMED).unwrap();
    let statechain_id = new_coin.statechain_id.as_ref().unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);

    // The deposit laddered the coin: T, X_0, S_0 — three co-signs, no flat row, no calendar.
    let deposit_ladder = mercuryrustlib::tesr::load(client_config, &wallet1.name, statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TV01 - the deposit booked SC={statechain_id} CONFIRMED with NO `tesr-` ladder row"))?;
    assert_eq!(se_num_sigs(client_config, statechain_id).await?, 3, "TV01 - a fresh deposit is exactly T + X_0 + S_0 on the enclave (no tx1)");
    assert_eq!(flat_backup_rows(client_config, &wallet1.name, statechain_id).await?, 0, "TV01 - no flat backup row at deposit");
    assert!(new_coin.locktime.is_none(), "TV01 - a laddered coin has no absolute calendar");
    let s0 = deposit_ladder.current().state.clone();
    let csv_old = s0.csv.ok_or_else(|| anyhow!("S_0 declares no CSV"))?;
    let f_txid = new_coin.utxo_txid.clone().ok_or_else(|| anyhow!("coin has no F txid"))?;
    let f_vout = new_coin.utxo_vout.ok_or_else(|| anyhow!("coin has no F vout"))?;

    println!("TV01 - wallet1 deposited SC={} ({} sats): ladder T={} S_0 csv {csv_old}, num_sigs 3, no flat row", &statechain_id[..8], amount, &deposit_ladder.trigger.txid[..8]);

    let wallet2_transfer_adress = mercuryrustlib::transfer_receiver::new_transfer_address(&client_config, &wallet2.name).await?;

    let batch_id = None;
    let force_send = false;

    let result = mercuryrustlib::transfer_sender::execute(&client_config, &wallet2_transfer_adress, &wallet1.name, &statechain_id, None, force_send, batch_id).await;
    assert!(
        result.is_ok(),
        "TV01 - the setup send must succeed: wallet1 -> wallet2 for SC={statechain_id}. Everything \
         this test proves about stale-state defence depends on the transfer having happened. \
         Failed with: {:?}",
        result.as_ref().err()
    );

    // The conveyance co-signed exactly ONE tier — the receiver-paying S' — and no per-hop backup.
    assert_eq!(
        se_num_sigs(client_config, statechain_id).await?, 4,
        "TV01 - one hop is exactly one more co-sign (S'): 3 + 1. A fifth would be a per-hop backup the census cannot account for."
    );
    assert_eq!(
        flat_backup_rows(client_config, &wallet1.name, statechain_id).await?, 0,
        "TV01 - the sender must hold NO flat backup after conveying"
    );
    let retained = mercuryrustlib::tesr::load(client_config, &wallet1.name, statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TV01 - the sender's retained ladder row vanished on conveyance"))?;
    assert_eq!(retained.current().state.txid, s0.txid, "TV01 - the sender's retained live state is still its own S_0");
    assert_eq!(retained.conveyed_states.len(), 1, "TV01 - the sender records the one S' it handed out");

    let transfer_receive_result = mercuryrustlib::transfer_receiver::execute(&client_config, &wallet2.name).await?;
    let received_statechain_ids = transfer_receive_result.received_statechain_ids;

    assert!(received_statechain_ids.contains(&statechain_id.to_string()));
    assert!(received_statechain_ids.len() == 1);

    mercuryrustlib::coin_status::update_coins(&client_config, &wallet2.name).await?;
    let local_wallet_2: mercuryrustlib::Wallet = mercuryrustlib::sqlite_manager::get_wallet(&client_config.pool, &wallet2.name).await?;
    let new_w2_coin = local_wallet_2.coins.iter().find(|&coin| coin.statechain_id == Some(statechain_id.clone())).unwrap();

    assert!(new_coin.status == CoinStatus::CONFIRMED);
    assert!(new_w2_coin.status == CoinStatus::CONFIRMED);

    // THE RECEIVED LADDER: exits to wallet2's key, census balances with the flat term ZERO, and the
    // previous owner's S_0 is DISCLOSED in it at a strictly higher CSV than wallet2's own S'.
    let received = mercuryrustlib::tesr::load(client_config, &wallet2.name, statechain_id)
        .await?
        .ok_or_else(|| anyhow!("TV01 - wallet2 booked SC={statechain_id} with NO `tesr-` ladder row — it has no exit"))?;
    assert_eq!(received.trigger.txid, deposit_ladder.trigger.txid, "TV01 - one T over F, conveyed, not a second one");
    assert_eq!(received.f_txid, f_txid, "TV01 - the received ladder is rooted at the coin's own F");
    assert_eq!(received.f_vout, f_vout, "TV01 - the received ladder is rooted at the coin's own F");
    assert!(new_w2_coin.locktime.is_none(), "TV01 - the received coin carries no calendar: locktime must be None");
    assert_eq!(flat_backup_rows(client_config, &wallet2.name, statechain_id).await?, 0, "TV01 - the receiver holds no flat backup row");
    let num_sigs = se_num_sigs(client_config, statechain_id).await?;
    mercuryrustlib::tesr::verify_bundle(&received, num_sigs, 0)
        .map_err(|e| anyhow!("TV01 - the received ladder must pass the census with the flat term ZERO (num_sigs {num_sigs}): {e}"))?;
    let s_new = received.current().state.clone();
    let csv_new = s_new.csv.ok_or_else(|| anyhow!("S' declares no CSV"))?;
    let disclosed_old = received.superseded_states.iter()
        .find(|s| s.txid == s0.txid)
        .ok_or_else(|| anyhow!("TV01 - the sender's S_0 must be DISCLOSED to the receiver as a superseded state — hiding it is exactly what the census exists to catch"))?;
    assert_eq!(disclosed_old.csv, Some(csv_old), "TV01 - the disclosed S_0 carries its real CSV");
    assert!(
        csv_new < csv_old,
        "TV01 - the receiver's S' (CSV {csv_new}) must mature STRICTLY BEFORE the previous owner's S_0 (CSV {csv_old}); \
         otherwise the previous owner could take the coin back"
    );
    let receiver_key = received.owner_exit_address.clone();
    assert_eq!(
        receiver_key, new_w2_coin.backup_address,
        "TV01 - Model A: the received ladder must exit to wallet2's OWN seed-derived key"
    );
    let x0 = received.current().extension.clone();
    let csv_e = x0.csv.ok_or_else(|| anyhow!("X_0 declares no CSV"))?;

    println!("TV01 - wallet2 received SC={}: ladder exits to {}, S' csv {csv_new} < previous owner's S_0 csv {csv_old} (disclosed), census balances at num_sigs {num_sigs} with flat term 0", &statechain_id[..8], &receiver_key[..12.min(receiver_key.len())]);

    // THE SECURITY PROPERTY OF THIS FILE, PART 1: neither party has a flat backup to broadcast.
    // wallet1 SENT the coin and wallet2 HOLDS it; the legacy flat broadcast refuses both by name.
    assert_flat_broadcast_refused(client_config, &wallet1.name, statechain_id, &core_wallet_address, "PREVIOUS owner").await?;
    assert_flat_broadcast_refused(client_config, &wallet2.name, statechain_id, &core_wallet_address, "CURRENT owner").await?;

    // PART 2, ON CHAIN: relative timelocks decide the race. Start the clock with T, walk to X_0.
    assert!(!is_outpoint_spent(client_config, &f_txid, f_vout), "TV01 - nothing is on chain yet: F unspent, no clock running");
    let _ = broadcast(client_config, &received.trigger.signed_tx)?;
    mine(1)?;
    assert!(tx_exists(client_config, &received.trigger.txid), "TV01 - T must confirm");
    let early_x = refusal_of(client_config, &x0.signed_tx)
        .ok_or_else(|| anyhow!("TV01 - X_0 was ACCEPTED with 1 confirmation of T; it needs {csv_e} (BIP-68)"))?;
    assert!(early_x.contains("non-BIP68-final"), "TV01 - X_0's early refusal must be the relative timelock: {early_x}");
    mine((csv_e - 1) as u32)?;
    let _ = broadcast(client_config, &x0.signed_tx)?;
    mine(1)?;
    assert!(tx_exists(client_config, &x0.txid), "TV01 - X_0 must confirm once T has {csv_e} confirmations");
    println!("TV01 - T and X_0 confirmed (X_0 refused at 1 conf, accepted at {csv_e})");

    // X_0 has 1 confirmation. One block before S' matures, S' is refused — by the timelock.
    if csv_new > 2 {
        mine((csv_new - 2) as u32)?;
    }
    let early_s = refusal_of(client_config, &s_new.signed_tx)
        .ok_or_else(|| anyhow!("TV01 - the CURRENT owner's S' was ACCEPTED one block before its CSV {csv_new}"))?;
    assert!(early_s.contains("non-BIP68-final"), "TV01 - S' early refusal must be the relative timelock: {early_s}");
    mine(1)?;

    // X_0 now has exactly csv_new confirmations. The PREVIOUS owner's stale S_0 must be refused
    // here, and specifically by BIP-68 — that is what makes the previous owner harmless. Any other
    // error means the broadcast was blocked by something incidental and the timelock was never
    // tested at all.
    let stale = refusal_of(client_config, &s0.signed_tx)
        .ok_or_else(|| anyhow!(
            "TV01 - THE STALE-STATE DEFENCE FAILED: the PREVIOUS owner's S_0 (CSV {csv_old}) was ACCEPTED at \
             {csv_new} confirmations of X_0 — wallet1 already sent this coin to wallet2. This is a theft path."
        ))?;
    assert!(
        stale.contains("non-BIP68-final"),
        "TV01 - the stale S_0 was refused, but NOT as `non-BIP68-final`. Only a relative-timelock refusal proves \
         the defence; an unreachable backend or a malformed tx would be refused too and tell us nothing. Got: {stale}"
    );
    println!("TV01 - previous owner's S_0 (csv {csv_old}) refused at {csv_new} confs of X_0: {stale}");

    // The other half of the property: the CURRENT owner's state broadcasts the moment its CSV is
    // met. Without this the test would be satisfied by a system that simply refuses everything.
    let _ = broadcast(client_config, &s_new.signed_tx)
        .map_err(|e| anyhow!("TV01 - the CURRENT owner's S' must be ACCEPTED at exactly {csv_new} confirmations of X_0 — if this fails, unilateral exit is broken for the legitimate owner: {e:#}"))?;
    mine(1)?;
    assert!(tx_exists(client_config, &s_new.txid), "TV01 - wallet2's S' must confirm");
    assert!(is_outpoint_spent(client_config, &x0.txid, x0.payload_vout), "TV01 - X_0's payload output consumed by wallet2's S'");
    wait_for_address(client_config, &receiver_key, s_new.out_value as u32)
        .await
        .map_err(|e| anyhow!("TV01 - the exit funds must land at the CURRENT owner's key {receiver_key}: {e}"))?;
    println!("TV01 - wallet2's S' confirmed; {} sat landed at wallet2's key", s_new.out_value);

    // PART 3: the previous owner is dead for the life of the chain, not until a calendar runs out.
    // Run S_0's own CSV out completely and it is still refused: its prevout is gone.
    mine(csv_old as u32)?;
    let dead = refusal_of(client_config, &s0.signed_tx)
        .ok_or_else(|| anyhow!("TV01 - the PREVIOUS owner's S_0 CONFIRMED after its CSV elapsed — the stale state won the outpoint"))?;
    println!("TV01 - previous owner's S_0 still refused after its full CSV ({csv_old} blocks): {dead}");

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

    w1_transfer_to_w2(&client_config, &wallet1, &wallet2).await?;

    println!("TV01 - Result as reported: the previous owner's state is beaten by relative timelock and then dead forever; the current owner exits through the ladder; no flat backup on either side.");

    Ok(())
}
