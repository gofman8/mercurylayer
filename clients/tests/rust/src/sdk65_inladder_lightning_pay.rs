//! E2E (SDK_E2E=65) — **NON-EXACT V2 Lightning PAY via a latched in-ladder split** (V2-LN-HODL.md §2b).
//!
//! The usability gap the exact lane (sdk63) leaves: real invoices are arbitrary amounts, and splitting
//! a V2 laddered coin yields an in-ladder-split CHILD conveyed via a mailbox that the exact-lane census
//! does not handle. This proves the latched-child-conveyance: Alice pays a NON-EXACT BOLT11 from a
//! single V2 coin — `pay_lightning_invoice_inladder` splits IN-LADDER (piece pays the SSP, change stays
//! Alice's), latches the PIECE to the invoice hash, and the SSP's pre-pay census runs
//! `verify_conveyed_child` on the CHILD bundle before `send_payment`. The piece sits at the same
//! operator-trust bar as the exact `S'`; the change is self-owned.
//!
//! No `UTEXO_PROTOCOL_DEFAULT` pin — runs on the real V2 default.
//! Run: SDK_E2E=65 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

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

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk65").await?;
    println!("SDK65 - LN pair up (channel usable)");

    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk65_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);

    // Alice: one V2 (TES-R laddered) coin, big enough to split for a non-exact payment + change.
    let deposit: u64 = 50_000;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk65_alice"), None).await?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(deposit).await?;
    bitcoin_core::sendtoaddress(deposit as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != deposit {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let parent_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk65_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk65_alice", &parent_sid).await?.is_some(),
        "alice's coin must carry a TES-R ladder (V2) for an in-ladder split"
    );
    println!("SDK65 - alice funded: {deposit} sats on a V2 coin (sid {parent_sid})");

    // Merchant issues a NON-EXACT invoice (< the whole coin ⟹ the pay MUST split in-ladder).
    let invoice_sats: u64 = 25_000;
    let invoice = merchant_node.ln_invoice(invoice_sats * 1000, None, 3600).await?;
    println!("SDK65 - merchant invoice created ({invoice_sats} sats, non-exact)");

    // Alice pays it by in-ladder-splitting the parent: piece → SSP (latched + censused), change → Alice.
    let preimage = alice
        .pay_lightning_invoice_inladder(&ssp, &invoice, &parent_sid)
        .await?;
    println!("SDK65 - invoice paid via IN-LADDER split; alice holds the preimage as proof");

    // Proof: preimage hashes to the invoice hash; merchant sees Succeeded.
    let (_, invoice_hash) = ssp.rln.decode_invoice(&invoice).await?;
    assert_eq!(
        hex::encode(Sha256::digest(hex::decode(&preimage)?)),
        invoice_hash,
        "preimage is the invoice's"
    );
    assert_eq!(merchant_node.invoice_status(&invoice).await?, "Succeeded");

    // The SSP censused + claimed the PIECE child (worth >= the invoice); Alice keeps her CHANGE.
    let ssp_bal = ssp.wallet.get_balance().await?;
    assert!(
        ssp_bal.available_sats >= invoice_sats,
        "SSP must own the piece child worth >= the invoice (got {})",
        ssp_bal.available_sats
    );
    let alice_bal = alice.get_balance().await?;
    assert!(
        alice_bal.available_sats > 15_000,
        "alice must keep the in-ladder change (got {})",
        alice_bal.available_sats
    );
    println!(
        "SDK65 - settled: SSP owns the piece ({} sat), alice change {} sat, merchant paid on LN",
        ssp_bal.available_sats, alice_bal.available_sats
    );

    println!("SDK65 - ✓ SUCCESS: a NON-EXACT BOLT11 was paid from a single V2 coin via a latched in-ladder split. The piece child was censused (verify_conveyed_child) BEFORE payment and adopted by the SSP after; the change stayed with Alice. V2 wallets can now pay arbitrary amounts over Lightning.");
    Ok(())
}
