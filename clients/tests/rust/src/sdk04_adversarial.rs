//! E2E (adversarial): SDK guard rails — the Utexo negative cases the SDK itself owns, on the
//! laddered (TES-R) coin shape.
//!
//! 1. InsufficientBalance: transfer above balance is refused with a typed error.
//! 2. Double-spend of a split parent: a non-exact payment out of a laddered coin runs the IN-LADDER
//!    split (`transfer()` auto-routes to `in_ladder_pay`), which TERMINALIZES the parent node at the
//!    SE (spend budget consumed by `SP`) and books it WITHDRAWN locally. The parent is then
//!    unspendable twice over — the wallet refuses a second full-value spend, and the SE refuses a
//!    second in-ladder split of the same node.
//! 3. Claim idempotence: repeated claim passes are no-ops (no double-booking).
//! 4. Withdraw of an already-withdrawn coin is refused.
//!
//! (Protocol-level adversarial cases live elsewhere: tm01 sender double-spend, ta02/ta03
//! duplicate deposits, RGB_E2E=4 SE single-use refusal, RGB_E2E=7 epoch deadline, tb04/SDK_E2E=3
//! latch locking; sdk58 covers the verifier-side `verify_child_bundle` attacks and sdk59 the happy
//! in-ladder payment path.) Run: SDK_E2E=4 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{transfer::InLadderLatch, SdkConfig, UtexoWallet};
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
    // TES-R ladders every fresh root coin: `claim()` auto-establishes an exit ladder on a fresh
    // deposit, so alice's coin is LADDERED. A laddered coin can never be self-split as plain BTC
    // ([B1] — a prior owner's no-timelock trigger could void the split), so a non-exact payment is
    // routed to the IN-LADDER split instead: `SP` descends from the trigger, the piece child pays
    // the recipient (Model A) and is conveyed to their mailbox, the change child pays us back.
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk4_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk4_bob"), None).await?;
    let bob_address = bob.get_utexo_address().await?;

    // Fund alice with one 40k coin.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(40_000).await?;
    bitcoin_core::sendtoaddress(40_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        // claim() confirms the deposit AND establishes its ladder in the same pass, so checking the
        // balance only after the claim guarantees the ladder is in place once we break.
        alice.claim().await?;
        if alice.get_balance().await?.available_sats == 40_000 {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK04 - alice funded (40k)");

    // --- 1. InsufficientBalance is a typed refusal ---------------------------------------------
    let err = alice.transfer(&bob_address, 100_000).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("insufficient balance"), "typed error, got: {msg}");
    println!("SDK04 - over-balance transfer refused: {msg}");

    // --- 2. In-ladder split, then try to double-spend the parent --------------------------------
    let record = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk4_alice").await?;
    let parent_id = record
        .coins
        .iter()
        .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED)
        .and_then(|c| c.statechain_id.clone())
        .ok_or_else(|| anyhow!("no confirmed coin"))?;
    // Fail loudly rather than silently testing the un-laddered plain-BTC split path: this case only means
    // anything if the parent actually carries a TES-R ladder.
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk4_alice", &parent_id).await?.is_some(),
        "alice's coin must carry a TES-R ladder — otherwise this is not the in-ladder split path"
    );
    // No "split slot" top-ups needed in-ladder: both child slots are funded by FREE derived tokens.
    // The split IS the payment — the 15k piece child is conveyed straight to bob (Model A), so
    // there is no separate hand-off of the piece afterwards.
    let r = alice.transfer(&bob_address, 15_000).await?;
    assert!(
        r.used_split,
        "a 15k payment out of a single 40k laddered coin must go through the IN-LADDER split"
    );
    assert_eq!(r.total_sats, 15_000);
    println!("SDK04 - split parent in-ladder: 15k piece conveyed to bob, change kept (off-chain)");

    // The parent is terminally spent. Two independent refusals must hold:
    //
    // (a) SE-level truth — the parent statechain node is TERMINAL (budget = finalized, consumed by
    //     SP), so the SE will co-sign nothing further over it. This is what stops an off-chain fork.
    let (_, _, parent_terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_id).await?;
    assert!(
        parent_terminal,
        "the split parent must be TERMINAL at the SE (budget=1, consumed by SP)"
    );

    // (b) SDK-level — a second spend attempt through the wallet is refused. The full parent value is
    //     no longer available (only the change child remains), so the whole-value re-spend is a typed
    //     insufficient-balance refusal...
    let err = alice
        .transfer(&bob_address, 40_000)
        .await
        .map(|_| ())
        .unwrap_err();
    println!("SDK04 - double-spend of the split parent refused: {err}");
    // ...and, faithfully to the pre-TES-R property this replaces, a direct second split of the SAME parent
    // statechain id is refused too — the SE refuses to co-sign a second SP over a terminal node.
    // This is the load-bearing assertion: without it the parent could yield a second payment.
    let err = alice
        .in_ladder_pay(&parent_id, &bob_address, 5_000, InLadderLatch::None)
        .await
        .map(|_| ())
        .unwrap_err();
    // `in_ladder_pay` can bail for several PLUMBING reasons before it ever asks the SE to co-sign a
    // second SP (missing ladder, fee/dust floors, derived-token issuance, slot creation). Any of those
    // would make the refusal above pass VACUOUSLY and silently stop testing terminality. Pin the cause
    // negatively so only the real security refusal (a terminal parent the SE will not co-sign over)
    // can satisfy this assertion.
    let msg = err.to_string().to_lowercase();
    for plumbing in [
        "has no tes-r ladder",
        "committed fee too high",
        "leaves no change",
        "derived",
        "token id not found",
        "child slot coin not found",
    ] {
        assert!(
            !msg.contains(plumbing),
            "the second-split refusal must come from the SE refusing to co-sign over a TERMINAL parent, \
             but it failed earlier for a plumbing reason ({plumbing:?}) — this assertion would pass vacuously: {err}"
        );
    }
    println!("SDK04 - second in-ladder split of the terminal parent refused (not a plumbing bail): {err}");

    // The change stays with alice as a usable (exit-only) child claim.
    let alice_change = alice.get_balance().await?.available_sats;
    assert!(
        alice_change > 0 && alice_change < 40_000,
        "alice keeps the split change as a spendable balance ({alice_change})"
    );
    println!("SDK04 - alice keeps {alice_change} sat of change after the split payment");

    // --- 3. Claim idempotence -------------------------------------------------------------------
    // The piece child arrives by mailbox conveyance (asynchronous), not a synchronous hand-off, so
    // poll until the first claim lands — then the repeat passes must be no-ops.
    let mut waited = 0;
    let first_claimed = loop {
        let r = bob.claim().await?;
        if r.claimed_transfers > 0 {
            break r.claimed_transfers;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("bob never received the conveyed piece child"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    assert_eq!(first_claimed, 1, "bob claims the piece once");
    let second = bob.claim().await?;
    assert_eq!(second.claimed_transfers, 0, "second claim pass is a no-op");
    let third = bob.claim().await?;
    assert_eq!(third.claimed_transfers, 0, "third claim pass is a no-op");
    assert_eq!(bob.get_balance().await?.available_sats, 15_000, "no double-booking");
    println!("SDK04 - claim is idempotent (1 claim, then no-ops; balance booked once)");

    // --- 4. Withdrawing the same coin twice is refused ------------------------------------------
    // Bob's piece is a received in-ladder CHILD: exit-only (its funding SP.out[j] is un-broadcast and
    // bob holds no SE co-signing key for it), so `withdraw` routes it to the unilateral exit and the
    // value lands at BOB's own key — the `exit_addr` argument is not used on that route. The coin is
    // then WITHDRAWING, and a second withdraw of it must be refused rather than start a rival exit.
    let exit_addr = bitcoin_core::getnewaddress()?;
    let withdrawn = bob.withdraw(&exit_addr, None, None).await?;
    assert_eq!(withdrawn.len(), 1);
    let err = bob
        .withdraw(&exit_addr, Some(withdrawn.clone()), None)
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(
        err.to_string().contains("not CONFIRMED"),
        "the second withdraw must be refused because the coin is already exiting, got: {err}"
    );
    println!("SDK04 - double-withdraw refused: {err}");

    println!("SDK04 - SUCCESS: typed insufficient-balance refusal; in-ladder split parent is terminal at the SE and refuses a second spend; idempotent claims (no double-booking); double-withdraw refusal.");
    Ok(())
}
