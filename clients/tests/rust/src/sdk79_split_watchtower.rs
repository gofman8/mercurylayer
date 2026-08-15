//! E2E (SDK_E2E=79) — **[B2] the sender's own watchtower must not destroy the recipient's allocation.**
//!
//! A coloured in-ladder split builds a terminalized PARENT SEGMENT: `SP` becomes the parent's current
//! state and the `S_0` it replaces is disclosed as superseded. Every child bundle carries a copy of
//! that segment, because the child's whole exit chain hangs off it. **Nothing wrote it back to the
//! SENDER's own `tesr-<parent>` row.** So after paying, alice's row still named `S_0` as live — and
//! `defend_ladders` drives exactly those rows, with no status filter of any kind.
//!
//! `S_0` and `SP` are RIVAL spends of `X_m`'s payload output. On the coloured lane the recipient's
//! RGB assignment lives on `SP`. So the moment anyone triggered the carrier, alice's own watchtower
//! would have broadcast `S_0`, raced the child it had just paid for, and destroyed bob's allocation —
//! a third party's loss, caused by the protection mechanism.
//!
//! Two independent fixes, and this test asserts both, plus the behaviour they exist for:
//!
//! (1) PERSISTED. `colored_in_ladder_pay` now stores the terminalized segment back to `tesr-<parent>`
//!     BEFORE conveying anything, so the window in which a recipient holds a bundle while the sender's
//!     tower is armed against it does not exist. Asserted by reading alice's row: its current state is
//!     `SP` — byte-identical to the state named in bob's conveyed child — and the old `S_0` appears in
//!     `superseded_states`, not in the exit chain. The sender's tower is now the child's ALLY: its
//!     `exit_tiers()` are exactly `T -> X_m -> SP`, the three transactions bob's child chain needs
//!     underneath it.
//!
//! (2) FILTERED. `defend_ladders` no longer acts on a ladder the wallet has already spent. Asserted
//!     with the carrier actually TRIGGERED on chain — the only state in which the pass does anything
//!     at all — by driving alice's tower to quiescence and then checking, on the CHAIN, that `S_0` was
//!     never broadcast while bob's `SP` was.
//!
//! The order matters: the trigger is broadcast from the STORED bundle (no key material, no SE), which
//! is what makes step (2) a real contested exit rather than a simulated one.
//!
//! ## PART B — [D1] the same theft by the OTHER route: a WHOLE-CARRIER conveyance
//!
//! Part A's filter was two checks: a DENYLIST on WITHDRAWN/WITHDRAWING, and a supersession check
//! driven by this wallet's `ctesr-` rows. Neither one closes a whole-coin handover:
//!
//!   * `transfer_colored_carrier` co-signs a receiver-paying `S'` over the SAME `X_m.out[0]` the
//!     sender's own retained `S` spends — the identical rival-state shape as `S_0` vs `SP`, one
//!     level up — and the recipient's RGB assignment lives on `S'`;
//!   * it writes NO `ctesr-` row (there is no child), so the supersession check has nothing to see;
//!   * and `transfer_sender::execute_colored` leaves the sender's coin **IN_TRANSFER**, which the
//!     denylist ADMITS (as does the TRANSFERRED that `update_coins` later promotes it to).
//!
//! So the sender's own watchtower would still broadcast `S`, race the recipient it had just paid in
//! full, and destroy the whole allocation. Part B drives exactly that, with the carrier genuinely
//! TRIGGERED on chain and with enough blocks mined for the sender's own tiers to mature — 12 for
//! `X_0`, then 24 more for her `S` — so "she did not broadcast it" is a refusal and not a wait.
//!
//! It asserts the DEFECT is still present and only the filter stands between it and the recipient:
//! alice's `tesr-` row still names her own `S` as live (nothing rewrote it), her coin's status is
//! neither WITHDRAWN nor WITHDRAWING (so the Part A denylist would have let it through), and yet
//! across ten passes her tower never touches it and `S` never reaches the chain — while bob's tower,
//! driving the same `X_m` output, lands `S'`.
//!
//! The fix under test is therefore not a third enumerated lane: `defend_ladders` now keys on
//! LIVENESS as an ALLOWLIST — it broadcasts only for a coin this wallet still holds CONFIRMED —
//! which is evidence the tower checks for itself and which every conveyance lane, present or future,
//! must invalidate to hand value away at all.
//!
//! ## PARTS C AND D — [D1 / A2] the key the filter reads must be DURABLE
//!
//! Parts A and B both assert AFTER the conveyance returned, and that is where they stop proving
//! anything: L1 reads `coin.status` FROM THE WALLET DB, and the whole-coin lane only ever set it in
//! MEMORY (`create_backup_transactions`), persisting at `update_wallet` — the last statement of
//! `execute_ex`, long after `transfer/update_msg` handed the recipient a co-signed `S'`. Part C
//! RACES a real, lethal `defend_ladders` against `transfer_colored_carrier` to close that. Part D
//! does the same one level down, on the coloured CHILD re-transfer, where the sender's coin stays
//! CONFIRMED for the entire hop and the `ctesr-` row's CONTENT is what decides what gets broadcast.
//! Both races are gated on the SE's own `num_sigs` counter — a marker independent of the fix — and
//! both are backed by a millisecond-resolution witness of the exact join L1 performs. See the long
//! note above `se_num_sigs` for why each of those three properties is load-bearing.
//!
//! Run: SDK_E2E=79 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)
//! Select parts with SDK79_PARTS (default "abcd"), e.g. SDK79_PARTS=cd for the two race cases.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

const SUPPLY: u64 = 1_000;
const PAY: u64 = 250;
/// Part B's asset — a SECOND carrier, conveyed WHOLE.
const SUPPLY_B: u64 = 700;
/// Part C's asset — a THIRD carrier, conveyed WHOLE while a watchtower pass RACES the conveyance.
const SUPPLY_C: u64 = 800;
/// Part D's asset — a FOURTH carrier, split in-ladder so the child can be RE-TRANSFERRED onward
/// while a watchtower pass races that hop.
const SUPPLY_D: u64 = 1_000;
const PAY_D: u64 = 250;

/// Regtest TES-R schedule, read from the protocol rather than copied: `X_0` matures `ext_csv(0)`
/// blocks after the trigger confirms, and the sender's own `S_0` a further `state_csv(0)` after
/// that. Part B mines strictly more than their sum so that "alice's tower did not broadcast `S`" is
/// a refusal rather than an immature CSV.
fn sender_exit_blocks() -> u32 {
    let p = mercurylib::tesr::TesrParams::regtest();
    (p.ext_csv(0) as u32) + (p.state_csv(0) as u32)
}

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

fn onchain(cc: &ClientConfig, txid: &str) -> Option<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::Txid;
    use electrum_client::ElectrumApi;
    cc.electrum_client.transaction_get(&Txid::from_str(txid).ok()?).ok()
}

fn tip(cc: &ClientConfig) -> Result<usize> {
    use electrum_client::ElectrumApi;
    Ok(cc.electrum_client.block_headers_subscribe()?.height)
}

fn mine_synced(cc: &ClientConfig, core: &str, n: u32) -> Result<()> {
    for _ in 0..n {
        let before = tip(cc)?;
        bitcoin_core::generatetoaddress(1, core)?;
        for _ in 0..60 {
            if tip(cc)? > before {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    Ok(())
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk79_alice", "./rgb-data-sdk79_bob", "./rgb-data-sdk79_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;
    let mut alice_cfg = SdkConfig::regtest("sdk79_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk79_alice".to_string());
    // The watchtower is driven EXPLICITLY here; a background pass would make "which pass broadcast
    // what" unanswerable.
    alice_cfg.auto_exit = false;
    // [CTES-R] The COLOURED lane is asked for BY NAME, on every wallet in this test.
    //
    // `SdkConfig::colored_ladder` ships **false** (2c351c6) — the lane is sound (sdk74/sdk75) but
    // its measured economics keep it opt-in (`docs/utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md`). Every
    // theft this test drives is SPECIFIC to that lane: `S_0` vs `SP`, `S` vs `S'`, and the child
    // re-transfer are all rival COLOURED states over one `X_m` payload output, and none of them
    // exists on the flat lane. So the lane is enabled here rather than inherited — with it off,
    // `find_colored_carrier` would simply never find a carrier and every part would hang, and if it
    // somehow did not, the counterfactuals below would pass vacuously. Nothing about what this test
    // proves depends on which way the default points.
    alice_cfg.colored_ladder = true;
    let mut bob_cfg = SdkConfig::regtest("sdk79_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk79_bob".to_string());
    bob_cfg.colored_ladder = true;
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_address = bob.get_utexo_address().await?;

    let core = bitcoin_core::getnewaddress()?;
    let rgb_fund = alice.get_token_funding_address().await?;
    // Four issuances now (A, B, C, D), so the issuer's on-chain float is raised to match.
    bitcoin_core::sendtoaddress(1_000_000, &rgb_fund)?;
    mine_synced(&cc, &core, 3)?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Four slots: the Part A issuance, the Part B issuance, and headroom for the claims each needs.
    // Parts C and D top themselves up.
    for _ in 0..4 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let parts = std::env::var("SDK79_PARTS").unwrap_or_else(|_| "abcd".to_string());
    if parts.contains('a') {
        part_ab(&cc, &alice, &bob, &bob_address, &core).await?;
    } else {
        println!("SDK79 - PARTS A/B skipped (SDK79_PARTS={parts})");
    }
    if parts.contains('c') {
        part_c(&cc, &alice, &bob, &bob_address, &core).await?;
    } else {
        println!("SDK79 - PART C skipped (SDK79_PARTS={parts})");
    }
    if parts.contains('d') {
        part_d(&cc, &alice, &bob, &bob_address, &core).await?;
    } else {
        println!("SDK79 - PART D skipped (SDK79_PARTS={parts})");
    }
    Ok(())
}

/// PART A + PART B, unchanged, lifted verbatim into their own function so `SDK79_PARTS` can select
/// them. Everything they need is created by `execute`; nothing after them depends on their locals.
async fn part_ab(
    cc: &ClientConfig,
    alice: &UtexoWallet,
    bob: &UtexoWallet,
    bob_address: &str,
    core: &str,
) -> Result<()> {
    let asset_id = alice.issue_token("WTWR", "Watchtower Token", 0, SUPPLY).await?;
    mine_synced(&cc, &core, 3)?;

    // ---- 1. A COLOURED carrier. ------------------------------------------------------------------
    let mut carrier_sid = String::new();
    for _ in 0..90 {
        alice.claim().await?;
        let coins =
            mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk79_alice").await?.coins;
        for c in coins.iter().filter(|c| {
            c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0
        }) {
            let Some(sid) = c.statechain_id.clone() else { continue };
            if mercuryrustlib::tesr::load(&cc, "sdk79_alice", &sid)
                .await?
                .is_some_and(|b| b.is_colored())
            {
                carrier_sid = sid;
                break;
            }
        }
        if !carrier_sid.is_empty() {
            break;
        }
        mine_synced(&cc, &core, 1)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(!carrier_sid.is_empty(), "alice's carrier never got a COLOURED ladder");

    // The state that MUST NEVER be broadcast after the split — captured before it happens.
    let pre = mercuryrustlib::tesr::load(&cc, "sdk79_alice", &carrier_sid)
        .await?
        .ok_or_else(|| anyhow!("the coloured ladder vanished"))?;
    let s0_txid = pre.current().state.txid.clone();
    let trigger_txid = pre.trigger.txid.clone();
    let x_m_txid = pre.current().extension.txid.clone();
    let trigger_hex = pre.trigger.signed_tx.clone();
    println!(
        "SDK79 - alice's coloured carrier {carrier_sid}: T {} -> X_0 {} -> S_0 {} (the state the \
         split is about to supersede)",
        &trigger_txid[..12],
        &x_m_txid[..12],
        &s0_txid[..12]
    );

    // ---- 2. The coloured in-ladder pay. ----------------------------------------------------------
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        bob.add_prepaid_token(&t).await;
    }
    let bob_bg = bob.start_background();
    let out = alice.transfer_tokens(&asset_id, &bob_address, PAY).await?;
    let piece_sid = out.coins[0].statechain_id.clone();
    for _ in 0..60 {
        alice.claim().await?;
        if bob
            .get_token_balances()
            .await?
            .iter()
            .any(|b| b.asset_id == asset_id && b.balance == PAY)
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let bob_child = mercuryrustlib::tesr::load_child(&cc, "sdk79_bob", &piece_sid)
        .await?
        .ok_or_else(|| anyhow!("bob never adopted the coloured child {piece_sid}"))?;
    let sp_txid = bob_child.parent.current().state.txid.clone();
    assert_ne!(sp_txid, s0_txid, "SP must be a different state from the one it supersedes");
    println!("SDK79 - alice paid bob {PAY}; bob's child names SP {} as the parent's live state", &sp_txid[..12]);

    // ---- 3. (1) PERSISTED: alice's own row agrees with the bundle she conveyed. -------------------
    let post = mercuryrustlib::tesr::load(&cc, "sdk79_alice", &carrier_sid)
        .await?
        .ok_or_else(|| anyhow!("alice's ladder row disappeared after the split"))?;
    assert_eq!(
        post.current().state.txid,
        sp_txid,
        "THE B2 DEFECT: alice's own `tesr-{carrier_sid}` row still names {} as the live state while \
         bob's child names {sp_txid}. Her watchtower drives THIS row, so it would broadcast a state \
         that rivals bob's SP over X_m's payload output and destroys his allocation.",
        post.current().state.txid
    );
    assert!(
        post.superseded_states.iter().any(|t| t.txid == s0_txid),
        "the superseded S_0 ({s0_txid}) must be disclosed in the sender's own row, not silently \
         dropped — it is the state a griefer could still try to use"
    );
    let exit_txids: Vec<String> = post.exit_tiers().iter().map(|t| t.txid.clone()).collect();
    assert!(
        !exit_txids.contains(&s0_txid),
        "the superseded state must be OUT of the sender's exit chain: {exit_txids:?}"
    );
    assert_eq!(
        exit_txids,
        vec![trigger_txid.clone(), x_m_txid.clone(), sp_txid.clone()],
        "the sender's tower must now drive exactly T -> X_m -> SP, i.e. the three transactions bob's \
         child chain needs underneath it — an ally, not a rival"
    );
    println!(
        "SDK79 - (1) PERSISTED: alice's `tesr-{carrier_sid}` now names SP {} live, S_0 {} \
         superseded, and its exit chain is T -> X_m -> SP",
        &sp_txid[..12],
        &s0_txid[..12]
    );

    // ---- 4. (2) FILTERED, with the carrier genuinely TRIGGERED. -----------------------------------
    //
    // `watch_pass` is a no-op until `F` is spent — an idle laddered coin never ages — so the filter
    // can only be tested in a contested exit. The trigger is broadcast straight from the stored
    // bundle: no keys, no SE, exactly what a prior owner or griefer would do.
    {
        use electrum_client::ElectrumApi;
        let raw = hex::decode(&trigger_hex)?;
        // Idempotent: a trigger already in the mempool is the state we want either way.
        let _ = cc.electrum_client.transaction_broadcast_raw(&raw);
    }
    mine_synced(&cc, &core, 2)?;
    assert!(
        onchain(&cc, &trigger_txid).is_some(),
        "the trigger must be on chain, or the watchtower pass has nothing to react to"
    );

    let parent_status = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk79_alice")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(carrier_sid.as_str()))
        .map(|c| c.status)
        .ok_or_else(|| anyhow!("the parent coin vanished"))?;
    assert_eq!(
        parent_status,
        CoinStatus::WITHDRAWN,
        "a carrier consumed by an in-ladder split is WITHDRAWN — that is the fact the watchtower \
         filter keys on"
    );

    // Drive alice's tower to quiescence. It must refuse to touch the spent parent; anything it does
    // broadcast comes from coins it still owns.
    for _ in 0..6 {
        let acted = alice.defend_ladders().await.map_err(|e| {
            anyhow!("alice's watchtower reported itself BLIND after the split: {e}")
        })?;
        assert!(
            !acted.contains(&carrier_sid),
            "the watchtower acted on {carrier_sid}, a ladder this wallet has already SPENT — its \
             retained state rivals the child it funded"
        );
        mine_synced(&cc, &core, 2)?;
    }
    assert!(
        onchain(&cc, &s0_txid).is_none(),
        "THE B2 THEFT: alice's watchtower broadcast the SUPERSEDED state {s0_txid}. It rivals bob's \
         SP over X_m's payload output, and on the coloured lane bob's RGB assignment lives on SP — \
         confirming S_0 destroys the allocation alice just paid him."
    );
    println!(
        "SDK79 - (2) FILTERED: with the carrier triggered on chain, six watchtower passes never \
         acted on the spent parent and the superseded S_0 {} is still absent from the chain",
        &s0_txid[..12]
    );

    // ---- 5. …and bob's chain really is the one that advances. ------------------------------------
    //
    // The positive half: the segment alice stopped racing is the segment bob's own tower drives,
    // through `watch_child_pass` (T -> X_m -> SP, then his two child tiers).
    for _ in 0..10 {
        let _ = bob.defend_ladders().await;
        if onchain(&cc, &sp_txid).is_some() {
            break;
        }
        mine_synced(&cc, &core, 4)?;
    }
    assert!(
        onchain(&cc, &x_m_txid).is_some(),
        "bob's child tower must have replayed the parent extension X_m"
    );
    assert!(
        onchain(&cc, &sp_txid).is_some(),
        "bob's child tower must have landed SP — the state that carries his allocation"
    );
    assert!(
        onchain(&cc, &s0_txid).is_none(),
        "S_0 must still be absent once SP is on chain: the two are rivals and only one can win"
    );
    drop(bob_bg);

    println!(
        "SDK79 - PART A PASS: after a coloured in-ladder pay the SENDER's ladder row names SP (S_0 \
         superseded and out of the exit chain), her watchtower refuses to act on the spent parent \
         even with the carrier triggered on chain, and the state that actually landed over X_m is \
         bob's SP — the recipient's allocation was never raced by the wallet that paid it."
    );

    // =========================== PART B — [D1] WHOLE-CARRIER CONVEYANCE ==========================
    //
    // Same theft, route the Part A filter did not cover. Everything here is a SECOND asset on a
    // SECOND carrier, so nothing in Part A is reused or disturbed.

    // ---- B1. A second COLOURED carrier. ----------------------------------------------------------
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let asset_b = alice.issue_token("WTW2", "Watchtower Token II", 0, SUPPLY_B).await?;
    mine_synced(&cc, &core, 3)?;

    let mut carrier_b = String::new();
    for _ in 0..90 {
        alice.claim().await?;
        let coins =
            mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk79_alice").await?.coins;
        for c in coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        {
            let Some(sid) = c.statechain_id.clone() else { continue };
            if sid == carrier_sid {
                continue; // Part A's carrier, already consumed
            }
            // Keyed on the CONTRACT, not on "the first coloured ladder": by now alice's Part A
            // change has itself been laddered as a root carrier (bob's tower landed SP, so its
            // funding is on chain), and picking that one would test the wrong coin.
            if mercuryrustlib::tesr::load(&cc, "sdk79_alice", &sid)
                .await?
                .is_some_and(|b| {
                    b.is_colored() && b.rgb.as_ref().is_some_and(|r| r.contract_id == asset_b)
                })
            {
                carrier_b = sid;
                break;
            }
        }
        if !carrier_b.is_empty() {
            break;
        }
        mine_synced(&cc, &core, 1)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(
        !carrier_b.is_empty(),
        "alice's SECOND carrier never got a COLOURED ladder — Part B has nothing to convey"
    );
    assert_ne!(carrier_b, carrier_sid, "Part B must run on a carrier Part A never touched");

    // The three transactions alice retains, captured BEFORE the hop. `s_alice` is the one that must
    // never reach the chain: bob's `S'` spends the same output and carries his whole allocation.
    let pre_b = mercuryrustlib::tesr::load(&cc, "sdk79_alice", &carrier_b)
        .await?
        .ok_or_else(|| anyhow!("the second coloured ladder vanished"))?;
    let s_alice = pre_b.current().state.txid.clone();
    let t_b = pre_b.trigger.txid.clone();
    let x_b = pre_b.current().extension.txid.clone();
    let t_b_hex = pre_b.trigger.signed_tx.clone();
    let alice_holdings = pre_b
        .rgb
        .as_ref()
        .map(|r| r.amount)
        .ok_or_else(|| anyhow!("the second ladder is not coloured"))?;
    assert_eq!(alice_holdings, SUPPLY_B, "the whole supply must sit on the second carrier");
    println!(
        "SDK79 - PART B: alice's second coloured carrier {carrier_b} carries {SUPPLY_B} of \
         {asset_b}: T {} -> X_0 {} -> S(alice) {}",
        &t_b[..12],
        &x_b[..12],
        &s_alice[..12]
    );

    // ---- B2. The WHOLE carrier is conveyed, sats and allocation together. -------------------------
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        bob.add_prepaid_token(&t).await;
    }
    alice.transfer_colored_carrier(&carrier_b, &bob_address).await?;
    let mut bob_b = None;
    for _ in 0..40 {
        bob.claim().await?;
        if let Some(b) = mercuryrustlib::tesr::load(&cc, "sdk79_bob", &carrier_b).await? {
            if b.is_colored() {
                bob_b = Some(b);
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let bob_b = bob_b.ok_or_else(|| anyhow!("bob never adopted the whole coloured carrier"))?;
    let s_bob = bob_b.current().state.txid.clone();
    assert_ne!(
        s_bob, s_alice,
        "the receiver-paying S' must be a DIFFERENT transaction from the state alice retains — \
         otherwise there is no rivalry to test"
    );
    assert_eq!(
        bob_b.current().extension.txid,
        x_b,
        "S' must spend the SAME extension output alice's retained state spends — that is what makes \
         the two rivals, and the whole reason her tower must stand down"
    );
    assert!(
        bob.get_token_balances()
            .await?
            .iter()
            .any(|b| b.asset_id == asset_b && b.balance == SUPPLY_B),
        "bob must have booked the whole {SUPPLY_B} of {asset_b}"
    );

    // ---- B3. THE DEFECT IS STILL THERE — only the filter stands between it and bob. ---------------
    //
    // Nothing rewrote alice's row (a whole-coin handover has no terminalized segment to write back
    // and no child bundle to disagree with it), so she is still holding a fully co-signed exit chain
    // that ends in a state paying HERSELF the coin she just gave away.
    let post_b = mercuryrustlib::tesr::load(&cc, "sdk79_alice", &carrier_b)
        .await?
        .ok_or_else(|| anyhow!("alice's second ladder row disappeared"))?;
    assert_eq!(
        post_b.current().state.txid,
        s_alice,
        "PART B's premise: alice's own row must still name HER state as live. If a later change \
         rewrites it, this test stops proving anything about the watchtower filter and must be \
         re-derived rather than deleted."
    );
    let alice_exit: Vec<String> = post_b.exit_tiers().iter().map(|t| t.txid.clone()).collect();
    assert_eq!(
        alice_exit,
        vec![t_b.clone(), x_b.clone(), s_alice.clone()],
        "alice retains the complete three-tier chain T -> X_0 -> S(alice), all co-signed"
    );

    let alice_status = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk79_alice")
        .await?
        .coins
        .into_iter()
        .find(|c| {
            c.statechain_id.as_deref() == Some(carrier_b.as_str()) && c.duplicate_index == 0
        })
        .map(|c| c.status)
        .ok_or_else(|| anyhow!("alice's conveyed carrier coin vanished"))?;
    // THE NON-VACUITY ASSERTION. Part A's filter skipped WITHDRAWN/WITHDRAWING. A whole-coin
    // handover produces neither, so that filter would have driven this ladder.
    assert!(
        alice_status != CoinStatus::WITHDRAWN && alice_status != CoinStatus::WITHDRAWING,
        "alice's conveyed carrier is {alice_status}, which the previous WITHDRAWN/WITHDRAWING \
         denylist would ALSO have skipped — Part B would then prove nothing. Re-derive this test \
         against whatever status the conveyance now leaves."
    );
    assert_ne!(
        alice_status,
        CoinStatus::CONFIRMED,
        "a conveyed coin must not still read CONFIRMED — that is the evidence the liveness \
         allowlist keys on, and if a conveyance ever left it CONFIRMED the tower would be armed \
         against its own recipient again"
    );
    println!(
        "SDK79 - (B3) alice still holds the whole co-signed chain T -> X_0 -> S(alice) {} for a \
         coin whose status is {alice_status} — a status the old WITHDRAWN/WITHDRAWING denylist \
         admitted. Only the liveness allowlist stops her tower now.",
        &s_alice[..12]
    );

    // ---- B4. TRIGGERED on chain, then alice's tower driven to quiescence. -------------------------
    //
    // The trigger comes straight from alice's stored bundle: no keys, no SE — exactly what a prior
    // owner or a griefer does. Then enough blocks for BOTH of alice's remaining tiers to mature, so
    // silence is a refusal and not a wait.
    {
        use electrum_client::ElectrumApi;
        let raw = hex::decode(&t_b_hex)?;
        let _ = cc.electrum_client.transaction_broadcast_raw(&raw);
    }
    mine_synced(&cc, &core, 2)?;
    assert!(
        onchain(&cc, &t_b).is_some(),
        "the second carrier's trigger must be on chain, or the watchtower pass has nothing to react to"
    );

    let budget = sender_exit_blocks() + 12; // X_0's CSV + S(alice)'s CSV, with headroom
    let passes = 10u32;
    let per_pass = (budget / passes) + 1;
    for _ in 0..passes {
        match alice.defend_ladders().await {
            Ok(acted) => assert!(
                !acted.contains(&carrier_b),
                "alice's watchtower ACTED on {carrier_b}, a whole carrier she has already conveyed \
                 to bob — its retained state rivals the one that carries his allocation"
            ),
            // Blindness on some OTHER coin (e.g. Part A's change child, whose chain is now partly
            // on chain) must not mask this assertion — but the conveyed carrier must never be the
            // coin reported: the allowlist decides it, it does not fail to decide it.
            Err(e) => assert!(
                !e.to_string().contains(&carrier_b),
                "alice's watchtower reported the conveyed carrier {carrier_b} as BLIND rather than \
                 skipping it: {e}"
            ),
        }
        mine_synced(&cc, &core, per_pass)?;
    }
    assert!(
        onchain(&cc, &s_alice).is_none(),
        "THE D1 THEFT: alice's watchtower broadcast her retained state {s_alice} for a carrier she \
         conveyed WHOLE to bob. It spends the same X_0 output as bob's S', and bob's entire \
         allocation ({SUPPLY_B} of {asset_b}) lives on S' — confirming alice's state destroys it."
    );
    println!(
        "SDK79 - (B4) FILTERED: with the conveyed carrier triggered on chain and {budget}+ blocks \
         mined (X_0 needs {}, S(alice) a further {}), ten watchtower passes never acted on it and \
         alice's retained state {} is still absent from the chain",
        mercurylib::tesr::TesrParams::regtest().ext_csv(0),
        mercurylib::tesr::TesrParams::regtest().state_csv(0),
        &s_alice[..12]
    );

    // ---- B5. …and bob's own tower is the one that advances the same output. -----------------------
    let bob_bg2 = bob.start_background();
    for _ in 0..12 {
        let _ = bob.defend_ladders().await;
        if onchain(&cc, &s_bob).is_some() {
            break;
        }
        mine_synced(&cc, &core, 5)?;
    }
    drop(bob_bg2);
    assert!(
        onchain(&cc, &x_b).is_some(),
        "bob's tower must have replayed the extension X_0 of the carrier he was handed"
    );
    assert!(
        onchain(&cc, &s_bob).is_some(),
        "bob's tower must have landed S' — the state that carries the allocation alice conveyed"
    );
    assert!(
        onchain(&cc, &s_alice).is_none(),
        "alice's retained state must still be absent once S' is on chain: the two are rivals over \
         X_0's payload output and only one can win"
    );
    println!(
        "SDK79 - (B5) bob's tower landed S' {} over X_0 {}; alice's rival {} never reached the \
         chain",
        &s_bob[..12],
        &x_b[..12],
        &s_alice[..12]
    );

    println!(
        "SDK79 - PASS: both routes closed. PART A — an in-ladder split's sender names SP live and \
         her tower will not race the child she funded. PART B — a WHOLE-carrier conveyance leaves \
         the sender holding a complete co-signed chain over a coin that is neither WITHDRAWN nor \
         WITHDRAWING, and the LIVENESS ALLOWLIST (broadcast only for a coin still held CONFIRMED) \
         stops her tower from destroying the recipient's allocation without the filter having to \
         know the lane existed."
    );
    Ok(())
}

// =================================================================================================
// PARTS C AND D — [D1 / A2] THE KEY THE FILTER READS MUST BE **DURABLE**
//
// Parts A and B established the liveness ALLOWLIST: `defend_ladders` broadcasts only for a coin this
// wallet still holds CONFIRMED. Both of them assert it AFTER the conveyance has returned, and that
// is exactly where they stop proving anything — because the filter reads `coin.status` **from the
// wallet DB**, and until this change the only thing that moved a conveyed coin out of CONFIRMED was
// `transfer_sender::create_backup_transactions`, which sets the field IN MEMORY on a copy that is
// not written back until `update_wallet` at the very end of `execute_ex`. So on EVERY whole-coin
// conveyance — including `transfer_colored_carrier` — there was a live window in which the recipient
// already held a co-signed, receiver-paying `S'` and a concurrent watchtower pass read a STALE
// CONFIRMED off disk and was ADMITTED. The filter was keyed on precisely the field that had not yet
// been written. The child re-transfer lane is the same shape one level down: the sender's child coin
// stays CONFIRMED for the whole hop (`transfer_colored_child` marks it WITHDRAWN only afterwards),
// so what the tower broadcasts is decided by the `ctesr-` row's CONTENT — which still named the
// state being superseded, because the row was written AFTER the conveyance rather than before it.
//
// ## HOW THESE TWO PARTS ARE MADE NON-VACUOUS
//
// Racing a live conveyance is only evidence if the concurrent pass is (a) genuinely concurrent,
// (b) genuinely LETHAL — able to broadcast, not merely able to run — and (c) started at a moment
// that is defined WITHOUT reference to the fix. All three are load-bearing:
//
//  (a) The pass runs on its own tokio task with its own `ClientConfig`, hammering the real
//      `defend_ladders`, for the whole duration of the real SDK call.
//  (b) The coin's trigger `T` is put on chain FIRST and `ext_csv(0)` blocks are mined, so `X_0` is
//      mature and an admitted pass broadcasts it immediately. `X_0` landing is therefore a direct,
//      on-chain observation that the tower was admitted — and it is a SAFE discriminator, because
//      `X_0` is shared by the sender's retained state and the recipient's `S'` alike. The state that
//      would actually destroy the recipient (`S`, a further `state_csv(0)` deep) can never mature
//      during the race, so this test cannot itself become the theft it is testing for.
//  (c) The pass is GATED on the SE's `num_sigs` for the coin rising above the pre-call baseline —
//      a coordinator-side, read-only marker of "the superseding state has now been co-signed" that
//      is entirely independent of the fix under test. Before that instant the coin is still wholly
//      the sender's and defending it is CORRECT, so an ungated race would have to fail even on
//      correct code. After it, the durable arm-down (which the fix performs BEFORE the co-sign) must
//      already be on disk.
//
// A second, cheaper witness samples the two facts the filter itself joins — the SE's `num_sigs` and
// the coin's status AS READ FROM THE WALLET DB — every millisecond throughout the call. A sample
// showing (superseding state co-signed) AND (disk still says CONFIRMED) IS an admitted watchtower
// pass, whether or not one happened to run at that microsecond. Its `post_cosign` counter is the
// non-vacuity guard: if the witness never observed the co-signed window at all, the test says so and
// fails rather than reporting green.
//
// COUNTERFACTUAL: with the durable arm-down in `transfer_sender::execute_ex` (Part C) or in
// `tesr::cosign_colored_child_retransfer` (Part D) reverted, the coin reads CONFIRMED off disk for
// the entire remainder of the call — two coordinator round-trips and an SE co-sign, hundreds of
// milliseconds — so both the witness and the racing tower catch it every time.
// =================================================================================================

/// The SE's cumulative co-sign counter for a node: the coordinator-side, read-only, fix-independent
/// marker both races gate on.
async fn se_num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or_else(|| anyhow!("no statechain_info for {sid}"))?
        .num_sigs)
}

/// A concurrent, read-only witness of exactly the join `defend_ladders`' L1 performs.
#[derive(Clone)]
struct Witness {
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    samples: Arc<AtomicU32>,
    /// Samples taken after the SE showed the superseding co-sign.
    post_cosign: Arc<AtomicU32>,
    /// Of those, the ones where the WALLET DB still said CONFIRMED — i.e. a watchtower pass that
    /// would have been admitted while a counterparty could already be handed rival material.
    admitted: Arc<AtomicU32>,
}

fn spawn_witness(
    wallet_name: &str,
    sid: &str,
    baseline_sigs: u32,
) -> (Witness, tokio::task::JoinHandle<()>) {
    let w = Witness {
        stop: Arc::new(AtomicBool::new(false)),
        ready: Arc::new(AtomicBool::new(false)),
        samples: Arc::new(AtomicU32::new(0)),
        post_cosign: Arc::new(AtomicU32::new(0)),
        admitted: Arc::new(AtomicU32::new(0)),
    };
    let me = w.clone();
    let wallet_name = wallet_name.to_string();
    let sid = sid.to_string();
    let handle = tokio::spawn(async move {
        // Its own config: its own pool and its own HTTP client, so it never serialises behind the
        // call it is watching.
        let cc = mercuryrustlib::client_config::load().await;
        me.ready.store(true, Ordering::SeqCst);
        while !me.stop.load(Ordering::SeqCst) {
            // A failed read is recorded as "not co-signed yet", which can only UNDER-count the
            // defect — never invent one. `post_cosign` is what stops that under-counting from
            // turning into a silently green test.
            let ns = match mercuryrustlib::utils::get_statechain_info(&sid, &cc).await {
                Ok(Some(i)) => i.num_sigs,
                _ => 0,
            };
            let status = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &wallet_name)
                .await
                .ok()
                .and_then(|wl| {
                    wl.coins.into_iter().find(|c| {
                        c.statechain_id.as_deref() == Some(sid.as_str()) && c.duplicate_index == 0
                    })
                })
                .map(|c| c.status);
            me.samples.fetch_add(1, Ordering::SeqCst);
            if ns > baseline_sigs {
                me.post_cosign.fetch_add(1, Ordering::SeqCst);
                if status == Some(CoinStatus::CONFIRMED) {
                    me.admitted.fetch_add(1, Ordering::SeqCst);
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    (w, handle)
}

/// The real thing: a watchtower hammering `defend_ladders` on its own task, started only once the SE
/// shows the superseding co-sign.
#[derive(Clone)]
struct Racer {
    stop: Arc<AtomicBool>,
    /// Passes that ran AFTER the gate opened. Zero means the race never happened.
    passes: Arc<AtomicU32>,
    /// The tower BROADCAST a tier for this coin — i.e. L1 admitted it.
    acted: Arc<AtomicBool>,
    /// The tower reported this coin as blindness instead of deciding about it.
    blind: Arc<AtomicBool>,
}

fn spawn_racer(w: UtexoWallet, sid: &str, baseline_sigs: u32) -> (Racer, tokio::task::JoinHandle<()>) {
    let r = Racer {
        stop: Arc::new(AtomicBool::new(false)),
        passes: Arc::new(AtomicU32::new(0)),
        acted: Arc::new(AtomicBool::new(false)),
        blind: Arc::new(AtomicBool::new(false)),
    };
    let me = r.clone();
    let sid = sid.to_string();
    let handle = tokio::spawn(async move {
        let cc = mercuryrustlib::client_config::load().await;
        // THE GATE. Before the SE has co-signed the superseding state this coin is still wholly
        // ours and defending it is the CORRECT behaviour, so a pass here would have to be allowed —
        // and the fix's durable arm-down runs strictly BEFORE that co-sign. Waiting for the counter
        // to move is therefore the earliest instant at which "the tower must stand down" is true,
        // expressed without reference to anything the fix writes.
        loop {
            if me.stop.load(Ordering::SeqCst) {
                return;
            }
            if matches!(se_num_sigs(&cc, &sid).await, Ok(n) if n > baseline_sigs) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        while !me.stop.load(Ordering::SeqCst) {
            me.passes.fetch_add(1, Ordering::SeqCst);
            match w.defend_ladders().await {
                Ok(acted) => {
                    if acted.iter().any(|a| a == &sid) {
                        me.acted.store(true, Ordering::SeqCst);
                    }
                }
                // Blindness on some OTHER coin is none of this race's business; blindness on THIS
                // one is a failure to decide, which the allowlist is supposed to make impossible.
                Err(e) => {
                    if e.to_string().contains(&sid) {
                        me.blind.store(true, Ordering::SeqCst);
                    }
                }
            }
            // Throttled to keep DB contention down: unthrottled this loop runs ~20 000 passes in
            // the time the conveyance takes, and ~300 is already far more of a race than the
            // property needs. Measured, so the number is not folklore: throttling did NOT speed the
            // witness up (132 -> 138 samples over the same call), because the witness's rate is
            // dominated by its own per-sample cost — one coordinator read plus a full wallet-blob
            // read, ~70 ms. That is why the reverted build resolves the defect window with a
            // handful of admitted samples rather than hundreds; a handful is still a decided
            // non-zero, which is what the assertion below tests.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    (r, handle)
}

/// Wait for a wallet's coloured root carrier of `asset` to appear, claiming and mining as needed.
async fn find_colored_carrier(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    asset: &str,
    core: &str,
) -> Result<String> {
    for _ in 0..90 {
        w.claim().await?;
        let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins;
        for c in coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        {
            let Some(sid) = c.statechain_id.clone() else { continue };
            if mercuryrustlib::tesr::load(cc, wallet_name, &sid)
                .await?
                .is_some_and(|b| {
                    b.is_colored() && b.rgb.as_ref().is_some_and(|r| r.contract_id == asset)
                })
            {
                return Ok(sid);
            }
        }
        mine_synced(cc, core, 1)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{wallet_name} never got a COLOURED ladder for {asset}"))
}

/// Durably set a coin's status in the wallet DB — the ONE field `defend_ladders`' liveness allowlist
/// reads, and the field A2 is about. Used only to run the in-test counterfactual: flip it back and
/// the very same pass, on the very same row and backend, fires.
async fn force_status(
    cc: &ClientConfig,
    wallet_name: &str,
    sid: &str,
    status: CoinStatus,
) -> Result<CoinStatus> {
    let mut w = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let c = w
        .coins
        .iter_mut()
        .find(|c| c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("no coin {sid} in {wallet_name}"))?;
    let prev = c.status.clone();
    c.status = status;
    mercuryrustlib::sqlite_manager::update_wallet(&cc.pool, &w).await?;
    Ok(prev)
}

// ============================ PART C — WHOLE-COIN CONVEYANCE, RACED =============================

async fn part_c(
    cc: &ClientConfig,
    alice: &UtexoWallet,
    bob: &UtexoWallet,
    bob_address: &str,
    core: &str,
) -> Result<()> {
    for _ in 0..3 {
        let t = prepaid_token(cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    for _ in 0..3 {
        let t = prepaid_token(cc).await?;
        bob.add_prepaid_token(&t).await;
    }
    let asset_c = alice.issue_token("WTW3", "Watchtower Token III", 0, SUPPLY_C).await?;
    mine_synced(cc, core, 3)?;
    let carrier_c = find_colored_carrier(cc, alice, "sdk79_alice", &asset_c, core).await?;

    let pre = mercuryrustlib::tesr::load(cc, "sdk79_alice", &carrier_c)
        .await?
        .ok_or_else(|| anyhow!("the third coloured ladder vanished"))?;
    let t_c = pre.trigger.txid.clone();
    let x_c = pre.current().extension.txid.clone();
    let s_alice_c = pre.current().state.txid.clone();
    let t_c_hex = pre.trigger.signed_tx.clone();

    // ---- C1. TRIGGERED FIRST — the griefer scenario, and what makes the racing tower LETHAL. ----
    //
    // Parts A and B convey and then trigger, so their towers are only ever asked to refuse a coin
    // whose exit has already begun. Here the exit is ALREADY under way when the owner conveys, which
    // is the case D1 is actually about: an admitted pass has a mature `X_0` in hand and broadcasts it
    // on the spot. `S(alice)` needs a further `state_csv(0)` blocks after `X_0` CONFIRMS and no
    // blocks are mined during the race, so the tier that would destroy the recipient's allocation
    // cannot mature inside this test — the discriminator is lethal, the test is not.
    {
        use electrum_client::ElectrumApi;
        let raw = hex::decode(&t_c_hex)?;
        let _ = cc.electrum_client.transaction_broadcast_raw(&raw);
    }
    mine_synced(cc, core, 2)?;
    assert!(onchain(cc, &t_c).is_some(), "the third carrier's trigger must be on chain");
    mine_synced(cc, core, mercurylib::tesr::TesrParams::regtest().ext_csv(0) as u32 + 2)?;
    assert!(
        onchain(cc, &x_c).is_none(),
        "X_0 must not be on chain BEFORE the race — nothing has been asked to defend this coin yet, \
         and if it were already landed the race would have no discriminator left"
    );
    println!(
        "SDK79 - (C1) carrier {carrier_c} ({SUPPLY_C} of {asset_c}) is TRIGGERED: T {} is on chain \
         and X_0 {} is mature. Any admitted watchtower pass now broadcasts X_0 immediately.",
        &t_c[..12],
        &x_c[..12]
    );

    // ---- C2. THE RACE. ---------------------------------------------------------------------------
    let baseline = se_num_sigs(cc, &carrier_c).await?;
    let (witness, wh) = spawn_witness("sdk79_alice", &carrier_c, baseline);
    for _ in 0..200 {
        if witness.ready.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(witness.ready.load(Ordering::SeqCst), "the witness task never came up");
    let (racer, rh) = spawn_racer(alice.clone(), &carrier_c, baseline);

    let conveyed = alice.transfer_colored_carrier(&carrier_c, bob_address).await;

    // Let both keep running a moment past the end of the call: the window this test exists for used
    // to close only at `update_wallet`, the very last statement.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    witness.stop.store(true, Ordering::SeqCst);
    racer.stop.store(true, Ordering::SeqCst);
    let _ = wh.await;
    let _ = rh.await;
    conveyed?;

    let post_cosign = witness.post_cosign.load(Ordering::SeqCst);
    let admitted = witness.admitted.load(Ordering::SeqCst);
    let passes = racer.passes.load(Ordering::SeqCst);
    println!(
        "SDK79 - (C2) race over: {} witness samples ({post_cosign} of them after the SE showed the \
         S' co-sign), {passes} watchtower passes after the gate opened",
        witness.samples.load(Ordering::SeqCst)
    );

    // ---- C3. NON-VACUITY FIRST. -------------------------------------------------------------------
    assert!(
        post_cosign > 0,
        "the witness never observed the window it exists to observe: the SE's num_sigs for \
         {carrier_c} never rose above {baseline} while the conveyance was in flight. This test \
         proves NOTHING in that state — do not read it as green. Re-derive the marker."
    );
    assert!(
        passes > 0,
        "the racing watchtower never ran a single pass after the gate opened, so nothing raced the \
         conveyance. This test proves NOTHING in that state."
    );

    // ---- C4. THE PROPERTY. ------------------------------------------------------------------------
    assert_eq!(
        admitted, 0,
        "THE A2 DEFECT: in {admitted} of {post_cosign} samples the SE had already co-signed the \
         receiver-paying S' for {carrier_c} while this wallet's DB still read CONFIRMED for it. \
         That is precisely the join `defend_ladders`' liveness allowlist performs, so each of those \
         samples is a watchtower pass that would have been ADMITTED to broadcast the sender's \
         retained state over the same X_0 output the recipient's S' spends — destroying the whole \
         allocation. The status must be made DURABLE before the conveyance, not left in memory \
         until `update_wallet`."
    );
    assert!(
        !racer.acted.load(Ordering::SeqCst),
        "THE A2 THEFT, observed live: a watchtower pass running concurrently with the conveyance of \
         {carrier_c} was admitted and BROADCAST a tier of the ladder alice was giving away."
    );
    assert!(
        !racer.blind.load(Ordering::SeqCst),
        "the racing watchtower reported {carrier_c} as BLIND rather than deciding about it — the \
         allowlist must DECIDE (skip), not fail to see"
    );
    assert!(
        onchain(cc, &x_c).is_none(),
        "THE A2 THEFT, on chain: X_0 {x_c} was broadcast during the conveyance of {carrier_c}. Only \
         alice's tower could have put it there, and it could only have done so by reading a stale \
         CONFIRMED off disk."
    );
    let status = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk79_alice")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(carrier_c.as_str()) && c.duplicate_index == 0)
        .map(|c| c.status)
        .ok_or_else(|| anyhow!("alice's conveyed carrier coin vanished"))?;
    assert_ne!(
        status,
        CoinStatus::CONFIRMED,
        "after the conveyance the coin must not read CONFIRMED on disk — that is the evidence the \
         allowlist keys on"
    );
    println!(
        "SDK79 - (C3/C4) {post_cosign} post-co-sign samples, ZERO admitted; {passes} concurrent \
         watchtower passes, none acted; X_0 {} still absent from the chain; alice's coin reads \
         {status} on disk.",
        &x_c[..12]
    );

    // ---- C5. …AND THE PASS REALLY WAS LETHAL — the counterfactual, run inside the test. --------
    //
    // Without this, "alice's tower did not broadcast X_0" is indistinguishable from "X_0 was not
    // broadcastable" and every assertion above is decoration. So flip back the ONE field the
    // allowlist reads — the coin's durable status — and run the SAME pass, on the SAME row, against
    // the SAME backend, at a moment when nothing else has changed. It fires immediately. That is
    // the whole of A2 in two lines: the tower's silence during the race was a REFUSAL, and the
    // refusal was caused by exactly the durable write this change adds.
    //
    // `S(alice)` is a further `state_csv(0)` deep and no blocks are mined here, so the tier that
    // would actually destroy a recipient still cannot mature — the counterfactual is safe to run.
    let restore_to = force_status(cc, "sdk79_alice", &carrier_c, CoinStatus::CONFIRMED).await?;
    let mut fired = false;
    for _ in 0..6 {
        if let Ok(acted) = alice.defend_ladders().await {
            if acted.iter().any(|a| a == &carrier_c) {
                fired = true;
                break;
            }
        }
        if onchain(cc, &x_c).is_some() {
            fired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Put it back before asserting, so a failure here cannot leave a conveyed coin re-armed.
    force_status(cc, "sdk79_alice", &carrier_c, restore_to).await?;
    assert!(
        fired,
        "the counterfactual did not fire: with {carrier_c} forced back to CONFIRMED the watchtower \
         STILL did not act on it. Then its silence during the race proves nothing about the \
         liveness allowlist — the tier may simply not have been broadcastable — and PART C must be \
         re-derived rather than read as green."
    );
    assert!(
        onchain(cc, &x_c).is_some(),
        "the counterfactual pass reported acting on {carrier_c} but X_0 {x_c} is not on the chain"
    );
    assert!(
        onchain(cc, &s_alice_c).is_none(),
        "S(alice) must still be absent — it is a further state_csv(0) deep and no blocks were mined"
    );
    // NOTE on why PART C stops here rather than having bob adopt: `transfer_receiver` refuses a
    // whole-coin handover whose `tx0` output is already spent ("tx0 output is spent or not
    // confirmed"), and this carrier was deliberately TRIGGERED before the conveyance so the racing
    // tower would be lethal. The recipient half of this lane is PART B's job — it conveys, adopts,
    // and lands `S'` — and PART C is the race. (That the SDK will open a conveyance on a coin whose
    // unilateral exit has already begun, producing a transfer no receiver can complete, is a
    // separate, pre-existing gap and is not what this test is about.)
    println!(
        "SDK79 - PART C PASS: a whole-coin coloured conveyance was raced by a real, LETHAL \
         watchtower gated on the SE's own co-sign counter. {passes} passes, none acted; \
         {post_cosign} post-co-sign samples, none admitted; X_0 {} absent throughout. Forcing the \
         coin's durable status back to CONFIRMED made the very same pass broadcast X_0 at once — \
         so the silence was a refusal, and the durable status was the only thing causing it.",
        &x_c[..12]
    );
    Ok(())
}

// ========================= PART D — CHILD RE-TRANSFER LANE, RACED ==============================

async fn part_d(
    cc: &ClientConfig,
    alice: &UtexoWallet,
    bob: &UtexoWallet,
    bob_address: &str,
    core: &str,
) -> Result<()> {
    let mut carol_cfg = SdkConfig::regtest("sdk79_carol");
    carol_cfg.rgb_data_dir = Some("./rgb-data-sdk79_carol".to_string());
    carol_cfg.auto_exit = false;
    // Same reason as alice and bob at the top of `execute`: the lane is opt-in (it ships OFF,
    // 2c351c6) and carol is the recipient of a coloured CHILD re-transfer, which only exists on it.
    carol_cfg.colored_ladder = true;
    let (carol, _) = UtexoWallet::initialize(carol_cfg, None).await?;
    let carol_address = carol.get_utexo_address().await?;
    for _ in 0..4 {
        let t = prepaid_token(cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    for _ in 0..4 {
        let t = prepaid_token(cc).await?;
        bob.add_prepaid_token(&t).await;
    }
    for _ in 0..4 {
        let t = prepaid_token(cc).await?;
        carol.add_prepaid_token(&t).await;
    }

    let asset_d = alice.issue_token("WTW4", "Watchtower Token IV", 0, SUPPLY_D).await?;
    mine_synced(cc, core, 3)?;
    let carrier_d = find_colored_carrier(cc, alice, "sdk79_alice", &asset_d, core).await?;
    println!("SDK79 - (D0) alice's fourth coloured carrier {carrier_d} holds {SUPPLY_D} of {asset_d}");

    // ---- D1. Alice pays bob in-ladder, so bob holds a coloured CHILD to re-transfer. -------------
    let out = alice
        .transfer_tokens(&asset_d, bob_address, PAY_D)
        .await
        .map_err(|e| anyhow!("(D1) alice's in-ladder pay to bob failed: {e:#}"))?;
    let piece_sid = out.coins[0].statechain_id.clone();
    let mut bob_child = None;
    // The last reason a POLL pass did not settle, so the timeout below names it instead of just
    // saying "never adopted". A pass that errors is "not ready yet" — this whole loop is the wait,
    // and `book_incoming_token` retries a transient accept on the NEXT `claim()` by construction —
    // but it is never SWALLOWED: if the state never arrives the loop fails and prints this.
    let mut last_wait: Option<String> = None;
    for _ in 0..60 {
        if let Err(e) = bob.claim().await {
            last_wait = Some(format!("bob.claim: {e:#}"));
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if let Err(e) = alice.claim().await {
            last_wait = Some(format!("alice.claim: {e:#}"));
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if let Some(cb) = mercuryrustlib::tesr::load_child(cc, "sdk79_bob", &piece_sid).await? {
            // SYNCHRONISATION, NOT RELAXATION — and the gate was genuinely under-specified.
            //
            // Adopting the `ctesr-` row and ACCEPTING the consignment into bob's RGB STOCK are two
            // different writes on two different retry clocks. `book_incoming_token` classifies a
            // resolver miss (the un-broadcast child extension is not in the indexer, and the proxy
            // is still catching up) as `Pending` and retries it on the NEXT `claim()`, so the bundle
            // is on disk one or more passes before the allocation is booked. Breaking on the row
            // alone let D2 start against a stock that did not yet know the contract, and
            // `transfer_colored_child` then failed on its own PRECONDITION — observed as both
            // "contract rgb:… is unknown" and "the coloured S'_child consignment does not validate
            // against its chain (Unresolved)" — neither of which is anything Part D is about.
            //
            // So the gate is the precondition the hop actually has: the child is adopted AND its
            // allocation is booked. That is strictly STRONGER than what was there, and it changes
            // nothing about what D2/D3 then prove — the race, the witness and the `admitted == 0`
            // assertion all run exactly as before, on a hop that can now reach them.
            let booked = match bob.get_token_balances().await {
                Ok(bals) => bals.into_iter().any(|b| b.asset_id == asset_d && b.balance >= PAY_D),
                Err(e) => {
                    last_wait = Some(format!("bob.get_token_balances: {e:#}"));
                    false
                }
            };
            if cb.is_colored() && booked {
                bob_child = Some(cb);
                break;
            }
            if !booked && last_wait.is_none() {
                last_wait = Some(format!(
                    "bob adopted the `ctesr-` row for {piece_sid} but has not BOOKED {PAY_D} of \
                     {asset_d} yet"
                ));
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let bob_child = bob_child.ok_or_else(|| {
        anyhow!(
            "bob never reached the state PART D needs for {piece_sid}: the coloured child adopted \
             AND {PAY_D} of {asset_d} booked into his RGB stock. Last pass reported: {}",
            last_wait.as_deref().unwrap_or("<no error recorded>")
        )
    })?;
    let s_child_bob = bob_child.child_state.txid.clone();
    let parent_t = bob_child.parent.trigger.txid.clone();
    let parent_t_hex = bob_child.parent.trigger.signed_tx.clone();
    let parent_x = bob_child.parent.current().extension.txid.clone();
    println!(
        "SDK79 - (D1) bob adopted coloured child {piece_sid} ({PAY_D} of {asset_d}); its live state \
         is {} and its parent segment is T {} -> X_m {} -> SP {}",
        &s_child_bob[..12],
        &parent_t[..12],
        &parent_x[..12],
        &bob_child.parent.current().state.txid[..12]
    );

    // ---- D2. THE PREMISE OF THIS LANE. -----------------------------------------------------------
    //
    // The child coin is CONFIRMED going in and STAYS CONFIRMED for the whole hop —
    // `transfer_colored_child` marks it WITHDRAWN only after the re-transfer returns. So the
    // liveness allowlist ADMITS this child throughout, by design (a child re-transfer has no
    // on-chain step and the coin genuinely is ours until the handover completes), and the only
    // things standing between bob's tower and carol's allocation are the two durable writes this
    // change reorders: the arm-down before the co-sign, and the superseding bundle before the
    // conveyance.
    let child_status = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk79_bob")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(piece_sid.as_str()) && c.duplicate_index == 0)
        .map(|c| c.status)
        .ok_or_else(|| anyhow!("bob's child coin vanished"))?;
    assert_eq!(
        child_status,
        CoinStatus::CONFIRMED,
        "bob's child coin must be CONFIRMED entering the hop — if a later change moves it earlier, \
         PART D stops testing the window it exists for and must be re-derived, not deleted"
    );

    // ---- D3. THE RACE. ---------------------------------------------------------------------------
    let baseline = se_num_sigs(cc, &piece_sid).await?;
    let (witness, wh) = spawn_witness("sdk79_bob", &piece_sid, baseline);
    for _ in 0..200 {
        if witness.ready.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(witness.ready.load(Ordering::SeqCst), "the witness task never came up");
    let (racer, rh) = spawn_racer(bob.clone(), &piece_sid, baseline);

    let conveyed = bob.transfer_colored_child(&piece_sid, &carol_address).await;

    tokio::time::sleep(Duration::from_millis(2000)).await;
    witness.stop.store(true, Ordering::SeqCst);
    racer.stop.store(true, Ordering::SeqCst);
    let _ = wh.await;
    let _ = rh.await;
    conveyed?;

    let post_cosign = witness.post_cosign.load(Ordering::SeqCst);
    let admitted = witness.admitted.load(Ordering::SeqCst);
    let passes = racer.passes.load(Ordering::SeqCst);
    assert!(
        post_cosign > 0,
        "the witness never observed the co-signed window for child {piece_sid} (num_sigs never rose \
         above {baseline} in flight). PART D proves NOTHING in that state."
    );
    assert!(
        passes > 0,
        "no watchtower pass ran after the gate opened, so nothing raced the child re-transfer. \
         PART D proves NOTHING in that state."
    );
    assert_eq!(
        admitted, 0,
        "THE D1 DEFECT ON THE CHILD LANE: in {admitted} of {post_cosign} samples the SE had already \
         co-signed S'_child for {piece_sid} while bob's DB still read CONFIRMED for the child coin. \
         `transfer_colored_child` does not mark it WITHDRAWN until it returns, so every one of those \
         samples is a watchtower pass the allowlist would have ADMITTED — driving a `ctesr-` row \
         over a state that is being superseded, which rivals carol's S'_child over ext_child's \
         payload output and burns her allocation."
    );
    // The racing tower is the SECONDARY witness on this lane and is honest about it: the child's
    // funding is un-broadcast at this point, so `watch_child_pass` is `Idle` by construction and a
    // pass could not have broadcast even if L1 had admitted it. The WITNESS above is what carries
    // the property here; D6 then establishes, on the same row and backend, that a pass on this
    // child IS lethal and that the durable status is the only thing that refuses it.
    assert!(
        !racer.acted.load(Ordering::SeqCst),
        "THE D1 THEFT, observed live: a watchtower pass running concurrently with the re-transfer of \
         child {piece_sid} was admitted and BROADCAST a tier of the chain bob was handing to carol."
    );
    assert!(
        !racer.blind.load(Ordering::SeqCst),
        "the racing watchtower reported child {piece_sid} as BLIND rather than deciding about it"
    );

    // ---- D4. STORE-BEFORE-CONVEY: the row bob keeps is the recipient's ALLY, not her rival. ------
    let post = mercuryrustlib::tesr::load_child(cc, "sdk79_bob", &piece_sid)
        .await?
        .ok_or_else(|| anyhow!("bob's ctesr row for {piece_sid} disappeared"))?;
    let s_child_carol = post.child_state.txid.clone();
    assert_ne!(
        s_child_carol, s_child_bob,
        "the re-transfer must have installed a NEW state; if it did not there is no supersession \
         and nothing to order"
    );
    assert!(
        post.child_superseded_states.iter().any(|t| t.txid == s_child_bob),
        "the state bob held ({s_child_bob}) must be DISCLOSED as superseded, not silently dropped — \
         carol's census counts it"
    );
    let chain: Vec<String> = mercuryrustlib::tesr::child_exit_chain(&post)
        .iter()
        .map(|(hex, _)| {
            use electrum_client::bitcoin::consensus::deserialize;
            let raw = hex::decode(hex).unwrap_or_default();
            deserialize::<electrum_client::bitcoin::Transaction>(&raw)
                .map(|t| t.txid().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert!(
        !chain.contains(&s_child_bob),
        "the superseded state must be OUT of the chain bob's tower drives: {chain:?}"
    );
    assert!(
        chain.contains(&s_child_carol),
        "bob's stored chain must END in the state he conveyed to carol — storing BEFORE conveying is \
         what turns his tower from carol's rival into her ally: {chain:?}"
    );
    let after_status = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk79_bob")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(piece_sid.as_str()) && c.duplicate_index == 0)
        .map(|c| c.status)
        .ok_or_else(|| anyhow!("bob's child coin vanished after the hop"))?;
    assert_ne!(after_status, CoinStatus::CONFIRMED, "the re-transferred child must not read CONFIRMED");
    println!(
        "SDK79 - (D3/D4) {post_cosign} post-co-sign samples, ZERO admitted; {passes} concurrent \
         watchtower passes, none acted; X_m {} absent; bob's row now names carol's S'_child {} live \
         with his own {} superseded and out of the exit chain; his coin reads {after_status}.",
        &parent_x[..12],
        &s_child_carol[..12],
        &s_child_bob[..12]
    );

    // ---- D5. Carol really got it, and the chain really was actionable. ---------------------------
    let mut carol_child = None;
    // Same under-specified gate as D1, one level down, and the same fix: the `ctesr-` row and the
    // ACCEPT into carol's RGB stock are separate writes, and `book_incoming_token` retries a
    // resolver miss on the next `claim()`. Breaking on the row alone ran the balance assertion
    // BEFORE the accept had had its retries, so the assertion below reported "an allocation was
    // destroyed by a hop" when nothing had been destroyed and the accept was simply still pending.
    // The ASSERTION IS UNCHANGED and still loud — this only makes the wait wait for the thing the
    // assertion is about, and carol never booking it in 60 passes is still a failure.
    let mut carol_wait: Option<String> = None;
    for _ in 0..60 {
        if let Err(e) = carol.claim().await {
            carol_wait = Some(format!("carol.claim: {e:#}"));
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if let Some(cb) = mercuryrustlib::tesr::load_child(cc, "sdk79_carol", &piece_sid).await? {
            let booked = match carol.get_token_balances().await {
                Ok(bals) => bals.into_iter().any(|b| b.asset_id == asset_d && b.balance == PAY_D),
                Err(e) => {
                    carol_wait = Some(format!("carol.get_token_balances: {e:#}"));
                    false
                }
            };
            if cb.is_colored() && booked {
                carol_child = Some(cb);
                break;
            }
            if !booked {
                carol_wait.get_or_insert_with(|| {
                    format!("carol adopted the row for {piece_sid} but has not booked {PAY_D} yet")
                });
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let carol_child = carol_child.ok_or_else(|| {
        anyhow!(
            "carol never adopted the re-transferred child AND booked {PAY_D} of {asset_d}. Last \
             pass reported: {}",
            carol_wait.as_deref().unwrap_or("<no error recorded>")
        )
    })?;
    assert_eq!(
        carol_child.child_state.txid, s_child_carol,
        "carol must hold exactly the state bob stored before conveying"
    );
    assert!(
        carol
            .get_token_balances()
            .await?
            .iter()
            .any(|b| b.asset_id == asset_d && b.balance == PAY_D),
        "carol must have booked the whole {PAY_D} of {asset_d} — an allocation must never be \
         destroyed by a hop"
    );
    println!("SDK79 - (D5) carol adopted child {piece_sid} and holds the full {PAY_D} of {asset_d}");

    // ---- D6. LETHALITY, AND THE ALLY PROPERTY, AS AN IN-TEST COUNTERFACTUAL. ---------------------
    //
    // Only now — after carol has adopted, because `transfer_receiver` refuses a handover whose
    // funding output is already spent — is the parent segment triggered. With `T` on chain and
    // `X_m` mature, a pass on bob's `ctesr-` row is genuinely LETHAL. Then flip back the ONE field
    // the allowlist reads and run the SAME pass on the SAME row: it fires at once. That closes the
    // gap the race alone cannot close — bob's silence during the hop was a REFUSAL, and the durable
    // arm-down is what caused it.
    //
    // What it broadcasts is the second half of the point. Because the superseding bundle was stored
    // BEFORE anything was conveyed, the row bob's tower drives is now CAROL's chain: the pass
    // advances the recipient's exit instead of racing it. An ally, not a rival.
    {
        use electrum_client::ElectrumApi;
        let raw = hex::decode(&parent_t_hex)?;
        let _ = cc.electrum_client.transaction_broadcast_raw(&raw);
    }
    mine_synced(cc, core, 2)?;
    assert!(onchain(cc, &parent_t).is_some(), "the child's parent trigger must be on chain");
    mine_synced(cc, core, mercurylib::tesr::TesrParams::regtest().ext_csv(0) as u32 + 2)?;
    assert!(
        onchain(cc, &parent_x).is_none(),
        "X_m must not be on chain before the counterfactual, or it has no discriminator left"
    );

    let restore_to = force_status(cc, "sdk79_bob", &piece_sid, CoinStatus::CONFIRMED).await?;
    let mut fired = false;
    for _ in 0..6 {
        if let Ok(acted) = bob.defend_ladders().await {
            if acted.iter().any(|a| a == &piece_sid) {
                fired = true;
                break;
            }
        }
        if onchain(cc, &parent_x).is_some() {
            fired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    force_status(cc, "sdk79_bob", &piece_sid, restore_to).await?;
    assert!(
        fired,
        "the counterfactual did not fire: with child {piece_sid} forced back to CONFIRMED bob's \
         watchtower STILL did not act on it. Then its silence during the hop proves nothing about \
         the liveness allowlist, and PART D must be re-derived rather than read as green."
    );
    assert!(
        onchain(cc, &parent_x).is_some(),
        "the counterfactual pass reported acting on {piece_sid} but X_m {parent_x} is not on chain"
    );
    assert!(
        onchain(cc, &s_child_bob).is_none(),
        "the state bob superseded must never reach the chain: it rivals carol's S'_child over \
         ext_child's payload output, and on the coloured lane her whole allocation lives there"
    );
    assert_eq!(
        carol
            .get_token_balances()
            .await?
            .iter()
            .find(|b| b.asset_id == asset_d)
            .map(|b| b.balance),
        Some(PAY_D),
        "carol's allocation must be intact after bob's tower ran over the same segment"
    );
    println!(
        "SDK79 - PART D PASS: a coloured CHILD re-transfer was raced by a watchtower gated on the \
         SE's own co-sign counter — {post_cosign} post-co-sign samples, ZERO admitted, {passes} \
         passes, none acted. The superseding bundle was on disk BEFORE anything was conveyed, so \
         bob's row now names carol's S'_child {} live with his own {} superseded and out of the \
         exit chain. Carol holds the full {PAY_D} of {asset_d}. Forcing bob's child coin back to \
         CONFIRMED made the same pass fire at once — and what it broadcast was X_m, a tier of \
         CAROL's chain: the ordering turned his tower from her rival into her ally.",
        &s_child_carol[..12],
        &s_child_bob[..12]
    );
    Ok(())
}
