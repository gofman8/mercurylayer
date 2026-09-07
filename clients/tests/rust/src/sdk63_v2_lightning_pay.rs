//! E2E (SDK_E2E=63) — **V2 (TES-R) coin → Lightning PAY through the SSP** (HODL-latch pivot).
//!
//! The user-facing blocker the pivot removes: with V2 as the default, a user's coins are all
//! laddered, and the old sdk53 guard refused a Lightning-latched transfer of a V2 coin — so a V2
//! wallet could not pay over LN at all. This proves it now works: alice pays a real BOLT11 from a
//! **V2** statechain coin, and the SSP's **pre-pay census** (`prepay_flat_census`: the conveyed
//! `backup_transactions` MUST be empty — `verify_flat_backup_lane` refuses any flat backup by name —
//! then `verify_bundle_bound(bundle, num_sigs, 0, authority)`, i.e. `num_sigs == tiers + superseded`
//! with the flat term pinned to ZERO, read from the enclave sig-count) runs before send_payment.
//!
//! Re-derived for the ladder rule, which this test now also pins on both ends of the hop:
//!   * alice's deposit is laddered AT SIGHT and carries NO flat backup: a `tesr-` row exists, there
//!     are ZERO flat backup rows (`create_tx1` is gone), `locktime == None`, and the enclave count is
//!     exactly the 3 tiers (a 4 would be a `tx1` co-signed at deposit);
//!   * the SSP's claimed coin is the same laddered coin one hop on: a `tesr-` row in the SSP's
//!     wallet, still ZERO flat rows (the hop co-signed the receiver-paying state `S'`, never a
//!     receiver-paying flat backup), still `locktime == None`, and its census balances with the
//!     flat term 0 — and does NOT balance with a flat term of 1 (no phantom `tx1` may be counted).
//!
//! To isolate the census + guard-lift from the in-ladder-split machinery, alice deposits the EXACT
//! invoice amount so `ensure_exact_coin` uses the whole V2 coin (no split).
//! 
//!
//! Run: SDK_E2E=63 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::ssp::{RlnClient, SspService};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use sha2::{Digest, Sha256};
use std::time::Duration;

use crate::{bitcoin_core, rln};

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// The statechain id of `wallet`'s one CONFIRMED (index-0) coin.
async fn confirmed_sid(cc: &mercuryrustlib::client_config::ClientConfig, wallet: &str) -> Result<String> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("{wallet} has no confirmed coin"))
}

/// The exit-material shape the ladder rule gives every laddered coin, read the way a receiver reads
/// it: a `tesr-` row EXISTS, there are ZERO flat backup rows, and `locktime == None`.
async fn assert_ladder_shape(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet: &str,
    sid: &str,
    what: &str,
) -> Result<mercuryrustlib::tesr::TesrBundle> {
    let bundle = mercuryrustlib::tesr::load(cc, wallet, sid)
        .await?
        .ok_or(anyhow!("{what} ({sid}) has no `tesr-` row in {wallet}: no exit material at all"))?;
    let flat_rows = mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet, sid)
        .await?
        .map_or(0, |rows| rows.len());
    assert_eq!(flat_rows, 0, "{what}: a laddered coin carries ZERO flat backup rows, found {flat_rows}");
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0)
        .ok_or(anyhow!("{what} ({sid}) is not in wallet {wallet}"))?;
    assert_eq!(coin.locktime, None, "{what}: coin.locktime must be None for life — no absolute calendar");
    Ok(bundle)
}

/// The enclave-attested co-signature count for `sid`.
async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain info for {sid}"))?
        .num_sigs)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    // Lightning: node A = merchant, node B = the SSP's node.
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk63").await?;
    println!("SDK63 - LN pair up (channel usable)");

    // SSP: statechain wallet + its RLN node, zero fee (so the exact coin == invoice amount).
    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk63_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);

    // alice: deposit EXACTLY the invoice amount as a single V2 coin (no split needed).
    let amount: u64 = 25_000;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk63_alice"), None).await?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(amount).await?;
    bitcoin_core::sendtoaddress(amount as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != amount {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK63 - alice funded: {amount} sats on a V2 (TES-R laddered) coin");

    // THE SHAPE, pinned before any Lightning leg: laddered, ZERO flat backup rows, no calendar, and
    // an enclave count of exactly the 3 tiers — a 4 would mean a flat `tx1` was co-signed at deposit.
    let alice_sid = confirmed_sid(&cc, "sdk63_alice").await?;
    let alice_bundle = assert_ladder_shape(&cc, "sdk63_alice", &alice_sid, "alice's deposit").await?;
    let sigs_at_deposit = num_sigs(&cc, &alice_sid).await?;
    assert_eq!(
        sigs_at_deposit, 3,
        "alice's laddered deposit must have consumed exactly 3 co-signs (T, X, S); a 4 is a flat tx1"
    );
    mercuryrustlib::tesr::verify_bundle(&alice_bundle, sigs_at_deposit, 0)
        .map_err(|e| anyhow!("alice's census must balance with a flat term of 0: {e}"))?;
    println!("SDK63 - alice's coin {alice_sid}: `tesr-` row, 0 flat backup rows, locktime None, num_sigs 3 == 0 flat + 3 tiers");

    // Merchant issues an invoice for the exact amount on its own node.
    let invoice = merchant_node.ln_invoice(amount * 1000, None, 3600).await?;
    println!("SDK63 - merchant invoice created ({amount} sats)");

    // alice pays it from her V2 statechain coin through the SSP. The SSP's execute_pay runs the
    // pre-pay ladder census (verify_bundle over the conveyed V2 ladder) BEFORE send_payment.
    let preimage = alice.pay_lightning_invoice(&ssp, &invoice).await?;
    println!("SDK63 - invoice paid from a V2 coin; alice holds the preimage as proof");

    // Proof: preimage hashes to the invoice hash; merchant sees Succeeded.
    let (_, invoice_hash) = ssp.rln.decode_invoice(&invoice).await?;
    assert_eq!(
        hex::encode(Sha256::digest(hex::decode(&preimage)?)),
        invoice_hash,
        "preimage is the invoice's"
    );
    assert_eq!(merchant_node.invoice_status(&invoice).await?, "Succeeded");

    // The SSP now owns the V2 coin (its claim re-ran verify_bundle and accepted the Model-A ladder).
    let ssp_balance = ssp.wallet.get_balance().await?;
    assert_eq!(ssp_balance.available_sats, amount, "SSP claimed the latched V2 coin");
    println!(
        "SDK63 - settled: SSP owns {amount} (V2 coin, census passed pre-pay AND at claim); merchant paid on LN"
    );

    // The hop conveyed NO flat backup: the SSP's coin is the same laddered coin one hop on — a
    // `tesr-` row in the SSP's wallet, still ZERO flat rows, still no calendar — and its census
    // balances with the flat term 0 (num_sigs == tiers + superseded: the hop added exactly the
    // receiver-paying state S', with the old S disclosed as superseded) and NOT with a flat term of 1.
    let ssp_bundle = assert_ladder_shape(&cc, "sdk63_ssp", &alice_sid, "the SSP's claimed coin").await?;
    let sigs_after_hop = num_sigs(&cc, &alice_sid).await?;
    mercuryrustlib::tesr::verify_bundle(&ssp_bundle, sigs_after_hop, 0).map_err(|e| {
        anyhow!("the SSP's census must balance with a flat term of 0 (num_sigs {sigs_after_hop}): {e}")
    })?;
    assert!(
        mercuryrustlib::tesr::verify_bundle(&ssp_bundle, sigs_after_hop, 1).is_err(),
        "no phantom flat backup may be counted: the census must NOT balance with a flat term of 1"
    );
    println!(
        "SDK63 - the SSP's coin {alice_sid}: `tesr-` row, 0 flat backup rows, locktime None, census \
         num_sigs {sigs_after_hop} == 0 flat + {} tiers + {} superseded",
        ssp_bundle.exit_tiers().len(),
        (sigs_after_hop as usize).saturating_sub(ssp_bundle.exit_tiers().len())
    );

    println!("SDK63 - ✓ SUCCESS: a V2 (TES-R) coin paid a real BOLT11 through the SSP. The sdk53 guard is lifted; the SSP's pre-pay census (no conveyed flat backup, verify_bundle_bound with flat term 0) guards rob-SSP; the LN preimage unlocked the coin and proved payment; the coin carried no flat backup and no calendar on either side of the hop. V2 wallets can now pay over Lightning.");
    Ok(())
}
