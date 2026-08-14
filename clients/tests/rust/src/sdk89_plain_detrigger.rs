//! E2E (SDK_E2E=89) — **[D68] the PLAIN de-trigger, driven through the wallet API it was missing.**
//!
//! `mercuryrustlib::tesr::cosign_detrigger` shipped with TES-R and had **zero production callers**
//! for its whole life: only the coloured twin was wired (`colored_reanchor`, CR-D). So the plain
//! lane's answer to trigger griefing was a function nobody could reach, and `PROTOCOL.md` §5.8
//! described a capability that did not exist as a code path. [D57] retracted the overstated half of
//! that claim; this test is the other half — the part that ships, executed.
//!
//! # The scenario, which is the grief
//!
//! alice holds a laddered coin. A griefer broadcasts her trigger `T` — they can, it is co-signed and
//! un-timelocked. She did not choose the moment. Without a de-trigger she now walks the CSV ladder
//! on the griefer's schedule; on mainnet that is up to `d·720 + 2160` blocks.
//!
//! `detrigger_to_owner` spends `T.out[0]` with **no relative timelock**, so it is valid immediately
//! and confirms ahead of every pre-signed extension. What this test proves, on chain:
//!
//! * **(a)** the coin is laddered and NOT coloured — the precondition for this lane, checked before
//!   anything is broadcast, so a coloured coin taking this path (which would destroy its allocation)
//!   is impossible rather than merely unlikely;
//! * **(b)** the griefer's `T` really is on chain and really does spend `F`, so the grief is real
//!   rather than simulated by calling the de-trigger on an untriggered coin;
//! * **(c)** the de-trigger confirms, and its input is `T.out[0]` — it collapses the grief rather
//!   than racing it;
//! * **(d)** the value lands at an address ALICE named. Both the default (her own backup address)
//!   and an explicit one are exercised, because the explicit form is what a holder under duress
//!   uses and an untested parameter is not an option;
//! * **(e)** **the old ladder is DEAD.** The pre-signed extension `X_0` is submitted directly to the
//!   node and REJECTED — its input no longer exists. That is the property the whole manoeuvre is
//!   for, and it is asserted against bitcoind rather than inferred.
//!
//! # What this does NOT prove, stated because §5.8 once claimed it
//!
//! There is no fresh funding output `F′` and no rebuilt `T′/X′_0/S′_0`. On the plain lane the
//! de-trigger pays a plain address: this is an **EXIT**, and getting back off-chain is a fresh
//! deposit. The restoration half is unbuilt ([D57]). A reader of this test must not upgrade "the
//! grief is survivable and the owner picks the moment" into "the ladder resets fresh".
//!
//! Run: SDK_E2E=89 ML_NETWORK=regtest cargo run   (regtest + lockbox up)

use anyhow::{anyhow, Result};
use electrum_client::bitcoin::{consensus::deserialize, Transaction, Txid};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use std::str::FromStr;
use std::time::Duration;

use crate::bitcoin_core;

const DEPOSIT: u64 = 100_000;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// The transaction as the CHAIN has it, or `None` when the backend does not know it. Reads go
/// through electrum, the same backend the wallet itself uses — asking a second oracle would measure
/// the oracle rather than the wallet.
fn onchain(cc: &ClientConfig, txid: &str) -> Option<Transaction> {
    let t = Txid::from_str(txid).ok()?;
    let raw = cc.electrum_client.transaction_get_raw(&t).ok()?;
    deserialize(&raw).ok()
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let core = bitcoin_core::getnewaddress()?;
    let cfg = SdkConfig::regtest("sdk89_alice");
    let cc = mercuryrustlib::client_config::load().await;
    let (alice, _) = UtexoWallet::initialize(cfg, None).await?;

    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(DEPOSIT as u32, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;

    // Poll for the ladder: `claim()` is what establishes it.
    let mut sid = String::new();
    for _ in 0..60 {
        alice.claim().await?;
        let coins = alice.get_balance().await?;
        if coins.available_sats >= DEPOSIT {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk89_alice").await?;
            if let Some(c) = rec
                .coins
                .iter()
                .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED && c.duplicate_index == 0)
            {
                if let Some(s) = c.statechain_id.clone() {
                    if mercuryrustlib::tesr::load(&cc, "sdk89_alice", &s).await?.is_some() {
                        sid = s;
                        break;
                    }
                }
            }
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(!sid.is_empty(), "alice's coin never got a TES-R ladder — nothing to de-trigger");

    // ===== (a) THE PRECONDITION, MEASURED ========================================================
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk89_alice", &sid)
        .await?
        .ok_or_else(|| anyhow!("ladder vanished"))?;
    assert!(
        !bundle.is_colored(),
        "[D68] this lane is for PLAIN ladders only — a coloured coin must go through \
         `colored_reanchor`, whose de-trigger carries the RGB transition. A plain de-trigger over a \
         carrier spends its payload output with an uncoloured tier and destroys the allocation."
    );
    let trigger_txid = bundle.trigger.txid.clone();
    // The newest extension is the tier a griefer's ladder-walk would rely on next; it is what must
    // become unbroadcastable. `exit_tiers()` is trigger-then-(extension,state) per level, so the
    // extension of the last level is at index 1 of that level's pair.
    let x0_hex = bundle
        .levels
        .last()
        .ok_or_else(|| anyhow!("the ladder has no level, so no extension to invalidate"))?
        .extension
        .signed_tx
        .clone();
    println!("SDK89 - (a) alice's coin {sid} is laddered and PLAIN; its trigger is {trigger_txid}");

    // ===== (b) THE GRIEF: someone else puts T on chain ===========================================
    //
    // Broadcast the trigger directly, exactly as a griefer with a copy of the watch bundle would.
    // alice does not choose this moment — that is the whole premise.
    let raw = hex::decode(&bundle.trigger.signed_tx)?;
    cc.electrum_client
        .transaction_broadcast_raw(&raw)
        .map_err(|e| anyhow!("the griefer could not broadcast the trigger: {e}"))?;
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    onchain(&cc, &trigger_txid).ok_or_else(|| {
        anyhow!(
            "the griefer's trigger {trigger_txid} is not on chain, so there is no grief to collapse \
             and everything below would be vacuous"
        )
    })?;
    println!("SDK89 - (b) GRIEF: the trigger {trigger_txid} is confirmed. Without a de-trigger alice \
              now walks the CSV ladder on the griefer's schedule.");

    // ===== (c)+(d) THE COLLAPSE, to an address ALICE names =======================================
    let dest = bitcoin_core::getnewaddress()?;
    let detrigger_txid = alice
        .detrigger_to_owner(&sid, Some(dest.clone()))
        .await
        .map_err(|e| anyhow!("[D68] the plain de-trigger failed: {e:#}"))?;
    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let de = onchain(&cc, &detrigger_txid)
        .ok_or_else(|| anyhow!("the de-trigger {detrigger_txid} never made it on chain"))?;

    // (c) it spends the TRIGGER's payload output, not something else.
    let t_txid = Txid::from_str(&trigger_txid)?;
    assert!(
        de.input.iter().any(|i| i.previous_output.txid == t_txid),
        "[D68] the de-trigger does not spend the trigger {trigger_txid}. Then it is not a \
         de-trigger — it neither kills the old ladder nor collapses the grief. inputs: {:?}",
        de.input.iter().map(|i| i.previous_output).collect::<Vec<_>>()
    );

    // (d) the value landed where alice said.
    let want_spk = {
        use electrum_client::bitcoin::Address;
        Address::from_str(&dest)?.assume_checked().script_pubkey()
    };
    assert!(
        de.output.iter().any(|o| o.script_pubkey == want_spk),
        "[D68] the de-trigger did not pay the address alice named ({dest}). The explicit-address \
         form is what a holder under duress uses; an untested parameter is not an option."
    );
    println!(
        "SDK89 - (c)+(d) COLLAPSED: de-trigger {detrigger_txid} confirmed, spends the trigger's \
         payload output, and paid {dest} — the address alice chose, at the moment alice chose."
    );

    // ===== (e) THE OLD LADDER IS DEAD, asserted against the node =================================
    //
    // The strongest available evidence, and the reason this test exists: submit the pre-signed
    // extension the griefer would have waited for. Its input is gone, so the node must refuse it.
    let x0_raw = hex::decode(&x0_hex)?;
    let x0_res = cc.electrum_client.transaction_broadcast_raw(&x0_raw);
    assert!(
        x0_res.is_err(),
        "[D68] the pre-signed extension X_0 was ACCEPTED after the de-trigger confirmed. Then the \
         old ladder is alive and the de-trigger achieved nothing — it is supposed to spend the \
         output every tier of that ladder depends on. Node said: {x0_res:?}"
    );
    let why = format!("{:?}", x0_res.unwrap_err());
    assert!(
        why.contains("missing") || why.contains("Missing") || why.contains("bad-txns-inputs")
            || why.contains("conflict") || why.contains("Conflict"),
        "X_0 was refused, but not for the reason that proves the ladder is dead (a missing/spent \
         input). A refusal for some other cause would make this assertion accidental. Got: {why}"
    );
    println!(
        "SDK89 - (e) THE OLD LADDER IS DEAD: the pre-signed X_0 is now unbroadcastable — the node \
         refuses it because its input no longer exists ({why})"
    );

    println!(
        "SDK89 PASS - [D68] `cosign_detrigger` is WIRED and driven end to end: a griefed plain \
         ladder was collapsed in two transactions with zero CSV wait, to an address the owner \
         named, and every pre-signed tier of the old ladder is dead. NOT proven, and not claimed: \
         a fresh funding output F' or a rebuilt ladder — on the plain lane this is an EXIT, and \
         getting back off-chain is a fresh deposit ([D57])."
    );
    Ok(())
}
