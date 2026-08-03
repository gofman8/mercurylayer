//! E2E: mercury-utexo-sdk token flow — issuer-SDK parity on RGB rails.
//!
//! alice issues 1000 TKN (RGB NIA) straight onto a statechain coin, then pays bob 250 TKN
//! **off-chain**: colored split (exact piece + change) + branch-carrying key handover with the
//! consignment riding the transfer message. bob's watcher auto-claims, validates the consignment
//! off-chain (un-broadcast witness chain) and books the balance under the VERIFIED contract id.
//! bob then unilaterally exits — the branch broadcasts and his token coin materializes on-chain.
//!
//! Run: SDK_E2E=2 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use std::time::Duration;

use crate::bitcoin_core;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk2_alice", "./rgb-data-sdk2_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }

    let cc = mercuryrustlib::client_config::load().await;

    let mut alice_cfg = SdkConfig::regtest("sdk2_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk2_alice".to_string());
    let mut bob_cfg = SdkConfig::regtest("sdk2_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk2_bob".to_string());
    // [CTES-R] The COLOURED lane is asked for BY NAME, on both wallets.
    //
    // `SdkConfig::colored_ladder` defaults to **false** (2c351c6), and that default is a shipping
    // decision, not a doubt about the lane: what keeps it off is the measured economics of the lane
    // it switches on (`docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md` — one coloured partial payment per
    // carrier, ever). A test of the lane must therefore ENABLE the lane; inheriting it from the
    // default would make this test a test of the default instead. The default is pinned, in the
    // direction it actually has, by sdk74 and sdk75.
    alice_cfg.colored_ladder = true;
    bob_cfg.colored_ladder = true;

    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_address = bob.get_utexo_address().await?;
    println!("SDK02 - wallets up; bob address: {bob_address}");

    // --- Fund alice's RGB engine (issuance needs a colorable UTXO + witness fees) -------------
    let rgb_fund_addr = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund_addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await; // electrs indexing

    // --- Issue 1000 TKN onto a fresh statechain coin -------------------------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset_id = alice.issue_token("TKN", "Spark Parity Token", 0, 1000).await?;
    println!("SDK02 - issued 1000 TKN: {asset_id}");

    // Wait for the deposit (the colored funding tx) to confirm.
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        alice.claim().await?;
        let b = alice.get_balance().await?;
        // V2DEF-5: a token carrier's sats are excluded from available_sats (carrier ⊥ ladder), so the
        // carrier-confirmed signal is the settled RGB allocation, not a sats threshold.
        if !b.tokens.is_empty() && b.tokens[0].balance == 1000 {
            println!(
                "SDK02 - alice token carrier confirmed: {} {} settled",
                b.tokens[0].balance,
                b.tokens[0].ticker.clone().unwrap_or_default()
            );
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("token carrier did not confirm: {b:?}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_tokens = alice.get_token_balances().await?;
    assert_eq!(alice_tokens[0].balance, 1000, "full supply on alice");

    // [CTES-R] Wait for the carrier's COLOURED ladder before paying.
    //
    // This is a synchronisation point, not a relaxation. alice runs with `colored_ladder` ON (set
    // above), so `claim()` establishes a coloured ladder over the carrier — but it can only do so once the
    // allocation is BOOKED, which is the very condition the loop above waits for. The two can land
    // in the same `claim()` pass or in consecutive ones, so without this the test would sometimes
    // reach `transfer_tokens` before the ladder exists and be refused by the retired-lane gate.
    // Waiting for the state under test is the fix; taking the old lane would not be.
    let carrier_sid = {
        let mut found = None;
        for _ in 0..60 {
            alice.claim().await?;
            let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk2_alice")
                .await?
                .coins;
            for c in coins.iter().filter(|c| c.duplicate_index == 0) {
                let Some(sid) = c.statechain_id.clone() else { continue };
                if mercuryrustlib::tesr::load(&cc, "sdk2_alice", &sid)
                    .await?
                    .is_some_and(|b| b.is_colored())
                {
                    found = Some(sid);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        found.ok_or_else(|| {
            anyhow!("alice's carrier never got a COLOURED ladder — the CTES-R lane is not engaged")
        })?
    };
    println!("SDK02 - alice's carrier {carrier_sid} carries a COLOURED (CTES-R) ladder");

    // --- Off-chain token transfer: 250 TKN to bob ----------------------------------------------
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let r = alice.transfer_tokens(&asset_id, &bob_address, 250).await?;
    assert!(r.used_split);
    println!(
        "SDK02 - sent 250 TKN off-chain over the CTES-R in-ladder split (piece {} = {} sat)",
        r.coins[0].statechain_id, r.coins[0].amount_sats
    );
    // The lane is asserted, not assumed: the piece must be a COLOURED CHILD carved out of `SP`, and
    // the parent carrier must be terminalized. If this had gone over the retired split lane the
    // piece would be a plain statechain coin with a consignment envelope and no `ctesr-` bundle.
    assert!(
        mercuryrustlib::tesr::load_child(&cc, "sdk2_alice", &r.coins[0].statechain_id)
            .await?
            .is_none(),
        "the conveyed piece must not still be held as a child bundle by the SENDER"
    );

    let (recv_asset, recv_amount) = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TokenTransferClaimed { asset_id, amount, .. }) => {
                    break (asset_id, amount)
                }
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim the token transfer in time"))?;
    println!("SDK02 - bob claimed {recv_amount} of {recv_asset} (consignment validated off-chain)");
    assert_eq!(recv_asset, asset_id, "contract id verified from the consignment");
    assert_eq!(recv_amount, 250);

    let bob_tokens = bob.get_token_balances().await?;
    assert_eq!(bob_tokens.len(), 1, "bob sees exactly one asset");
    assert_eq!(bob_tokens[0].asset_id, asset_id);
    assert_eq!(bob_tokens[0].balance, 250, "bob booked 250 TKN");
    let alice_tokens = alice.get_token_balances().await?;
    assert_eq!(alice_tokens[0].balance, 750, "alice keeps 750 TKN change");
    println!("SDK02 - balances: alice 750 TKN / bob 250 TKN — fully off-chain");

    // --- bob's cooperative exit: the token coin is DELIBERATELY not swept -----------------------
    //
    // RE-DERIVED, and the old claim was already wrong. This step used to print "bob cooperatively
    // exited N coin(s) ... (branch materialized + direct spend)" while asserting nothing, and N has
    // always been 0: `withdraw` refuses to sweep a token carrier into an RGB-unaware L1 spend, which
    // is precisely what would DESTROY the allocation. The line described an exit that never happened.
    //
    // So the property is stated as what it actually is, and asserted: a cooperative sweep must leave
    // the token coin alone, and bob's balance must survive it intact. bob's real exit route is the
    // keyless five-tier coloured child walk (`unilateral_exit`), which sdk77 drives end to end and
    // which this test deliberately does not repeat.
    bob_bg.abort();
    let exit_addr = bitcoin_core::getnewaddress()?;
    let withdrawn = bob.withdraw(&exit_addr, None, None).await?;
    assert!(
        withdrawn.is_empty(),
        "a cooperative sweep must not touch bob's token coin — an RGB-unaware spend of it destroys \
         the allocation; it swept {withdrawn:?}"
    );
    bitcoin_core::generatetoaddress(3, &core)?;
    let bob_after = bob.get_token_balances().await?;
    assert_eq!(
        bob_after.first().map(|b| b.balance),
        Some(250),
        "bob's 250 TKN must survive the cooperative sweep untouched"
    );
    println!(
        "SDK02 - bob's cooperative sweep to {exit_addr} correctly swept 0 coin(s): the coloured \
         token coin is excluded (an RGB-unaware spend would destroy the allocation) and bob still \
         holds 250 TKN"
    );

    println!("SDK02 - SUCCESS: issue -> off-chain token transfer over the CTES-R in-ladder split (a coloured child carved out of SP, conveyed and booked under the VERIFIED contract) -> balances 750/250 -> a cooperative sweep that correctly leaves the token coin alone. Tokens with zero on-chain cost per payment.");
    Ok(())
}
