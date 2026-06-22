//! E2E: **fully off-chain, peer-to-peer RGB transfer** over a statechain (no Bitcoin transaction).
//!
//! This is the "spend the coin to myself with a different OP_RETURN" model: the asset moves to the
//! receiver while the **sats stay with the sender** and **nothing is broadcast**. The receiver
//! accepts the asset by validating the sender's *unbroadcast* exit transaction **off-chain** with the
//! standard rgb-lib offchain validator (`validate_consignment_offchain` / `OffchainResolver`).
//!
//! Flow (each step prints the standard rgb-lib view and asserts an invariant):
//!   1. Sender issues 1000 and deposits it onto its statechain UTXO X; registers X.
//!   2. Receiver onboards its own statechain UTXO Y, registers it, and creates a standard rgb-lib
//!      blinded invoice on Y (`blind_receive` -> recipient_id referencing Y).
//!   3. Sender performs a statechain SELF-transfer of X (spends X to itself, sats stay with the
//!      sender, key-share rotated, lower nLockTime) whose OP_RETURN commits the RGB transition
//!      assigning the asset to the receiver's blinded seal Y. The exit tx is NOT broadcast.
//!   4. Sender hands the receiver the consignment (P2P). The receiver VALIDATES IT OFF-CHAIN against
//!      the unbroadcast exit tx -> valid=true. The asset is now the receiver's off-chain; X is still
//!      unspent on Bitcoin. (Security: the receiver only accepts because the unbroadcast exit's
//!      OP_RETURN commits the asset to its seal.)
//!   5. Optional finalization: broadcasting the exit makes it Bitcoin-confirmed (shown in rgb07).
//!
//! Run with RGB_E2E=8. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, rgb::RgbStatechainStatus, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 31;
const ISSUED: u64 = 1000;
const COIN_SAT_X: u32 = 50_000;
const COIN_SAT_Y: u32 = 30_000;

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

/// Rich standard-rgb-lib state dump for a wallet/asset, printed at each step.
fn dump(tag: &str, rgb: &mut RgbWallet, asset_id: &str) {
    tokio::task::block_in_place(|| {
        match rgb.balance(asset_id) {
            Ok((s, f, sp)) => println!("    [{tag}] get_asset_balance: settled={s} future={f} spendable={sp}"),
            Err(e) => println!("    [{tag}] get_asset_balance: <{e}>"),
        }
        if let Ok(us) = rgb.unspents_dump() {
            for (op, sat, color, exists, nalloc, pending) in us {
                println!("    [{tag}] list_unspents: {op} sats={sat} colorable={color} exists={exists} allocations={nalloc} pending_blinded={pending}");
            }
        }
        if let Ok(a) = rgb.list_allocations(asset_id) {
            for (op, amt, settled) in a {
                println!("    [{tag}] allocation: {op} amount={amt} settled={settled}");
            }
        }
        if let Ok(ts) = rgb.transfers(asset_id) {
            for (kind, status, amt, txid) in ts {
                println!("    [{tag}] list_transfers: kind={kind} status={status} amount={amt} txid={txid}");
            }
        }
    });
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
async fn deposit_coin<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, fund: F) -> Result<(String, String, u32)>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, size_sat).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &wallet.name).await?
        .coins.iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address.as_str()))
        .ok_or(anyhow!("coin not found for {sc_address}"))?.clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "statechain coin must confirm");
    Ok((coin.statechain_id.unwrap(), coin.utxo_txid.unwrap(), coin.utxo_vout.unwrap()))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data8");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut sender, contract) = tokio::task::block_in_place(|| setup("./rgb-data8/sender", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data8/receiver", false, false))?;
    println!("RGB08 - issued {ISSUED} units of {contract}");

    // ---- 1. Sender: deposit asset onto statechain UTXO X, register it. ----
    let sources: Vec<String> = tokio::task::block_in_place(|| sender.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let (sc_x, x_txid, x_vout) = deposit_coin(&cc, "rgb08_sender", COIN_SAT_X, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            sender.fund_statechain(sc, COIN_SAT_X as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    tokio::task::block_in_place(|| sender.register_statechain(&x_txid, x_vout, COIN_SAT_X as u64, &contract, ISSUED, &sources))?;
    println!("RGB08 - [sender] asset on statechain UTXO X = {x_txid}:{x_vout}, X unspent on-chain = {}", !is_outpoint_spent(&cc, &x_txid, x_vout));
    dump("sender", &mut sender, &contract);

    // ---- 2. Receiver: onboard statechain UTXO Y, register it, blind-invoice on Y. ----
    let (_sc_y, y_txid, y_vout) = deposit_coin(&cc, "rgb08_receiver", COIN_SAT_Y, |sc| {
        Ok(bitcoin_core::sendtoaddress(COIN_SAT_Y, sc)?)
    }).await?;
    tokio::task::block_in_place(|| receiver.register_statechain(&y_txid, y_vout, COIN_SAT_Y as u64, &contract, 0, &[]))?;
    let recipient_id = tokio::task::block_in_place(|| receiver.blind_receive(None, ISSUED))?;
    println!("RGB08 - [receiver] onboarded statechain UTXO Y = {y_txid}:{y_vout}; blinded invoice -> {recipient_id}");

    // ---- 3. Sender self-spends X (sats stay with sender) with OP_RETURN -> receiver's seal Y. NOT broadcast. ----
    let r = mercuryrustlib::rgb::refresh_rgb_anchor_self_transfer(
        &cc, &sender, "rgb08_sender", &sc_x, &contract, ISSUED, BLINDING + 1, NETWORK, Some(&recipient_id),
    ).await?;
    println!("RGB08 - off-chain transfer tx {} : X={} tx_n {}->{} nLockTime {}->{} (NOT broadcast)",
        r.new_backup_txid, r.funding_outpoint, r.previous_tx_n, r.new_tx_n, r.previous_nlocktime, r.new_nlocktime);
    assert_eq!(r.status, RgbStatechainStatus::RgbAnchorRefreshAccepted, "self-transfer must complete");
    assert_eq!(r.funding_outpoint, format!("{x_txid}:{x_vout}"), "same funding outpoint X (sats stay put)");
    assert!(r.new_nlocktime < r.previous_nlocktime, "latest state has the lowest nLockTime");
    // INVARIANT: nothing was broadcast - X is still unspent on Bitcoin.
    assert!(!is_outpoint_spent(&cc, &x_txid, x_vout), "off-chain: X must still be UNSPENT (no Bitcoin tx)");
    println!("RGB08 - sender spent X to itself; sats stay with sender; asset committed to receiver's seal via OP_RETURN; X UNSPENT");

    // ---- 4. Receiver validates the unbroadcast exit consignment OFF-CHAIN (standard rgb-lib). ----
    let (valid, detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain(&r.rgb_commitment_consignment, &r.new_backup_txid)
    })?;
    println!("RGB08 - [receiver] validate_consignment_offchain(txid={}) -> valid={valid} detail={:?}", r.new_backup_txid, detail);
    assert!(valid, "receiver must validate the off-chain transfer (unbroadcast exit OP_RETURN commits the asset to Y)");

    // Still nothing on-chain after acceptance - this was a pure off-chain P2P transfer.
    assert!(!is_outpoint_spent(&cc, &x_txid, x_vout), "after off-chain acceptance, X is still UNSPENT");
    println!("RGB08 - SUCCESS: off-chain P2P statechain transfer accepted by the receiver via standard rgb-lib offchain validation; the asset moved to the receiver's statechain UTXO, the sats stayed with the sender, and nothing was broadcast.");
    Ok(())
}
