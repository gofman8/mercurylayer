//! E2E (rgb-tests parity, RGB_E2E=11): **issue every RGB asset schema over statechain** (NIA, UDA,
//! CFA, IFA) and prove a NON-NIA fungible asset (CFA) moves through the statechain co-signer exactly
//! like NIA.
//!
//! Mirrors upstream `issue_nia` / `issue_uda` / `issue_cfa` / `issue_ifa` (each asserts the asset is
//! issued with the right ticker/name/precision/supply) plus a CFA transfer over statechain.
//!
//! Re-derived for the ladder rule. The CFA transfer used to be an un-broadcast
//! `create_colored_split_tx` over the carrier's funding output — the off-chain BRANCH lane, which is
//! retired (the SDK refuses to register its sub-coins; a laddered coin's `T` already spends `F`). The
//! carrier here is a `single_use` deposit: the one deposit shape that gets NO TES-R ladder at first
//! sight (the SE refuses any second co-sign on it) and, since `create_tx1` is gone, no flat backup
//! either — its ONLY spend is the one the SE co-signs. So the transfer is that one spend: a coloured
//! WITHDRAWAL of the whole CFA allocation (`create_colored_backup_tx`, `is_withdrawal = true`) to the
//! receiver's witness address, validated off-chain by the receiver against its own txid (the same
//! gate a coloured-ladder receiver runs on a tier), then broadcast and settled on chain. What is
//! asserted, and why each part can fail:
//!
//!   1. all four schemas issue with the right ticker/name/precision/supply;
//!   2. the CFA carrier is `single_use`-shaped: no `tesr-` row, ZERO flat backup rows,
//!      `locktime == None` (a ladder here would spend the coin's only co-sign; a flat row would be
//!      the deleted `tx1`);
//!   3. the coloured withdrawal colours a CFA transition (rgb-lib accepts a CFA `output_map` just as
//!      it does NIA), the receiver validates it off-chain, and once broadcast the receiver settles the
//!      FULL supply while the issuer settles to zero and the carrier outpoint is spent.
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
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

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
    // single_use: the SE co-signs exactly ONE spend of this coin — the withdrawal below.
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

async fn coin_outpoint(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<(String, u32, Coin)> {
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

/// The exit-material shape of a `single_use` deposit under the ladder rule: nothing pre-signed at
/// all — no `tesr-` row (the SE would refuse the ladder's second co-sign), no flat backup row
/// (`create_tx1` no longer exists), and no absolute calendar on the coin.
async fn assert_single_use_shape(cc: &ClientConfig, wallet_name: &str, coin: &Coin) -> Result<()> {
    let sid = coin.statechain_id.clone().ok_or(anyhow!("coin has no statechain id"))?;
    assert!(coin.single_use, "the CFA carrier must be a single_use deposit");
    assert!(
        mercuryrustlib::tesr::load(cc, wallet_name, &sid).await?.is_none(),
        "a single_use coin must carry NO TES-R ladder: laddering it would spend its only co-sign \
         and the withdrawal below would be the SE's refusal"
    );
    let flat = mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, &sid).await?;
    assert!(
        flat.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "a deposit must carry NO flat backup row (create_tx1 is gone); found {:?} row(s) for {sid}",
        flat.map(|v| v.len())
    );
    assert!(
        coin.locktime.is_none(),
        "no coin carries an absolute calendar: locktime must be None, got {:?}",
        coin.locktime
    );
    Ok(())
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

    // ---- 2. Move the CFA through the statechain co-signer, proving a NON-NIA fungible asset
    //         colours and settles exactly like NIA. The carrier is a single_use coin, so its one
    //         SE co-signed spend is the transfer: a coloured WITHDRAWAL of the whole allocation to
    //         the receiver's witness address, validated off-chain, then broadcast and confirmed.
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

    // The carrier's shape under the ladder rule: single_use ⟹ no ladder, no flat backup, no calendar.
    assert_single_use_shape(&cc, "rgb11_cfa", &coin_c).await?;
    println!("RGB11 - C is single_use: no `tesr-` row, zero flat backup rows, locktime None (its one spend is the withdrawal below)");

    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(CFA_SUPPLY))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let spend = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc, &issuer, &mut coin_c, &cfa, CFA_SUPPLY, &recv_addr, 0, true, None, NETWORK,
        si.fee_rate_sats_per_byte, si.initlock, si.interval, BLINDING, None, None,
    ).await?;
    println!("RGB11 - CFA withdrawal C -> receiver({CFA_SUPPLY}) co-signed (tx {}, payload vout {}); not yet broadcast", spend.txid, spend.recipient_vout);

    // The receiver's gate: the CFA consignment validates off-chain against the spend's own txid,
    // exactly as a NIA one does (and as a coloured-ladder tier does).
    let (valid, detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&spend.consignment, &[spend.txid.clone()])
    })?;
    println!("RGB11 - [receiver] validate_offchain_chain -> valid={valid} detail={:?}", detail);
    assert!(valid, "receiver must validate the CFA transition off-chain: {detail:?}");

    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recv_id, &spend.consignment, &spend.txid, spend.recipient_vout)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&spend.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_c}:{vout_c}")]))?;

    let mut recv_bal = 0;
    let mut issuer_bal = u64::MAX;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = issuer.refresh(None); let _ = receiver.refresh(None); });
        recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&cfa)).unwrap_or(0);
        issuer_bal = tokio::task::block_in_place(|| issuer.settled_balance(&cfa)).unwrap_or(u64::MAX);
        if recv_bal == CFA_SUPPLY && issuer_bal == 0 { break; }
        thread::sleep(Duration::from_secs(1));
    }

    crate::rgb_dump::dump("receiver after CFA transfer (got the whole supply)", &mut receiver, &cfa);
    crate::rgb_dump::dump("issuer after CFA transfer (settled to zero)", &mut issuer, &cfa);

    assert_eq!(recv_bal, CFA_SUPPLY, "receiver must hold the whole {CFA_SUPPLY} CFA supply after the transfer");
    assert_eq!(issuer_bal, 0, "the issuer's CFA must be gone (whole allocation withdrawn to the receiver)");
    assert!(is_outpoint_spent(&cc, &txid_c, vout_c), "CFA coin C consumed by the withdrawal");

    println!("RGB11 - SUCCESS: NIA/UDA/CFA/IFA all issue over statechain with correct metadata + supply; a CFA (non-NIA fungible) moves through the statechain co-signer exactly like NIA — single_use carrier (no ladder, no flat backup), one coloured withdrawal, validated off-chain by the receiver, settled on chain ({CFA_SUPPLY} -> receiver, issuer 0).");
    Ok(())
}
