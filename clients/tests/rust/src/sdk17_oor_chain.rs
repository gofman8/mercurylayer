//! E2E (parity): **out-of-round transferability** — the defining Ark/Arkade OOR property. Value moves
//! through MULTIPLE off-chain hops (alice -> bob -> carol) with ZERO on-chain footprint; the original
//! funding outpoint stays unspent the whole time. Only the final unilateral exit touches the chain.
//! This is our statechain equivalent of chaining out-of-round Ark VTXO transfers and redeeming at the
//! end, and of Spark's off-chain leaf transfers.
//!
//! Run: SDK_E2E=17 ML_NETWORK=regtest cargo run

use std::str::FromStr;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> bool {
    use electrum_client::bitcoin::Txid;
    let raw = match cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()) {
        std::result::Result::Ok(r) => r,
        _ => return true,
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk).unwrap_or_default();
    !listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout)
}

async fn claim_one(w: &UtexoWallet) -> Result<()> {
    for _ in 0..30 {
        if w.claim().await?.claimed_transfers >= 1 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(anyhow!("receiver claimed no transfer"))
}

pub async fn execute() -> Result<()> {
    // STILL V1-PINNED — the last one, and the reason is precise. Hop 2 below sends 10 000 of Bob's
    // 20 000 RECEIVED sub-coin: a NON-EXACT split of a child. Children became first-class on V2
    // (V2-CHILD-FIRSTCLASS.md Commit B — sdk60 moves a whole child alice->bob->carol off-chain), but a
    // child-level in-ladder SPLIT needs a depth-2 ancestor chain in the bundle, which is Commit C
    // (see docs/utexo/V2-CHILD-FIRSTCLASS-PLAN.md §C1-C6). Un-pinned today this fails cleanly with
    // "insufficient balance" — the planner correctly refuses to split a child rather than falling
    // through to the B1-unsafe plain split. Do NOT un-pin by making hop 2 a whole-child transfer:
    // that would silently drop this test'"'"'s partial-second-hop coverage (sdk60 already covers whole).
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk17_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk17_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk17_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    // alice deposits — this is the ONLY on-chain funding tx in the whole flow.
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
    let deposit = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk17_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(40_000))
        .ok_or_else(|| anyhow!("no deposit"))?
        .clone();
    let o_txid = deposit.utxo_txid.clone().unwrap();
    let o_vout = deposit.utxo_vout.unwrap();
    println!("SDK17 - alice deposited 40k; funding outpoint O = {o_txid}:{o_vout} (the only on-chain tx)");

    // --- OOR hop 1: alice -> bob (off-chain) ------------------------------------------------------
    for _ in 0..2 { let t = prepaid_token(&cc).await?; alice.add_prepaid_token(&t).await; }
    let r1 = alice.transfer(&bob_addr, 20_000).await?;
    assert!(r1.used_split);
    claim_one(&bob).await?;
    assert_eq!(bob.get_balance().await?.available_sats, 20_000, "bob got 20k off-chain");
    assert!(!is_outpoint_spent(&cc, &o_txid, o_vout), "hop 1 was OUT-OF-ROUND: O still unspent");
    println!("SDK17 - OOR hop 1: alice -> bob 20k, off-chain (O still unspent) \u{2713}");

    // --- OOR hop 2: bob -> carol (off-chain) — bob re-spends his received sub-coin ----------------
    for _ in 0..2 { let t = prepaid_token(&cc).await?; alice.add_prepaid_token(&t).await; }
    // (carol/bob need a spend token too; add to bob's wallet)
    for _ in 0..2 { let t = prepaid_token(&cc).await?; bob.add_prepaid_token(&t).await; }
    let r2 = bob.transfer(&carol_addr, 10_000).await?;
    assert!(r2.used_split);
    claim_one(&carol).await?;
    assert_eq!(carol.get_balance().await?.available_sats, 10_000, "carol got 10k off-chain");
    assert!(!is_outpoint_spent(&cc, &o_txid, o_vout), "hop 2 was OUT-OF-ROUND: O STILL unspent after two hops");
    println!("SDK17 - OOR hop 2: bob -> carol 10k, off-chain (O STILL unspent after 2 hops) \u{2713}");

    // --- Only the final unilateral exit touches the chain -----------------------------------------
    let carol_coin = r2.coins[0].statechain_id.clone();
    let _ = carol.unilateral_exit(Some(vec![carol_coin.clone()]), None).await?;
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(is_outpoint_spent(&cc, &o_txid, o_vout), "carol's exit finally spends O on-chain");
    println!("SDK17 - carol exited unilaterally -> O is finally spent on-chain (the branch broadcast) \u{2713}");

    println!("SDK17 - SUCCESS: value moved alice -> bob -> carol across TWO out-of-round hops with ZERO on-chain footprint (the funding outpoint stayed unspent the whole time); only carol's final unilateral exit touched the chain. This is the Ark/Arkade out-of-round + redeem semantics (and Spark's off-chain leaf transfer) achieved over a Mercury statechain.");
    Ok(())
}
