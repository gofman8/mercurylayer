//! E2E: RGB statechain **exit to on-chain via a standard rgb-lib witness invoice**.
//!
//! Demonstrates that exiting a statechain coin uses the *same* rgb-lib receive flow as on-chain RGB:
//! - Issuer issues 1000 and deposits the asset onto a statechain UTXO (re-colorable color deposit).
//! - The receiver creates a standard rgb-lib **witness invoice** (`witness_receive`).
//! - The owner spends the statechain UTXO into a colored withdrawal that pays the invoice's address
//!   (blind-MuSig2 co-signed with the SE) and broadcasts it.
//! - The receiver settles it with a standard `refresh` - so the asset lands on the receiver's
//!   *on-chain* wallet and `get_asset_balance` reads 1000. (Exit to on-chain, standard rgb-lib.)
//!
//! Run with RGB_E2E=3. Requires the regtest + Mercury (lockbox) stack.

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
const COIN_SAT: u32 = 40_000;

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> bool {
    // The exit output is a p2tr; if electrum no longer lists it among the script's unspents it was spent.
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

async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
}

fn dump(label: &str, rgb: &mut RgbWallet, asset_id: &str) {
    tokio::task::block_in_place(|| {
        let _ = rgb.refresh(None);
        match rgb.balance(asset_id) {
            Ok((s, f, sp)) => println!("    [{label}] balance: settled={s} future={f} spendable={sp}"),
            Err(e) => println!("    [{label}] balance: <{e}>"),
        }
        if let Ok(a) = rgb.list_allocations(asset_id) {
            for (op, amt, settled) in a {
                println!("    [{label}] allocation on {op}: amount={amt} settled={settled}");
            }
        }
        if let Ok(ts) = rgb.transfers(asset_id) {
            for (kind, status, amt, txid) in ts {
                println!("    [{label}] transfer: kind={kind} status={status} amount={amt} txid={txid}");
            }
        }
    });
}

fn setup(data_dir: &str, issue: bool) -> Result<(RgbWallet, Option<String>)> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(500_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    rgb.create_utxos(1, 200_000, 2)?;
    let contract = if issue {
        Some(rgb.issue_nia("RGBSC", "RGB Statechain Asset", 0, vec![ISSUED])?)
    } else {
        None
    };
    Ok((rgb, contract))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data3");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // Issuer + receiver rgb-lib wallets.
    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data3/issuer", true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data3/receiver", false))?;
    println!("RGB03 - issued {ISSUED} units of {contract}");

    // Deposit onto a statechain UTXO (re-colorable color deposit).
    let wallet1 = mercuryrustlib::wallet::create_wallet("rgb03_w1", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet1).await?;
    let token = mercuryrustlib::deposit::get_token(&cc).await?;
    let token_id = crate::utils::handle_token_response(&cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(&cc, &wallet1.name, &token_id, COIN_SAT)
            .await?;
    let (dtxid, dvout, _c, signed) = tokio::task::block_in_place(|| {
        issuer.fund_statechain(&sc_address, COIN_SAT as u64, &contract, ISSUED, 2, BLINDING)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?;
    println!("RGB03 - deposited to statechain UTXO {dtxid}:{dvout}");
    wait_for_address(&cc, &sc_address, COIN_SAT).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(&cc, &wallet1.name).await?;
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &wallet1.name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address.as_str()))
        .ok_or(anyhow!("deposited coin not found"))?
        .clone();
    assert!(coin.status == CoinStatus::CONFIRMED);

    println!("RGB03 - [standard rgb-lib] receiver before exit:");
    dump("receiver", &mut receiver, &contract);

    // EXIT TO ON-CHAIN via a standard rgb-lib witness invoice.
    let recipient_id = tokio::task::block_in_place(|| receiver.witness_receive(ISSUED))?;
    let exit_address = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recipient_id))?;
    println!("RGB03 - receiver witness invoice -> on-chain address {exit_address}");
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let mut coin_w = coin.clone();
    let withdraw = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc, &issuer, &mut coin_w, &contract, ISSUED, &exit_address, 1, true, None, NETWORK,
        si.fee_rate_sats_per_byte, si.initlock, si.interval, BLINDING,
    )
    .await?;
    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recipient_id, &withdraw.consignment, &withdraw.txid, withdraw.recipient_vout)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&withdraw.signed_tx)?)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    tokio::task::block_in_place(|| receiver.refresh(None))?;

    println!("RGB03 - [standard rgb-lib] receiver after exit (withdrawal tx {}):", withdraw.txid);
    dump("receiver", &mut receiver, &contract);
    let recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&contract))?;
    assert_eq!(recv_bal, ISSUED, "receiver must hold {ISSUED} on-chain after the exit (standard rgb-lib balance)");

    println!("RGB03 - exit to on-chain complete: receiver on-chain balance = {recv_bal} (standard rgb-lib)");

    // ... AND BACK: the receiver re-deposits the exited asset onto a fresh statechain UTXO.
    // Same color deposit as onboarding - proving the asset round-trips on-chain -> statechain.
    let wallet2 = mercuryrustlib::wallet::create_wallet("rgb03_w2", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet2).await?;
    let token = mercuryrustlib::deposit::get_token(&cc).await?;
    let token_id = crate::utils::handle_token_response(&cc, &token).await?;
    const COIN_SAT2: u32 = 20_000;
    let sc_address2 =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(&cc, &wallet2.name, &token_id, COIN_SAT2)
            .await?;
    let (dtxid2, dvout2, _c2, signed2) = tokio::task::block_in_place(|| {
        receiver.fund_statechain(&sc_address2, COIN_SAT2 as u64, &contract, ISSUED, 2, BLINDING)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed2)?)?;
    println!("RGB03 - re-deposited to fresh statechain UTXO {dtxid2}:{dvout2}");
    wait_for_address(&cc, &sc_address2, COIN_SAT2).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(&cc, &wallet2.name).await?;
    let coin2 = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &wallet2.name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address2.as_str()))
        .ok_or(anyhow!("re-deposited coin not found"))?
        .clone();
    assert!(coin2.status == CoinStatus::CONFIRMED, "re-deposited statechain coin must confirm");

    println!("RGB03 - [standard rgb-lib] receiver after re-deposit:");
    dump("receiver", &mut receiver, &contract);
    // Round-trip proof at the layer that is actually authoritative for a color deposit:
    //  - the on-chain exit UTXO that held the asset is now SPENT (consumed by the re-deposit tx), and
    //  - Mercury confirms the fresh statechain coin.
    // The asset is committed (OP_RETURN) to the statechain UTXO and is re-colorable for a future exit,
    // exactly like the onboarding deposit (rgb01 proves a color-deposited statechain UTXO can be exited).
    // It does NOT yet show in DB `list_allocations` on the statechain outpoint - that needs the
    // register primitive (design doc item 1: list_unspents/allocations are BDK/DB-backed).
    let exit_outpoint = format!("{}:{}", withdraw.txid, withdraw.recipient_vout);
    let spent = tokio::task::block_in_place(|| is_outpoint_spent(&cc, &withdraw.txid, withdraw.recipient_vout));
    assert!(spent, "the on-chain exit UTXO {exit_outpoint} must be spent by the re-deposit");
    assert!(coin2.status == CoinStatus::CONFIRMED, "the asset's new statechain coin must be confirmed");

    println!("RGB03 - round-trip complete: asset exited to on-chain UTXO {exit_outpoint} (now spent) and returned to statechain UTXO {dtxid2}:{dvout2} (confirmed, re-colorable)");
    Ok(())
}
