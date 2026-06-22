//! E2E: a full **statechain -> statechain** RGB transfer via a standard rgb-lib **blinded invoice**.
//!
//! This is the user's exact vision: the receiver creates an rgb invoice whose `recipient_id`
//! references *its own statechain UTXO* (`blind_receive` on the registered statechain coin); the
//! sender spends its statechain UTXO "to itself" and commits the RGB transition in an OP_RETURN
//! (the blind-MuSig2 co-signed unilateral-exit tx); the receiver settles with the standard
//! `refresh`, ending up with the asset on its statechain UTXO. The new color path
//! (`color_blinded` -> `AssetColoringInfo::blinded_map`) is what lets the transition assign to an
//! existing outpoint (the receiver's statechain seal) rather than a fresh witness vout.
//!
//! Run with RGB_E2E=5. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 31;
const ISSUED: u64 = 1000;
const COIN_SAT_A: u32 = 40_000;
const COIN_SAT_B: u32 = 30_000;

async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
}

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> bool {
    use electrum_client::bitcoin::Txid;
    let raw = match cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()) {
        std::result::Result::Ok(r) => r,
        _ => return false,
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk).unwrap_or_default();
    !listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout)
}

fn setup(data_dir: &str, issue: bool, make_utxos: bool) -> Result<(RgbWallet, Option<String>)> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(500_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    if make_utxos {
        rgb.create_utxos(1, 200_000, 2)?;
    }
    let contract = if issue {
        Some(rgb.issue_nia("RGBSC", "RGB Statechain Asset", 0, vec![ISSUED])?)
    } else {
        None
    };
    Ok((rgb, contract))
}

/// Mercury-deposit a statechain coin to `wallet_name` of `size_sat`; return (address, txid, vout).
/// `fund` broadcasts the funding tx and returns its txid.
async fn deposit_coin<F>(
    cc: &ClientConfig,
    wallet_name: &str,
    size_sat: u32,
    fund: F,
) -> Result<(String, String, u32)>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, size_sat)
            .await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok((sc_address, wallet.name, 0))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data5");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data5/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data5/receiver", false, false))?;
    println!("RGB05 - issued {ISSUED} units of {contract}");

    // -------- Sender: deposit the asset onto statechain UTXO A and register it. --------
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter()
        .map(|(op, _, _)| op)
        .collect();
    let (addr_a, w1, _v) = deposit_coin(&cc, "rgb05_sender", COIN_SAT_A, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_A as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        let txid = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?;
        Ok(txid.to_string())
    })
    .await?;
    let mut coin_a = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &w1)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(addr_a.as_str()))
        .ok_or(anyhow!("coin A not found"))?
        .clone();
    assert!(coin_a.status == CoinStatus::CONFIRMED);
    let txid_a = coin_a.utxo_txid.clone().unwrap();
    let vout_a = coin_a.utxo_vout.unwrap();
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_a, vout_a, COIN_SAT_A as u64, &contract, ISSUED, &sources)
    })?;
    let bal_sender = tokio::task::block_in_place(|| issuer.settled_balance(&contract))?;
    println!("RGB05 - sender holds {bal_sender} on statechain UTXO A = {txid_a}:{vout_a}");
    assert_eq!(bal_sender, ISSUED);

    // -------- Receiver: onboard a free statechain UTXO B, register it, and blind_receive on it. --------
    let (addr_b, w2, _vb) = deposit_coin(&cc, "rgb05_receiver", COIN_SAT_B, |sc| {
        Ok(bitcoin_core::sendtoaddress(COIN_SAT_B, sc)?)
    })
    .await?;
    let coin_b = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &w2)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(addr_b.as_str()))
        .ok_or(anyhow!("coin B not found"))?
        .clone();
    let txid_b = coin_b.utxo_txid.clone().unwrap();
    let vout_b = coin_b.utxo_vout.unwrap();
    tokio::task::block_in_place(|| {
        receiver.register_statechain(&txid_b, vout_b, COIN_SAT_B as u64, &contract, 0, &[])
    })?;
    let recipient_id = tokio::task::block_in_place(|| receiver.blind_receive(None, ISSUED))?;
    println!("RGB05 - receiver blinded invoice on its statechain UTXO B = {txid_b}:{vout_b} -> {recipient_id}");

    // -------- Sender: spend A "to itself" + OP_RETURN committing amount -> receiver's blinded seal. --------
    let exit_address = tokio::task::block_in_place(|| issuer.get_address())?;
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let blinded = vec![(recipient_id.clone(), ISSUED)];
    let transfer = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc, &issuer, &mut coin_a, &contract, ISSUED, &exit_address, 1, true, None, NETWORK,
        si.fee_rate_sats_per_byte, si.initlock, si.interval, BLINDING, Some(&blinded),
    )
    .await?;
    println!("RGB05 - sender built unilateral-exit tx {} spending A, OP_RETURN -> receiver seal", transfer.txid);

    // Receiver fetches + settles via the standard refresh flow.
    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recipient_id, &transfer.consignment, &transfer.txid, transfer.recipient_vout)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&transfer.signed_tx)?)?;
    // A blinded transfer settles over successive refreshes as the witness tx gains confirmations:
    // first refresh validates+accepts (allocation becomes "future"), later ones settle it.
    let mut recv_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| receiver.refresh(None))?;
        recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&contract))?;
        if recv_bal == ISSUED {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }

    // -------- Proofs (standard rgb-lib methods). --------
    let recv_allocs = tokio::task::block_in_place(|| receiver.list_allocations(&contract))?;
    for (op, amt, settled) in &recv_allocs {
        println!("RGB05 - [standard rgb-lib] receiver allocation: {op} amount={amt} settled={settled}");
    }
    assert_eq!(recv_bal, ISSUED, "receiver must hold {ISSUED} after the blinded statechain transfer");
    assert!(
        recv_allocs.iter().any(|(op, amt, settled)| op == &format!("{txid_b}:{vout_b}") && *amt == ISSUED && *settled),
        "the asset must land on the receiver's statechain UTXO B {txid_b}:{vout_b}"
    );
    assert!(
        tokio::task::block_in_place(|| is_outpoint_spent(&cc, &txid_a, vout_a)),
        "the sender's statechain UTXO A must be spent on-chain by the transfer"
    );

    println!("RGB05 - SUCCESS: statechain->statechain transfer via a standard rgb-lib blinded invoice; the asset moved onto the receiver's statechain UTXO B and A was consumed.");
    Ok(())
}
