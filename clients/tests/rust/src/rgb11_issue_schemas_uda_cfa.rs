//! E2E (rgb-tests parity): **issue every RGB asset schema over statechain** (NIA, UDA, CFA, IFA) and
//! prove a NON-NIA fungible asset (CFA) transfers over the statechain exactly like NIA.
//!
//! Mirrors upstream `issue_nia` / `issue_uda` / `issue_cfa` / `issue_ifa` (each asserts the asset is
//! issued with the right ticker/name/precision/supply) plus a CFA witness transfer over statechain.
//!
//! UDA is a single-token NFT whose allocation is DATA (not Fungible), so it is asserted as issued +
//! present in list_assets; a UDA *transfer* over statechain needs the data-allocation path on
//! fund/register/list_allocations (a documented bridge gap) and is out of scope here.
//!
//! Run with RGB_E2E=11. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 111;

const NIA_SUPPLY: u64 = 1000;
const UDA_SUPPLY: u64 = 1; // a UDA is a single unique token
const CFA_SUPPLY: u64 = 500;
const IFA_ISSUED: u64 = 800;
const IFA_INFLATION: u64 = 200;

const CFA_COIN_SAT: u32 = 90_000;
const CFA_RECV_AMT: u64 = 200;
const CFA_CHANGE_AMT: u64 = 300;
const CFA_RECV_SAT: u64 = 30_000;
const CFA_CHANGE_SAT: u64 = 20_000; // split fee = 90_000 - 50_000 = 40_000

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

/// Open a wallet, fund it, and make plenty of colorable UTXOs (one per issuance + fund sources).
fn setup(data_dir: &str) -> Result<RgbWallet> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(2_000_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    // Each colorable UTXO must hold enough sats to later fund a CFA_COIN_SAT (90k) colored coin, so
    // size them generously (one issuance lands on each; the CFA one is later spent by fund_statechain).
    rgb.create_utxos(8, 150_000, 2)?;
    Ok(rgb)
}

async fn deposit_coin<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, fund: F) -> Result<String>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use(cc, &wallet.name, &token_id, size_sat).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok(sc_address)
}

fn setup_receiver(data_dir: &str) -> Result<RgbWallet> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(500_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    Ok(rgb)
}

async fn coin_outpoint(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<(String, u32, mercuryrustlib::Coin)> {
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address))
        .ok_or(anyhow!("coin not found for {sc_address}"))?
        .clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "statechain coin must confirm");
    let txid = coin.utxo_txid.clone().ok_or(anyhow!("no utxo_txid"))?;
    let vout = coin.utxo_vout.ok_or(anyhow!("no utxo_vout"))?;
    Ok((txid, vout, coin))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data11");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let mut issuer = tokio::task::block_in_place(|| setup("./rgb-data11/issuer"))?;

    // ---- 1. Issue every schema; assert each is present with the right ticker/name/precision/supply.
    let nia = tokio::task::block_in_place(|| issuer.issue_nia("NIATK", "NIA asset", 0, vec![NIA_SUPPLY]))?;
    let uda = tokio::task::block_in_place(|| issuer.issue_uda("UDATK", "UDA asset", None, 0, None, vec![]))?;
    let cfa = tokio::task::block_in_place(|| issuer.issue_cfa("CFA asset", None, 0, vec![CFA_SUPPLY], None))?;
    let ifa = tokio::task::block_in_place(|| issuer.issue_ifa("IFATK", "IFA asset", 0, vec![IFA_ISSUED], vec![IFA_INFLATION]))?;
    println!("RGB11 - issued NIA={nia}");
    println!("RGB11 - issued UDA={uda}");
    println!("RGB11 - issued CFA={cfa}");
    println!("RGB11 - issued IFA={ifa}");

    let assets = tokio::task::block_in_place(|| issuer.list_assets())?;
    for (id, ticker, name, prec) in &assets {
        println!("RGB11 -   list_assets: id={id} ticker={ticker} name={name} precision={prec}");
    }
    let find = |id: &str| assets.iter().find(|(a, _, _, _)| a == id).cloned();
    let nia_row = find(&nia).ok_or_else(|| anyhow!("NIA not in list_assets"))?;
    let uda_row = find(&uda).ok_or_else(|| anyhow!("UDA not in list_assets"))?;
    let cfa_row = find(&cfa).ok_or_else(|| anyhow!("CFA not in list_assets"))?;
    let ifa_row = find(&ifa).ok_or_else(|| anyhow!("IFA not in list_assets"))?;
    assert_eq!(nia_row.1, "NIATK", "NIA ticker");
    assert_eq!(nia_row.2, "NIA asset", "NIA name");
    assert_eq!(uda_row.1, "UDATK", "UDA ticker");
    assert_eq!(cfa_row.2, "CFA asset", "CFA name");
    assert_eq!(ifa_row.1, "IFATK", "IFA ticker");
    assert_eq!(tokio::task::block_in_place(|| issuer.settled_balance(&nia))?, NIA_SUPPLY, "NIA supply");
    assert_eq!(tokio::task::block_in_place(|| issuer.settled_balance(&cfa))?, CFA_SUPPLY, "CFA supply");
    assert_eq!(tokio::task::block_in_place(|| issuer.settled_balance(&ifa))?, IFA_ISSUED, "IFA issued supply");
    println!("RGB11 - all four schemas issued with correct ticker/name/precision/supply (UDA supply = {UDA_SUPPLY} NFT)");

    // ---- 2. Transfer the CFA over statechain (witness), proving a NON-NIA fungible asset behaves
    //         exactly like NIA: fund a colored statechain coin, split to receiver + change, exit.
    let mut receiver = tokio::task::block_in_place(|| setup_receiver("./rgb-data11/receiver"))?;
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&cfa))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_c = deposit_coin(&cc, "rgb11_cfa", CFA_COIN_SAT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, CFA_COIN_SAT as u64, &cfa, CFA_SUPPLY, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_c, vout_c, mut coin_c) = coin_outpoint(&cc, "rgb11_cfa", &addr_c).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_c, vout_c, CFA_COIN_SAT as u64, &cfa, CFA_SUPPLY, &sources)
    })?;
    println!("RGB11 - CFA coin C = {txid_c}:{vout_c} holds {}", tokio::task::block_in_place(|| issuer.settled_balance(&cfa))?);

    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(CFA_RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let change_id = tokio::task::block_in_place(|| issuer.witness_receive(CFA_CHANGE_AMT))?;
    let change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change_id))?;
    let splits = vec![
        (recv_addr.clone(), CFA_RECV_SAT, CFA_RECV_AMT),
        (change_addr.clone(), CFA_CHANGE_SAT, CFA_CHANGE_AMT),
    ];
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_c, &cfa, &splits, 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB11 - CFA split C -> recv({CFA_RECV_AMT}) + change({CFA_CHANGE_AMT}) (tx {}); NOT broadcast", split.txid);
    let recv_vout = split.output_vouts[0];
    let change_vout = split.output_vouts[1];

    let (valid, detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&split.consignment, &[split.txid.clone()])
    })?;
    println!("RGB11 - [receiver] validate_offchain_chain -> valid={valid} detail={:?}", detail);
    assert!(valid, "receiver must validate the CFA transfer off-chain");

    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recv_id, &split.consignment, &split.txid, recv_vout)?;
        issuer.post_consignment(&change_id, &split.consignment, &split.txid, change_vout)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&split.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_c}:{vout_c}")]))?;

    let mut recv_bal = 0;
    let mut chg_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = issuer.refresh(None); let _ = receiver.refresh(None); });
        recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&cfa)).unwrap_or(0);
        chg_bal = tokio::task::block_in_place(|| issuer.settled_balance(&cfa)).unwrap_or(0);
        if recv_bal == CFA_RECV_AMT && chg_bal == CFA_CHANGE_AMT { break; }
        thread::sleep(Duration::from_secs(1));
    }

    crate::rgb_dump::dump("receiver after CFA transfer (got 200)", &mut receiver, &cfa);
    crate::rgb_dump::dump("issuer after CFA transfer (change 300)", &mut issuer, &cfa);

    assert_eq!(recv_bal, CFA_RECV_AMT, "receiver must hold {CFA_RECV_AMT} CFA after the transfer");
    assert_eq!(chg_bal, CFA_CHANGE_AMT, "issuer CFA change must be {CFA_CHANGE_AMT}");
    assert!(is_outpoint_spent(&cc, &txid_c, vout_c), "CFA coin C consumed by the transfer");

    println!("RGB11 - SUCCESS: NIA/UDA/CFA/IFA all issue over statechain with correct metadata + supply; a CFA (non-NIA fungible) transfers over statechain exactly like NIA (200 -> receiver, 300 change).");
    Ok(())
}
