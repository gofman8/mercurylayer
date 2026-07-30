//! E2E (security, honest about the trust floor): the **fresh double-sign** case — a *malicious SE*
//! that co-signs a second, conflicting spend of a coin's funding UTXO `F`.
//!
//! Under TES-R a coin's ladder hangs off a **trigger** `T` that spends `F` and is itself
//! *locktime-free*: it carries no absolute locktime and no CSV, because the relative timelocks live
//! on the extension/state tiers hanging BELOW it, and an idle coin never ages (nothing is broadcast,
//! so no clock runs). A freshly SE-co-signed RIVAL trigger over the same `F` is therefore exactly as
//! final as the owner's own — the CSV tier chain breaks no tie and gives NO advantage. The contest
//! degrades to a plain on-chain race (first-seen / highest-fee wins). The ladder shape changed from
//! the pre-TES-R design; this floor did not.
//!
//! We model the malicious SE with a NORMAL (non-single-use, no-budget) coin, for which the honest SE
//! already permits re-signing — `server/src/endpoints/sign.rs`'s single-use / spend-budget /
//! epoch-deadline gates simply do not fire — so obtaining two conflicting co-signatures of one coin
//! exercises exactly the code path a gate-ignoring SE would take. The point: two valid conflicting
//! spends of `F` exist, and only the one that confirms first wins. This is the irreducible single-SE
//! trust floor that neither the ladder (an absolute-locktime backup chain OR the TES-R CSV tiers),
//! single_use, nor the terminal/budget query can close (only threshold signing can).
//!
//! Not to be confused with sdk12, which proves the INVERSE property: the SE cannot double-sign behind
//! the receiver's back (nonce atomicity). Here the SE is *willing*, and nothing client-side stops it.
//!
//! Run: SDK_E2E=15 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

const NETWORK: &str = "regtest";
/// The two rival triggers must differ as *transactions*. A txid covers no witness data, so two
/// co-signs of the SAME tier bytes would be one and the same tx — they are built at different
/// committed fee rates (and to different destinations) so they are genuinely distinct conflicts.
/// `FEE_X > FEE_Y` also makes the winner unambiguous: the loser is neither first nor a valid RBF.
const FEE_X: f64 = 5.0;
const FEE_Y: f64 = 3.0;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    // No protocol pin: this runs on the real TES-R default. The coin below is laddered by
    // claim() (`tesr::establish_auto`), which is exactly the point — the CSV ladder IS in place, the
    // coin never ages while idle, and none of that buys the honest owner anything against a freshly
    // co-signed rival trigger over the same funding UTXO.
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk15_alice"), None).await?;

    // Fund a NORMAL statechain coin (single_use=false, no budget) -> the SE permits re-signing, which
    // is exactly what a malicious SE ignoring its own terminality gates would do for an off-chain node.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(40_000).await?;
    bitcoin_core::sendtoaddress(40_000, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 40_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 { return Err(anyhow!("deposit did not confirm")); }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    // Take the confirmed coin once claim() has also established its TES-R ladder (the default).
    let mut found = None;
    for _ in 0..15 {
        let candidate = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk15_alice")
            .await?
            .coins
            .iter()
            .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(40_000))
            .cloned();
        if let Some(c) = candidate {
            let sid = c.statechain_id.clone().unwrap_or_default();
            if mercuryrustlib::tesr::load(&cc, "sdk15_alice", &sid).await?.is_some() {
                found = Some(c);
                break;
            }
        }
        alice.claim().await?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let coin = found.ok_or_else(|| anyhow!("no confirmed coin carrying a TES-R ladder"))?;
    let sid = coin.statechain_id.clone().ok_or_else(|| anyhow!("coin has no statechain_id"))?;
    // The TES-R property is really in force before the attack: the coin IS laddered.
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk15_alice", &sid).await?.is_some(),
        "the coin must carry a TES-R ladder — the ladder is present and still powerless below"
    );
    println!("SDK15 - funded a normal coin {sid} (SE permits re-signing); its TES-R ladder is established");

    let f_txid = coin.utxo_txid.clone().ok_or_else(|| anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or_else(|| anyhow!("no F vout"))?;
    let f_value = coin.amount.ok_or_else(|| anyhow!("no F value"))? as u64;
    let agg = coin.aggregated_address.clone().ok_or_else(|| anyhow!("no aggregated_address"))?;
    let thief_addr = bitcoin_core::getnewaddress()?;

    // --- The malicious SE blind-co-signs TWO conflicting TRIGGERS over the SAME F -----------------
    // Both spend F, both are locktime-free (a trigger has neither nLockTime nor a CSV — those bind
    // only the tiers beneath it), so neither carries any handicap relative to the other:
    //   tx_X = the owner-shaped trigger paying P2TR(A), fee-bumped (the honest owner's head start).
    //   tx_Y = a thief-shaped trigger paying an address the owner does not control — what a willing
    //          SE, alone or colluding with a previous owner, can always produce.
    let mut c1 = coin.clone();
    let t_x = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg, NETWORK, FEE_X)?;
    let tx_x = mercuryrustlib::tesr::cosign_tier(&cc, &mut c1, t_x.tx_hex.clone(), f_value, NETWORK).await?;
    let mut c2 = coin.clone(); // fresh nonce state
    let t_y = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &thief_addr, NETWORK, FEE_Y)?;
    let tx_y = mercuryrustlib::tesr::cosign_tier(&cc, &mut c2, t_y.tx_hex.clone(), f_value, NETWORK).await?;
    let txx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&tx_x)?)?;
    let txy: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&tx_y)?)?;
    // Both spend the SAME coin outpoint -> they conflict.
    assert_eq!(txx.input[0].previous_output, txy.input[0].previous_output,
        "both fresh co-signs must spend the same coin outpoint (they conflict)");
    // ...and they are genuinely two different transactions, not one tx signed twice.
    assert_ne!(txx.txid(), txy.txid(), "the two fresh co-signs must be distinct transactions");
    println!("SDK15 - the SE produced TWO conflicting fresh TRIGGER co-signs: tx_X {} and tx_Y {} (same input {})",
        txx.txid(), txy.txid(), txx.input[0].previous_output);

    // --- On-chain it is a plain race: only the FIRST-seen confirms; the other is rejected ---------
    // Once tx_X is mined the outcome is fee-independent: tx_Y's input simply no longer exists.
    let bx = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&tx_x)?);
    assert!(bx.is_ok(), "first-broadcast conflicting spend is accepted: {bx:?}");
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let by = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&tx_y)?);
    assert!(by.is_err(), "the conflicting spend loses the race (already-spent input): {by:?}");
    println!("SDK15 - tx_X won the race (confirmed); tx_Y REJECTED (its input is already spent). Only ONE of the two co-signed conflicts can settle — and it won only by being first with the higher fee, not by anything the ladder did.");

    println!("SDK15 - SUCCESS (documents the trust floor): a malicious SE CAN produce two conflicting fresh co-signatures of one funding UTXO. On a laddered coin the rival is a TRIGGER, which is locktime-free — the TES-R CSV tier chain constrains only the tiers BELOW a trigger, so two fresh triggers over F start their clocks on equal terms and the ladder gives NO advantage. It is a first-seen/highest-fee RACE. The honest party's only defence is speed + fee (be first / bump via the P2A anchor). This is the irreducible single-SE trust — closed only by threshold-signing the SE, not by any client-side layer (not the TES-R CSV ladder, not single_use, not the terminal/spend-budget query).");
    Ok(())
}
