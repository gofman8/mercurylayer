//! E2E (SDK_E2E=40) — **TES-R (Utexo V2) consensus core** on the live SE + real bitcoind.
//!
//! Validates the load-bearing claims of `docs/utexo/V2-DESIGN.md` against real Bitcoin consensus,
//! co-signed by the *unchanged* blind SE (proving the "enclave cryptographically unchanged" claim —
//! it blind-signs v3 + relative-timelock + P2A sighashes exactly as it signs V1 backups):
//!
//!   PART 1 — un-broadcast immunity, CSV enforcement, full unilateral exit (coin A):
//!     * Deposit F; blind-co-sign the whole tier tree T → X → S (nothing broadcast; nothing ages).
//!     * Broadcast T, confirm. Assert X is REJECTED before E confirmations of T (BIP-68 not met).
//!     * Mine to E; X accepted. Assert S REJECTED before D confirmations of X; mine to D; S accepted.
//!     * The owner's funds land with NO operator cooperation — a pure unilateral exit.
//!
//!   PART 2 — cooperative de-trigger defeats a hostile trigger (coin B):
//!     * Deposit F'; co-sign the stale ladder T' → X'.
//!     * A griefer broadcasts T'. The owner responds with a fresh no-timelock DE-TRIGGER spend of
//!       T'.out[0] — it confirms immediately, before X' can ever mature. Assert X' can NEVER confirm
//!       (its prevout is spent), even after E blocks. Griefing collapses to a priced nuisance.
//!
//! Run with SDK_E2E=40 (needs the regtest + Mercury lockbox stack; bitcoind must be Core 28+ for
//! v3/TRUC + P2A relay — the stack ships Core 30).

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

use crate::{bitcoin_core, electrs};

const NETWORK: &str = "regtest";
const FEE_RATE: f64 = 2.0;
const COIN_SAT: u32 = 100_000;
// Test-scale CSV constants (mainnet uses E0=720, D0=1440). Small so a full lifecycle mines fast.
const CSV_E: u16 = 4; // extension relative-timelock (blocks)
const CSV_D: u16 = 6; // state relative-timelock (blocks)

async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
}

/// True iff `txid:vout` is no longer in the UTXO set (i.e. it has been spent / never existed unspent).
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

fn tx_exists(cc: &ClientConfig, txid: &str) -> bool {
    use electrum_client::bitcoin::Txid;
    cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()).is_ok()
}

fn broadcast(cc: &ClientConfig, tx_hex: &str) -> Result<String> {
    let raw = hex::decode(tx_hex)?;
    Ok(cc.electrum_client.transaction_broadcast_raw(&raw)?.to_string())
}

fn mine(n: u32) -> Result<()> {
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(n, &core)?;
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// Deposit a fresh statechain coin of `COIN_SAT` to a new wallet; return its confirmed `Coin` (F).
async fn deposit_coin(cc: &ClientConfig, wallet_name: &str) -> Result<Coin> {
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, COIN_SAT)
            .await?;
    let _ = bitcoin_core::sendtoaddress(COIN_SAT, &sc_address)?;
    wait_for_address(cc, &sc_address, COIN_SAT).await?;
    mine(cc.confirmation_target.max(1))?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(&sc_address))
        .ok_or(anyhow!("coin not found for {sc_address}"))?
        .clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "funding coin F must confirm");
    Ok(coin)
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk40");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // ============================ PART 1: coin A — full lifecycle ============================
    let mut coin_a = deposit_coin(&cc, "sdk40_alice").await?;
    let f_txid = coin_a.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin_a.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let f_value = coin_a.amount.ok_or(anyhow!("no F value"))? as u64;
    let agg_addr = coin_a.aggregated_address.clone().ok_or(anyhow!("no agg addr"))?;
    let owner_exit = bitcoin_core::getnewaddress()?;
    println!("SDK40 - coin A: F = {f_txid}:{f_vout} ({f_value} sat) @ {agg_addr}");

    // Blind-co-sign the FULL tier tree over F — every tier through the live SE. Nothing broadcast.
    let t = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg_addr, NETWORK, FEE_RATE)?;
    let t_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_a, t.tx_hex.clone(), f_value, NETWORK).await?;

    let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, &agg_addr, NETWORK, CSV_E, FEE_RATE)?;
    let x_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_a, x.tx_hex.clone(), t.out_value, NETWORK).await?;

    let s = mercurylib::tesr::build_state(&x.txid, x.out_value, &owner_exit, NETWORK, CSV_D, FEE_RATE)?;
    let s_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_a, s.tx_hex.clone(), x.out_value, NETWORK).await?;
    println!("SDK40 - tier tree co-signed by SE: T={} X={}(csv {CSV_E}) S={}(csv {CSV_D})", t.txid, x.txid, s.txid);

    // Un-broadcast immunity: nothing is on chain, F is still unspent, no clock is running.
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "un-broadcast: F still UNSPENT, nothing ages");

    // Broadcast the trigger and confirm it (1 conf). The clock starts now.
    let _ = broadcast(&cc, &t_signed)?;
    mine(1)?;
    assert!(tx_exists(&cc, &t.txid), "T must confirm");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by T");
    println!("SDK40 - T broadcast + confirmed (1 conf); clock started");

    // CSV enforcement #1: X needs E confirmations of T. With 1, it MUST be rejected.
    assert!(broadcast(&cc, &x_signed).is_err(), "X must be REJECTED before {CSV_E} confs of T (BIP-68)");
    println!("SDK40 - ✓ X rejected at 1 conf (relative-timelock enforced)");
    let _ = mine((CSV_E - 1) as u32); // T now has E confs
    let _ = broadcast(&cc, &x_signed)?;
    mine(1)?;
    assert!(tx_exists(&cc, &x.txid), "X must confirm once T has {CSV_E} confs");
    println!("SDK40 - ✓ X accepted after {CSV_E} confs of T");

    // CSV enforcement #2: S needs D confirmations of X.
    assert!(broadcast(&cc, &s_signed).is_err(), "S must be REJECTED before {CSV_D} confs of X");
    println!("SDK40 - ✓ S rejected at 1 conf");
    let _ = mine((CSV_D - 1) as u32);
    let _ = broadcast(&cc, &s_signed)?;
    mine(1)?;
    assert!(tx_exists(&cc, &s.txid), "S must confirm once X has {CSV_D} confs");
    assert!(is_outpoint_spent(&cc, &x.txid, 0), "X.out0 consumed by S");
    // Funds landed at the owner's own address — a complete unilateral exit, no operator help.
    assert!(wait_for_address(&cc, &owner_exit, s.out_value as u32).await.is_ok(), "owner exit funds landed");
    println!("SDK40 - ✓ PART 1: full unilateral exit T→X→S reached owner ({} sat) with no SE cooperation", s.out_value);

    // ============================ PART 2: coin B — cooperative de-trigger ============================
    let mut coin_b = deposit_coin(&cc, "sdk40_bob").await?;
    let fb_txid = coin_b.utxo_txid.clone().ok_or(anyhow!("no F' txid"))?;
    let fb_vout = coin_b.utxo_vout.ok_or(anyhow!("no F' vout"))?;
    let fb_value = coin_b.amount.ok_or(anyhow!("no F' value"))? as u64;
    let agg_b = coin_b.aggregated_address.clone().ok_or(anyhow!("no agg addr B"))?;
    let detrigger_dest = bitcoin_core::getnewaddress()?;

    // Stale ladder: trigger + extension.
    let tb = mercurylib::tesr::build_trigger(&fb_txid, fb_vout, fb_value, &agg_b, NETWORK, FEE_RATE)?;
    let tb_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_b, tb.tx_hex.clone(), fb_value, NETWORK).await?;
    let xb = mercurylib::tesr::build_extension(&tb.txid, tb.out_value, &agg_b, NETWORK, CSV_E, FEE_RATE)?;
    let xb_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_b, xb.tx_hex.clone(), tb.out_value, NETWORK).await?;

    // A griefer broadcasts the trigger to force a cost.
    let _ = broadcast(&cc, &tb_signed)?;
    mine(1)?;
    println!("SDK40 - coin B: hostile trigger T' broadcast");

    // Owner responds with a fresh no-timelock DE-TRIGGER spend of T'.out0 — the coin's own aggregate
    // key (co-signed fresh by the SE), confirming immediately, before X' can ever mature.
    let de = mercurylib::tesr::build_detrigger(&tb.txid, tb.out_value, &detrigger_dest, NETWORK, FEE_RATE)?;
    let de_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_b, de.tx_hex.clone(), tb.out_value, NETWORK).await?;
    let _ = broadcast(&cc, &de_signed)?;
    mine(1)?;
    assert!(tx_exists(&cc, &de.txid), "de-trigger confirms immediately (no CSV wait)");
    assert!(is_outpoint_spent(&cc, &tb.txid, 0), "T'.out0 consumed by the de-trigger");
    println!("SDK40 - ✓ de-trigger confirmed, spending T'.out0 with no timelock");

    // The stale extension can now NEVER confirm — its prevout is gone — even after the full E window.
    let _ = mine(CSV_E as u32);
    assert!(broadcast(&cc, &xb_signed).is_err(), "stale X' can never confirm: its prevout T'.out0 is spent");
    println!("SDK40 - ✓ PART 2: stale ladder defeated — griefing collapses to a priced nuisance");

    // ============================ PART 3: off-chain renewal (Decker-Wattenhofer) ============================
    // Renewal = co-sign a NEW extension X_1 with a LOWER extension-CSV than X_0, both spending
    // T.out[0]. It replaces horizontally (no new depth, no on-chain byte). Consensus-level claim:
    // once T confirms, the renewed X_1 (lower CSV) matures FIRST, so the pre-renewal X_0 can never win
    // the race for T.out[0] — old state dies at the consensus level, not by an enclave promise. This
    // is what lets an active coin renew unlimited times off-chain (footprint scales with activity).
    const CSV_E0: u16 = 4; // pre-renewal extension
    const CSV_E1: u16 = 2; // renewed extension (E0 − δE), strictly lower
    let mut coin_c = deposit_coin(&cc, "sdk40_carol").await?;
    let fc_txid = coin_c.utxo_txid.clone().ok_or(anyhow!("no F'' txid"))?;
    let fc_vout = coin_c.utxo_vout.ok_or(anyhow!("no F'' vout"))?;
    let fc_value = coin_c.amount.ok_or(anyhow!("no F'' value"))? as u64;
    let agg_c = coin_c.aggregated_address.clone().ok_or(anyhow!("no agg addr C"))?;

    let tc = mercurylib::tesr::build_trigger(&fc_txid, fc_vout, fc_value, &agg_c, NETWORK, FEE_RATE)?;
    let tc_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_c, tc.tx_hex.clone(), fc_value, NETWORK).await?;
    // X_0 (old) and X_1 (renewed, lower CSV) both spend T''.out[0].
    let x0 = mercurylib::tesr::build_extension(&tc.txid, tc.out_value, &agg_c, NETWORK, CSV_E0, FEE_RATE)?;
    let x0_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_c, x0.tx_hex.clone(), tc.out_value, NETWORK).await?;
    let x1 = mercurylib::tesr::build_extension(&tc.txid, tc.out_value, &agg_c, NETWORK, CSV_E1, FEE_RATE)?;
    let x1_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_c, x1.tx_hex.clone(), tc.out_value, NETWORK).await?;

    let _ = broadcast(&cc, &tc_signed)?;
    let _ = mine(CSV_E1 as u32); // T'' now has E1 confs: X_1 final, X_0 (needs E0 > E1) not yet
    // At the renewal moment the OLD extension cannot confirm, but the RENEWED one can:
    assert!(broadcast(&cc, &x0_signed).is_err(), "pre-renewal X_0 not final at {CSV_E1} confs");
    let _ = broadcast(&cc, &x1_signed)?;
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &x1.txid), "renewed X_1 (lower CSV) confirms first");
    assert!(is_outpoint_spent(&cc, &tc.txid, 0), "T''.out0 consumed by the renewed extension");
    // The superseded extension is now permanently dead, even past its own CSV window.
    let _ = mine(CSV_E0 as u32);
    assert!(broadcast(&cc, &x0_signed).is_err(), "superseded X_0 can never confirm (prevout spent)");
    println!("SDK40 - ✓ PART 3: off-chain renewal — X_1 superseded X_0 at consensus level (zero on-chain bytes)");

    println!("SDK40 - PASS: TES-R consensus core validated on live SE + real bitcoind");
    Ok(())
}
