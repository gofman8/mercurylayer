//! E2E (rgb-tests parity): **contract metadata + IFA supply invariants** for assets issued in the
//! statechain-RGB engine. Mirrors upstream `issue_nia`/`issue_cfa`/`issue_ifa` (which assert schema,
//! ticker, name, precision, and supply) and the IFA inflation-accounting invariant
//! `max_supply == initial_supply + inflation_right`; then realizes the inflation right and shows the
//! circulating supply rise to the cap.
//!
//! Run with RGB_E2E=14. Requires the regtest + rgb-proxy + electrs stack (no Mercury coin funding).

use std::{env, fs, thread, time::Duration};

use anyhow::Result;
use mercury_rgb::RgbWallet;

use crate::bitcoin_core;

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";

const NIA_SUPPLY: u64 = 1000;
const CFA_SUPPLY: u64 = 500;
const IFA_ISSUED: u64 = 800;
const IFA_INFLATION: u64 = 200; // max = 800 + 200 = 1000

fn meta(w: &RgbWallet, id: &str) -> Result<(String, String, String, u8, u64, u64, u64)> {
    tokio::task::block_in_place(|| w.asset_metadata(id))
}

fn setup(data_dir: &str) -> Result<RgbWallet> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(2_000_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    rgb.create_utxos(8, 60_000, 2)?;
    Ok(rgb)
}

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data14");
    env::set_var("ML_NETWORK", "regtest");

    let mut w = tokio::task::block_in_place(|| setup("./rgb-data14/issuer"))?;

    // ---- Issue every schema (CFA with details; IFA with ticker + inflation right). ----
    let nia = tokio::task::block_in_place(|| w.issue_nia("NIATK", "NIA asset", 2, vec![NIA_SUPPLY]))?;
    let uda = tokio::task::block_in_place(|| w.issue_uda("UDATK", "UDA asset", Some("a unique token"), 0, None, vec![]))?;
    let cfa = tokio::task::block_in_place(|| w.issue_cfa("CFA asset", Some("collectible details"), 1, vec![CFA_SUPPLY], None))?;
    let ifa = tokio::task::block_in_place(|| w.issue_ifa("IFATK", "IFA asset", 0, vec![IFA_ISSUED], vec![IFA_INFLATION]))?;

    // ---- Assert full metadata for each: (schema, ticker, name, precision, initial, max, circulating).
    
    let (nsch, ntk, nnm, npr, nin, nmx, ncir) = meta(&w, &nia)?;
    println!("RGB14 - NIA metadata: schema={nsch} ticker={ntk} name={nnm} precision={npr} initial={nin} max={nmx} circulating={ncir}");
    assert_eq!(nsch, "Nia", "NIA schema"); assert_eq!(ntk, "NIATK"); assert_eq!(nnm, "NIA asset");
    assert_eq!(npr, 2, "NIA precision"); assert_eq!(nin, NIA_SUPPLY); assert_eq!(nmx, NIA_SUPPLY, "NIA is non-inflatable: max == initial");

    let (usch, utk, unm, _upr, _uin, _umx, _ucir) = meta(&w, &uda)?;
    println!("RGB14 - UDA metadata: schema={usch} ticker={utk} name={unm}");
    assert_eq!(usch, "Uda", "UDA schema"); assert_eq!(utk, "UDATK"); assert_eq!(unm, "UDA asset");

    let (csch, _ctk, cnm, cpr, cin, cmx, _ccir) = meta(&w, &cfa)?;
    println!("RGB14 - CFA metadata: schema={csch} name={cnm} precision={cpr} initial={cin} max={cmx}");
    assert_eq!(csch, "Cfa", "CFA schema"); assert_eq!(cnm, "CFA asset"); assert_eq!(cpr, 1, "CFA precision");
    assert_eq!(cin, CFA_SUPPLY); assert_eq!(cmx, CFA_SUPPLY, "CFA is non-inflatable: max == initial");

    let (isch, itk, inm, ipr, iin, imx, icir) = meta(&w, &ifa)?;
    println!("RGB14 - IFA metadata: schema={isch} ticker={itk} name={inm} precision={ipr} initial={iin} max={imx} circulating={icir}");
    assert_eq!(isch, "Ifa", "IFA schema"); assert_eq!(itk, "IFATK"); assert_eq!(inm, "IFA asset"); assert_eq!(ipr, 0);
    assert_eq!(iin, IFA_ISSUED, "IFA initial issued supply");
    assert_eq!(imx, IFA_ISSUED + IFA_INFLATION, "IFA invariant: max_supply == initial + inflation right");
    println!("RGB14 - IFA supply invariant holds: max({imx}) == initial({iin}) + inflation-right({IFA_INFLATION})");

    // ---- Realize the inflation right on-chain; circulating supply must rise to the cap. ----
    let (inflate_txid, minted) = tokio::task::block_in_place(|| w.inflate(&ifa, vec![IFA_INFLATION], 2))?;
    assert_eq!(minted, IFA_INFLATION, "inflate mints the full inflation right");
    println!("RGB14 - inflated IFA by {minted} (tx {inflate_txid})");

    let mut circulating = icir;
    for _ in 0..20 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(1, &core)?;
        tokio::task::block_in_place(|| { let _ = w.refresh(Some(ifa.clone())); });
        let (_, _, _, _, _, mx_now, cir_now) = meta(&w, &ifa)?;
        circulating = cir_now;
        // once realized, the whole max supply is circulating and no inflation right remains.
        if circulating >= mx_now { break; }
        thread::sleep(Duration::from_secs(1));
    }
    println!("RGB14 - IFA circulating after inflate = {circulating}");
    assert_eq!(circulating, IFA_ISSUED + IFA_INFLATION, "after realizing the inflation right, circulating == max_supply");

    println!("RGB14 - SUCCESS: NIA/UDA/CFA/IFA metadata (schema/ticker/name/precision/supply) is faithful over statechain-RGB; the IFA invariant max=initial+inflation holds and realizing the right raises circulating supply to the cap ({}).", IFA_ISSUED + IFA_INFLATION);
    Ok(())
}
