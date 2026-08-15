//! E2E (SDK_E2E=78) — **[B1] the CTES-R flip must not strand a carrier it cannot colour.**
//!
//! On a wallet running the CTES-R lane (`colored_ladder` ON — set explicitly by all three wallets
//! here; it ships OFF, see the note at the config block), a carrier that has no COLOURED ladder is refused
//! by every route: the legacy RGB-aware split/combine lane is retired, `unilateral_exit` refuses it
//! ("its ladder is not COLOURED"), and `withdraw` refuses it as a carrier. For a carrier that is
//! merely WAITING for its ladder that is right — a later `claim()` colours it and it moves. For a
//! carrier that can never be coloured it is a life sentence, and the largest class of those is every
//! **pre-flip 1_500-sat token piece**: at the 2 sat/vB committed rate it sat above the coloured
//! CHILD floor (1_478, so a split carved it) and below the coloured ROOT floor (2_052, so its
//! receiver could never ladder it). [D44] raised the committed rate to 3 sat/vB and both floors with
//! it (child 1_818, root 2_562), so 1_500 is now below BOTH — it cannot even be carved. Raising
//! `TOKEN_PIECE_SATS` protects new pieces; it does nothing for the ones in circulation.
//!
//! This test starts from coins of exactly that shape — [`N_CARRIERS`] carriers funded with
//! **1_500 sats**, the legacy piece size — on a wallet with the flip ON, and proves they are neither
//! stranded nor rescued by weakening anything.
//!
//! (a) THE TRAP IS REAL, MEASURED. Each carrier is below the coloured ROOT floor — read from
//!     `mercuryrustlib::tesr` rather than copied — and after repeated `claim()` passes NONE
//!     of them has a ladder row of any kind — `build_colored_ladder`'s pre-flight refuses them, and
//!     it refuses on a comparison between two numbers neither side can ever move.
//!
//! (b) SPENDABLE. `transfer_tokens` of 250 (more than any single carrier holds) succeeds through the
//!     MIGRATION HATCH: `migration_hatch_verdict` opens the legacy RGB-aware lane for carriers that
//!     are permanently below the floor and hold no ladder, because the retirement gate's premise —
//!     a rival trigger `T` spending the same `F` — cannot exist for them. bob books 250.
//!
//! (c) THE RECEIVER CAN SPEND WHAT IT WAS PAID. **[D3, re-derived.]** This assertion used to be
//!     satisfied by the SENDER's side of the piece — a refusal message plus a `materialise_carrier`
//!     call — and it accepted EITHER outcome for the receiver, so it could not fail and proved
//!     nothing about the coin bob actually holds. It was written that way because rgb-lib really did
//!     refuse to colour bob's piece (`Invalid coloring info` over its own funding output), four
//!     independent times.
//!
//!     `RGB_E2E=16` found why, with a control: the legacy receive path booked a piece with
//!     `import_asset_offchain` (rgb-lib `save_new_asset` — imports the CONTRACT, never
//!     `accept_transfer`s the transfer) plus `register_statechain` (a SQLITE row), and **never
//!     accepted the transfer into the RGB stock**. No witness ord for the split/combine, no revealed
//!     seal at the piece's outpoint, so `color_psbt` computed `available (0)` while
//!     `get_asset_balance` reported the full amount out of sqlite. Not the E7 class — nothing was
//!     archived and no bundle was invalidated; nothing was ever accepted.
//!
//!     `accept_incoming_tokens` now makes the third call the coloured lane always made —
//!     `accept_ladder` over the piece's exit branch, revealing the one seal a legacy piece has. So
//!     the outcome is no longer optional and this section asserts the thing that was always the
//!     point: bob's piece gets a COLOURED ladder, and bob **SPENDS it** — a real conveyance to a
//!     third wallet, carol, who books the full amount off-chain while bob drops to zero. An
//!     assertion that cannot fail is not evidence; a spend that lands at a third party is.
//!
//!     The size point survives intact, and is still why the migration hatch is evidence-keyed: the
//!     piece carries `TOKEN_PIECE_SATS`, which CLEARS the root floor, so by SIZE it was never in the
//!     migration class — yet for four reproductions it could not be coloured. Size was the wrong
//!     question for the whole class.
//!
//! (d) HONESTLY REFUSED, WITHOUT DESTROYING THE ALLOCATION. alice's 1_626-sat change carrier is
//!     itself in the class. A named `unilateral_exit` on it used to return
//!     `ExitStatus{complete:true}`; [D2] that was a false green, on two counts. `ExitStatus::complete`
//!     is documented as "branch (if any) and backup both broadcast", and the backup is deliberately
//!     never broadcast for a carrier (it is an RGB-unaware spend of `F`); and the "materialisation"
//!     it reported was a rebroadcast of the LEGACY combine transaction, which that lane already
//!     confirmed at payment time — a no-op whose `complete` flag was computed from "is the funding
//!     txid known to the chain", true of every confirmed coin. There is no SE-free spend of this
//!     coin that preserves the allocation, because colouring is what makes a tier an RGB witness and
//!     this is the class that cannot be coloured. So the call now REFUSES, by name, and names the
//!     routes that do exist. Asserted: the refusal names the migration hatch, the pre-signed plain
//!     sweep is still NOT on chain, and a read-only `color_psbt` stock probe still spends exactly 50
//!     units out of the carrier's outpoint (and refuses 51, so the probe is discriminating).
//!     `get_asset_balance` is never used as evidence (E7). The coin is not stranded: (b) above is
//!     the route the refusal names, exercised on these same carriers.
//!
//! NARROWNESS, and where each half of it is proved:
//!   * an above-floor carrier must stay refused — unit `migration_hatch_is_narrow` drives every
//!     branch of the verdict, and sdk75's negative control shows a 17_324-sat FLAT carrier still
//!     refused a unilateral exit with the unchanged message;
//!   * here, at E2E level: bob's `TOKEN_PIECE_SATS` piece is NOT in the class (it gets a coloured
//!     ladder and is spent onward, assertion (c)), and the exit-everything default still excludes
//!     every carrier including the one this test exits by name.
//!
//! # [D44] WHY FOUR CARRIERS AND NOT THREE — the escape bar, and what raising the rate cost
//!
//! The hatch pays out over `transfer_tokens_combine`, which carves the receiver a piece of the
//! CURRENT [`PIECE`] size and leaves the sender the change. So escaping the legacy class costs
//! `TOKEN_PIECE_SATS + fee_reserve + min_split_output` of AGGREGATED carrier value, and [D44] moved
//! that bar from `3_066 + 300 + 666` = **4_032** to `4_074 + 300 + 666` = **5_040**. Three 1_500-sat
//! carriers (4_500) cleared the old bar and do not clear the new one; four (6_000) do.
//!
//! That is a real cost of [D44] and it is stated rather than engineered away: a holder whose ENTIRE
//! legacy holding sits between 4_032 and 5_040 sat could escape before the rate rise and cannot now.
//! The alternative — sizing the hatch's payout down to the bare root floor so three carriers still
//! fit — was rejected, because it hands the RECEIVER a piece with no rate head-room: if the
//! committed rate moves between the send and bob's `claim()`, bob's piece is un-colourable and bob,
//! who did nothing, inherits the stranding. The head-room exists precisely to survive that window,
//! so the hatch pays a HEALTHY piece or it does not pay. What it costs to reach that bar is the
//! number above.
//!
//! Run: SDK_E2E=78 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

/// The legacy token-piece size. NOT a parameter of this test — it is the number every pre-flip piece
/// in circulation carries, which is why the carriers below are funded with exactly it.
const LEGACY_PIECE: u64 = 1_500;
/// The piece a payment carves TODAY, from the SDK rather than copied.
const PIECE: u64 = mercury_utexo_sdk::tokens::TOKEN_PIECE_SATS;
/// How many legacy carriers alice starts with. Four, not three: see the escape-bar section of the
/// module comment — `PIECE + reserve + min_split_output` is 5_040 at the [D44] rate, and three
/// 1_500-sat carriers hold 4_500. This is the smallest count that can still pay.
const N_CARRIERS: usize = 4;

const SUPPLY: u64 = 100;
const MINT: u64 = 100;
const PAY: u64 = 250;

fn floors() -> (u64, u64) {
    let rate = mercurylib::tesr::TesrParams::regtest().committed_fee_rate;
    (
        mercuryrustlib::tesr::colored_child_floor(rate, mercuryrustlib::tesr::COLORED_LADDER_DUST),
        mercuryrustlib::tesr::colored_ladder_floor(rate, mercuryrustlib::tesr::COLORED_LADDER_DUST),
    )
}

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn add_tokens(cc: &ClientConfig, w: &UtexoWallet, n: usize) -> Result<()> {
    for _ in 0..n {
        let t = prepaid_token(cc).await?;
        w.add_prepaid_token(&t).await;
    }
    Ok(())
}

async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset)
        .map(|t| t.balance)
        .unwrap_or(0))
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

/// Mine `n` blocks and wait for the INDEXER on each — see sdk75's copy for the electrs/bitcoind race
/// that makes a well-mined transaction report as "can't be located in the blockchain".
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

/// Every CONFIRMED coin of `wallet` funded with exactly `sats`.
async fn coins_of_size(cc: &ClientConfig, wallet: &str, sats: u64) -> Result<Vec<String>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .into_iter()
        .filter(|c| {
            c.status == CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.amount == Some(sats as u32)
        })
        .filter_map(|c| c.statechain_id)
        .collect())
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk78_alice", "./rgb-data-sdk78_bob", "./rgb-data-sdk78_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;
    let (child_floor, root_floor) = floors();

    // The premise of the whole test, stated before anything is built: the legacy 1_500-sat piece
    // cannot carry a coloured ROOT ladder, so its receiver could never ladder it.
    //
    // [D44] At the OLD 2.0 rate it sat strictly BETWEEN the two floors (child 1_482, root 2_058) and
    // that gap was the stranding class. Raising the committed rate to 3.0 moved both floors up
    // (child 1_818, root 2_562), so 1_500 is now below BOTH — it cannot even be CARVED as a coloured
    // child. That is strictly worse for the legacy piece, not better, and the test says so rather
    // than asserting a band the constant has fallen out of.
    assert!(
        LEGACY_PIECE < root_floor,
        "the legacy 1_500-sat piece must be unable to carry a coloured ROOT ladder (floor \
         {root_floor}) — that inability IS the stranding class this test is about"
    );
    assert!(
        LEGACY_PIECE < child_floor,
        "[D44] at the 3 sat/vB rate 1_500 no longer clears even the coloured CHILD floor \
         ({child_floor}). If this ever flips back, the committed rate has been lowered and every \
         floor in this test must be re-derived with it"
    );
    assert!(
        PIECE >= root_floor,
        "a piece carved today must clear the root floor, or the hatch would feed itself"
    );

    let mut alice_cfg = SdkConfig::regtest("sdk78_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk78_alice".to_string());
    let mut bob_cfg = SdkConfig::regtest("sdk78_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk78_bob".to_string());
    // carol is the ONWARD hop: (c) is only evidence if what bob receives can leave his wallet again
    // and land, booked, somewhere else.
    let mut carol_cfg = SdkConfig::regtest("sdk78_carol");
    carol_cfg.rgb_data_dir = Some("./rgb-data-sdk78_carol".to_string());
    // THE FLIP IS THE PREMISE, SO IT IS SET, NOT INHERITED.
    //
    // This test is about what a wallet running the CTES-R lane does to a carrier it can never
    // colour, and the stranding it probes is CAUSED by the lane being on: `refuse_legacy_colored_
    // split_lane` only fires when `config.colored_ladder` is true, and the migration hatch only
    // exists to reopen what that refusal closes. With the lane off there is no retirement gate,
    // nothing is stranded, and (a)/(b)/(c) below would all pass vacuously.
    //
    // It used to read the flag off the DEFAULT and assert the default was ON. The default ships
    // **false** (2c351c6) — the lane is sound (sdk74/sdk75) but its economics keep it opt-in
    // (`docs/utexo/spec/PARTIAL-PAYMENT-ECONOMICS.md`). So the three wallets opt in by name and the
    // assertion below checks the thing that is actually load-bearing here: that every wallet in
    // this test IS running the lane, which is what makes the trap reachable at all. Nothing about
    // the stranding class, the floors or the hatch changes — 1_500 sits between the same two
    // floors whatever the default is.
    alice_cfg.colored_ladder = true;
    bob_cfg.colored_ladder = true;
    carol_cfg.colored_ladder = true;
    assert!(
        alice_cfg.colored_ladder && bob_cfg.colored_ladder && carol_cfg.colored_ladder,
        "every wallet here must run the CTES-R lane — this test is about what that lane does to \
         legacy carriers, and with it off there is nothing to strand"
    );
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let (carol, _) = UtexoWallet::initialize(carol_cfg, None).await?;
    let bob_address = bob.get_utexo_address().await?;

    let core = bitcoin_core::getnewaddress()?;
    let rgb_fund = alice.get_token_funding_address().await?;
    // DERIVED, not a magic number. `issue_inflatable_token_sized` calls
    // `create_utxos(inflation.len() + 2, TOKEN_CARRIER_SATS * 4, ..)` — one colourable UTXO per
    // allocation plus a spare — and each on-chain mint needs room of its own. With [D53]'s fourth
    // carrier the issuance alone wants 5 x 90_144 = 450_720 sat, which is why the old flat 400_000
    // failed with rgb-lib's `Insufficient allocations` the moment N_CARRIERS went from 3 to 4. A
    // literal here is a trap for the next person who changes the carrier count.
    let rgb_funding =
        (N_CARRIERS as u32 + 3) * mercury_utexo_sdk::tokens::TOKEN_CARRIER_SATS as u32 * 4;
    bitcoin_core::sendtoaddress(rgb_funding, &rgb_fund)?;
    mine_synced(&cc, &core, 3)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    println!(
        "SDK78 - (1) setup done: alice/bob/carol up with colored_ladder=ON, {rgb_funding} sat funded to \
         alice's RGB address, floors measured child {child_floor} < legacy {LEGACY_PIECE} < root \
         {root_floor} (today's piece {PIECE})"
    );

    // ---- 1. N_CARRIERS PRE-FLIP CARRIERS: 1_500 sats each, one asset. ---------------------------
    //
    // An IFA, because the class has to be reproduced on SEVERAL carriers of the SAME asset: a coin
    // below the root floor is below `TOKEN_PIECE_SATS + reserve` too, so a single-carrier split can
    // never fund a payment out of one — combining them is the only spend such coins have ever had,
    // before the flip as after it. `*_sized` is what makes a legacy-sized carrier constructible; the
    // default issuance path still funds at `TOKEN_CARRIER_SATS`.
    add_tokens(&cc, &alice, 2 * N_CARRIERS).await?;
    let asset_id = alice
        .issue_inflatable_token_sized(
            "ULEG",
            "Legacy Piece",
            0,
            SUPPLY,
            vec![MINT; N_CARRIERS - 1],
            LEGACY_PIECE,
        )
        .await?;
    println!("SDK78 - issued {SUPPLY} of {asset_id} onto a {LEGACY_PIECE}-sat (pre-flip) carrier");
    mine_synced(&cc, &core, 2)?;
    // Mint is an ON-CHAIN inflate and polls for its own confirmation, so it needs blocks while it
    // waits: a scoped background miner, exactly as sdk29 does it.
    let mining = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let miner = {
        let m = mining.clone();
        let core = core.clone();
        std::thread::spawn(move || {
            while m.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = bitcoin_core::generatetoaddress(1, &core);
                std::thread::sleep(Duration::from_secs(2));
            }
        })
    };
    let mut mint_res = Ok(());
    for _ in 0..N_CARRIERS - 1 {
        match alice.mint_tokens_sized(&asset_id, vec![MINT], LEGACY_PIECE).await {
            Ok((_, minted)) => assert_eq!(minted, MINT),
            Err(e) => {
                mint_res = Err(e);
                break;
            }
        }
    }
    mining.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = miner.join();
    mint_res?;
    mine_synced(&cc, &core, 2)?;
    let all_units = SUPPLY + (N_CARRIERS as u64 - 1) * MINT;
    println!(
        "SDK78 - (1) {} on-chain mints of {MINT} each settled onto {LEGACY_PIECE}-sat carriers \
         (background miner stopped); polling up to 90 claim passes for {N_CARRIERS} confirmed carriers",
        N_CARRIERS - 1
    );

    let mut legacy_sids: Vec<String> = Vec::new();
    for _ in 0..90 {
        mine_synced(&cc, &core, 1)?;
        alice.claim().await?;
        legacy_sids = coins_of_size(&cc, "sdk78_alice", LEGACY_PIECE).await?;
        if legacy_sids.len() >= N_CARRIERS && token_balance(&alice, &asset_id).await? == all_units {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        legacy_sids.len(),
        N_CARRIERS,
        "{N_CARRIERS} {LEGACY_PIECE}-sat carriers must have confirmed (found {legacy_sids:?})"
    );
    assert_eq!(
        token_balance(&alice, &asset_id).await?,
        all_units,
        "all {all_units} units must be settled across the {N_CARRIERS} legacy carriers"
    );
    println!(
        "SDK78 - (1) {N_CARRIERS} {LEGACY_PIECE}-sat carriers CONFIRMED {legacy_sids:?} holding all \
         {all_units} units of {asset_id}"
    );

    // ---- 2. (a) THE TRAP, MEASURED: not one of them can ever be coloured. -----------------------
    //
    // Ten further claim passes, so "no ladder" is a refusal rather than a race. The refusal is
    // `build_colored_ladder`'s pre-flight and it is arithmetic: 1_500 is below the coloured ROOT
    // floor at the PROTOCOL committed fee rate, and neither number can move.
    for _ in 0..10 {
        alice.claim().await?;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    for sid in legacy_sids.iter() {
        let ladder = mercuryrustlib::tesr::load(&cc, "sdk78_alice", sid).await?;
        assert!(
            ladder.is_none(),
            "carrier {sid} ({LEGACY_PIECE} sat) must have NO ladder at all — it cannot afford three \
             coloured rungs ({root_floor} sat), so a ladder row here would mean it was laddered \
             PLAIN, which would destroy the allocation on the first tier"
        );
    }
    println!(
        "SDK78 - (a) {N_CARRIERS} {LEGACY_PIECE}-sat carriers, all in the trap: {LEGACY_PIECE} is \
         below the child floor {child_floor} and the root floor {root_floor}, and none of them has \
         any ladder row after 10 claim passes"
    );

    // ---- 3. (b) SPENDABLE: the migration hatch pays 250 across all of them. ---------------------
    //
    // No single carrier holds 250, so this is the COMBINE route — one `create_colored_combine_tx`
    // spending every funding output. With the flip on that route is the retired lane, and the
    // ONLY reason it runs is that every input is permanently below the root floor and holds no
    // ladder, i.e. no trigger exists or can ever exist to rival the spend.
    add_tokens(&cc, &bob, 3).await?;
    let bob_bg = bob.start_background();
    let out = alice.transfer_tokens(&asset_id, &bob_address, PAY).await.map_err(|e| {
        anyhow!(
            "the flip STRANDED the legacy carriers: paying {PAY} of {asset_id} across \
             {N_CARRIERS} {LEGACY_PIECE}-sat carriers was refused ({e}). Every carrier must retain \
             at least one safe way out. If the refusal is about SATS, the escape bar moved: it is \
             `PIECE ({PIECE}) + reserve + min_split_output`, see the module comment."
        )
    })?;
    assert!(out.used_split, "a combine pays over a split, not a whole-coin handover");
    let piece_sid = out.coins[0].statechain_id.clone();
    assert_eq!(
        out.coins[0].amount_sats, PIECE,
        "the piece the hatch pays out must carry the CURRENT piece size"
    );
    println!(
        "SDK78 - (b) migration hatch COMBINE returned: {PAY} of {asset_id} sent to bob over \
         {N_CARRIERS} {LEGACY_PIECE}-sat inputs, used_split={}, piece {piece_sid} at {} sat; polling \
         up to 90 claim passes for bob to book it",
        out.used_split, out.coins[0].amount_sats
    );

    for _ in 0..90 {
        alice.claim().await?;
        if token_balance(&bob, &asset_id).await? == PAY {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        token_balance(&bob, &asset_id).await?,
        PAY,
        "bob must have booked {PAY} of {asset_id} out of the legacy carriers"
    );
    assert_eq!(
        token_balance(&alice, &asset_id).await?,
        all_units - PAY,
        "alice keeps the token change"
    );
    println!(
        "SDK78 - (b) SPENDABLE: {PAY} of {asset_id} paid to bob across all {N_CARRIERS} legacy \
         carriers through the migration hatch; piece {piece_sid} carries {PIECE} sat"
    );

    // ---- 4. alice's change is ITSELF in the class, and that is the coin to exit. ----------------
    // The combine's own arithmetic, re-derived here rather than pinned: piece + reserve out of the
    // aggregate, and the reserve floors at 300 for carriers this size.
    let change_sats = N_CARRIERS as u64 * LEGACY_PIECE - PIECE - 300;
    let change_sid = coins_of_size(&cc, "sdk78_alice", change_sats)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("alice's {change_sats}-sat combine change carrier is missing"))?;
    assert!(
        change_sats < root_floor,
        "the change carrier ({change_sats} sat) is in the class too — the hatch does not launder \
         value out of it, it moves the PAID part out and leaves the rest exactly where it was"
    );
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk78_alice", &change_sid).await?.is_none(),
        "the change carrier must have no ladder either"
    );
    println!(
        "SDK78 - (4) alice's combine CHANGE carrier {change_sid} holds {change_sats} sat — below \
         the root floor {root_floor}, so it is in the same class, and it has no ladder row"
    );

    // ---- 5. (d) EXITABLE: materialise, never sweep. ---------------------------------------------
    //
    // The pre-signed PLAIN backup of this coin is captured BEFORE the exit, so "it was not
    // broadcast" is a statement about a specific transaction rather than about an absence.
    let plain_backup_txid = {
        let rows = mercuryrustlib::sqlite_manager::get_backup_txs(
            &cc.pool,
            "sdk78_alice",
            &change_sid,
        )
        .await?;
        let latest = rows
            .iter()
            .max_by_key(|b| b.tx_n)
            .ok_or_else(|| anyhow!("the change carrier has no plain backup stored"))?;
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(&latest.tx)?)?;
        tx.txid().to_string()
    };
    assert!(
        onchain(&cc, &plain_backup_txid).is_none(),
        "the plain backup must be un-broadcast before the exit, or the after-shot proves nothing"
    );

    // The exit-everything default still excludes every carrier — the hatch is opt-in by NAME.
    let sweep_all = alice.unilateral_exit(None, None).await?;
    assert!(
        !sweep_all.iter().any(|s| s.statechain_id == change_sid),
        "an un-colourable carrier must stay OUT of the exit-everything default: materialising is an \
         on-chain broadcast and has to be asked for"
    );
    println!(
        "SDK78 - (d) exit-everything default returned {} exits and EXCLUDED the change carrier \
         {change_sid}; its plain backup {}... is un-broadcast going in",
        sweep_all.len(),
        &plain_backup_txid[..8]
    );

    // [D2] The re-derived assertion. The old one — `exits[0].complete` — asserted a field whose own
    // documentation ("branch and backup both broadcast") this arm can never satisfy, over a branch
    // replay that was a no-op. It is not weakened here, it no longer applies: the API stopped
    // claiming an exit it cannot perform. What must hold instead is that the refusal is HONEST and
    // ACTIONABLE, and that nothing was broadcast on the way to it.
    let refusal = alice
        .unilateral_exit(Some(vec![change_sid.clone()]), None)
        .await
        .err()
        .ok_or_else(|| {
            anyhow!(
                "unilateral_exit REPORTED SUCCESS for an un-colourable carrier. There is no \
                 SE-free spend of {change_sid} that preserves its allocation — every pre-signed \
                 spend of its funding output this wallet holds is RGB-unaware — so any success \
                 here is a false green on an escape hatch, and it will be believed."
            )
        })?
        .to_string();
    assert!(
        refusal.contains(&change_sid)
            && refusal.contains("materialise_carrier")
            && refusal.contains("transfer_tokens"),
        "the refusal must name the coin AND both routes that do work — `materialise_carrier` (settle \
         the allocation on chain) and `transfer_tokens` (the migration hatch, which step (b) just \
         exercised on these very carriers). A refusal that does not tell the holder what to do \
         instead strands them just as effectively as a lie: {refusal}"
    );
    println!("SDK78 - (d) unilateral_exit refuses honestly: {refusal}");

    // …and the route it names is real. THIS is what the old `complete:true` was actually doing: the
    // `branch-` rows of a carrier are the UN-BROADCAST coloured combine, so landing them settles the
    // allocation on chain for the first time and wins the clawback race. Same action, no longer
    // wearing an exit's clothes.
    let branch_broadcast = alice.materialise_carrier(&change_sid).await.map_err(|e| {
        anyhow!(
            "the route the refusal names does not work: materialise_carrier({change_sid}) — {e}. A \
             refusal that names an unusable route is the same false green in a different place."
        )
    })?;
    assert!(
        branch_broadcast,
        "alice's change carrier was carved by the legacy COMBINE, whose transaction is stored \
         un-broadcast as its exit branch — materialising it must broadcast that branch, not report \
         'no branch' (which would mean the allocation had no on-chain witness to settle)"
    );
    assert!(
        onchain(&cc, &plain_backup_txid).is_none(),
        "THE PLAIN BACKUP WAS BROADCAST ({plain_backup_txid}) — that is an RGB-unaware spend of the \
         carrier's funding output and it DESTROYS the allocation. Materialisation must broadcast the \
         branch and nothing else."
    );
    println!(
        "SDK78 - (d) materialise_carrier({change_sid}) BROADCAST the carrier's un-broadcast combine \
         branch (branch_broadcast={branch_broadcast}); plain backup {}... still off chain",
        &plain_backup_txid[..8]
    );
    mine_synced(&cc, &core, 3)?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // THE SURVIVAL PROOF, and it is the read-only stock probe, never a balance (E7 measured
    // `get_asset_balance` reporting a full settled balance over a stock at zero).
    let change_units = all_units - PAY;
    let mut probe = alice.probe_carrier_funding(&change_sid, &asset_id, change_units).await;
    for _ in 0..20 {
        if probe.is_ok() {
            break;
        }
        let msg = probe.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        if !msg.contains("can't be located in the blockchain") {
            break; // a real verdict, not indexer lag
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        probe = alice.probe_carrier_funding(&change_sid, &asset_id, change_units).await;
    }
    probe.map_err(|e| {
        anyhow!(
            "THE ALLOCATION DID NOT SURVIVE: the stash will not spend {change_units} of {asset_id} \
             out of the carrier's outpoint — {e}"
        )
    })?;
    let over = alice.probe_carrier_funding(&change_sid, &asset_id, change_units + 1).await;
    assert!(
        over.is_err(),
        "the stock probe accepted MORE than the allocation — it is not discriminating and its \
         success proves nothing"
    );
    println!(
        "SDK78 - (d) HONEST: the {change_sats}-sat un-colourable carrier has no SE-free exit and \
         `unilateral_exit` now says so (naming the migration hatch), plain backup \
         {plain_backup_txid} NOT broadcast, and the read-only color_psbt stock probe still spends \
         {change_units} of {asset_id} out of it ({} refused)",
        change_units + 1
    );

    // ---- 6. (c) [D3] THE RECEIVER CAN SPEND WHAT IT WAS PAID. ------------------------------------
    //
    // The piece carries `TOKEN_PIECE_SATS`, which CLEARS the root floor, and its funding is on chain
    // (step 5 broadcast the combine). So by SIZE it was never in the migration class — and yet, four
    // times running, rgb-lib refused to colour it: `Invalid coloring info` over its own funding
    // output, indefinitely. `RGB_E2E=16` isolated the cause with a control: the legacy receive path
    // imported the CONTRACT and wrote a sqlite row but never `accept_transfer`ed the transfer into
    // the stock, so no seal was revealed at the piece's outpoint and `color_psbt` saw
    // `available (0)`. `accept_incoming_tokens` now calls `accept_ladder` for the legacy lane too —
    // a legacy piece is a one-rung ladder — and the outcome is no longer optional.
    //
    // What is asserted here is a SPEND, not a refusal: bob's piece must get a coloured ladder, and
    // bob must be able to convey it onward to carol, who books the whole amount off-chain. The old
    // version of this section accepted either colouring outcome and then exercised the SENDER's
    // side, so it could not fail.
    //
    // The plain backup is captured first, so "it was not swept" is again about a named transaction.
    let piece_backup_txid = {
        let rows =
            mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk78_bob", &piece_sid).await?;
        let latest = rows
            .iter()
            .max_by_key(|b| b.tx_n)
            .ok_or_else(|| anyhow!("bob's piece has no plain backup stored"))?;
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(&latest.tx)?)?;
        tx.txid().to_string()
    };
    // (c.1) THE PIECE MUST BE COLOURABLE. Before the [D3] fix this loop ran to exhaustion: the
    // receive path had never accepted the transfer into bob's stock, so `build_colored_ladder`'s
    // `color_psbt` over the piece's own funding output answered `available (0)` on every pass. The
    // failure below therefore names the exact call whose absence caused it, so a regression is
    // diagnosable from the message alone.
    println!(
        "SDK78 - (c.1) bob's piece {piece_sid} plain backup {}... captured; polling up to 40 claim \
         passes for a COLOURED ladder on it",
        &piece_backup_txid[..8]
    );
    let mut piece_ladder = None;
    let mut colour_error = String::new();
    for _ in 0..40 {
        bob.claim().await?;
        if let Some(b) = mercuryrustlib::tesr::load(&cc, "sdk78_bob", &piece_sid).await? {
            if b.is_colored() {
                piece_ladder = Some(b);
                break;
            }
        }
        if let Err(e) = bob.probe_carrier_funding(&piece_sid, &asset_id, PAY).await {
            colour_error = e.to_string();
        }
        mine_synced(&cc, &core, 1)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    drop(bob_bg);
    let piece_ladder = piece_ladder.ok_or_else(|| {
        anyhow!(
            "[D3] bob's {PIECE}-sat piece could NOT be coloured after 40 claim passes (last \
             read-only stock probe: {colour_error}). It clears the root floor ({root_floor}) and its \
             funding is on chain, so this is the receive path, not the coin: check that \
             `accept_incoming_tokens` still calls `accept_ladder` for the LEGACY envelope lane. \
             Booking a piece with `import_asset_offchain` + `register_statechain` alone leaves the \
             stock with no witness ord and no revealed seal at the piece's outpoint, and every \
             later colouring answers `Invalid coloring info … available (0)` while \
             `get_asset_balance` still reports the full amount out of sqlite (RGB_E2E=16)."
        )
    })?;
    assert!(
        piece_ladder.f_value >= root_floor,
        "a piece carved today must clear the coloured root floor"
    );
    let (piece_contract, piece_amt, piece_txids, _) = bob.colored_ladder_health(&piece_sid).await?;
    assert_eq!(piece_contract, asset_id);
    assert_eq!(
        piece_amt, PAY,
        "the coloured ladder must carry the whole {PAY} bob was paid"
    );
    for txid in &piece_txids {
        assert!(
            onchain(&cc, txid).is_none(),
            "tier {txid} of bob's ladder must still be UN-BROADCAST"
        );
    }
    println!(
        "SDK78 - (c.1) COLOURABLE: bob's {PIECE}-sat piece holds a COLOURED ladder carrying \
         {piece_amt} of {asset_id} over {} un-broadcast tiers",
        piece_txids.len()
    );

    // (c.2) THE SPEND, and this is the assertion that replaced the one that could not fail. bob
    // conveys the piece he was PAID onward to carol — a real transfer of a real allocation out of
    // the receiver's wallet — and carol books all of it. Nothing here is bob's own issuance: every
    // unit came out of the four 1_500-sat legacy carriers, through the migration hatch, through
    // the legacy receive path, and out the other side.
    let carol_address = carol.get_utexo_address().await?;
    let before = tip(&cc)?;
    bob.transfer_colored_carrier(&piece_sid, &carol_address)
        .await
        .map_err(|e| {
            anyhow!(
                "bob CANNOT SPEND the piece he was paid: conveying {piece_sid} to carol was refused \
                 ({e}). A received piece that can be booked but never spent is stranded value, \
                 whatever the balance says."
            )
        })?;
    println!(
        "SDK78 - (c.2) bob CONVEYED piece {piece_sid} to carol at tip {before}; polling up to 40 \
         carol claim passes for her to book {PAY} of {asset_id}"
    );
    let mut carol_balance = 0;
    for _ in 0..40 {
        carol.claim().await?;
        carol_balance = token_balance(&carol, &asset_id).await?;
        if carol_balance == PAY {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        carol_balance, PAY,
        "carol must have booked the whole {PAY} bob conveyed — a spend that does not land at the \
         far end is not a spend"
    );
    assert_eq!(
        token_balance(&bob, &asset_id).await?,
        0,
        "bob must no longer hold the allocation he conveyed"
    );
    let carol_bundle = mercuryrustlib::tesr::load(&cc, "sdk78_carol", &piece_sid)
        .await?
        .ok_or_else(|| anyhow!("carol did not adopt the ladder of the piece bob conveyed"))?;
    assert!(
        carol_bundle.is_colored(),
        "carol adopted a ladder that lost its colour — the allocation would die on its first tier"
    );
    let (carol_contract, carol_amt, _, _) = carol.colored_ladder_health(&piece_sid).await?;
    assert_eq!(carol_contract, asset_id);
    assert_eq!(
        carol_amt, PAY,
        "carol's OWN leaf consignment must validate against her ladder for the full {PAY}"
    );
    assert_eq!(
        tip(&cc)?,
        before,
        "the onward hop must be entirely OFF-CHAIN — not one block was needed"
    );
    // And nothing RGB-unaware was ever broadcast for the piece on the way out.
    assert!(
        onchain(&cc, &piece_backup_txid).is_none(),
        "bob's PLAIN backup was broadcast ({piece_backup_txid}) — that is an RGB-unaware spend of \
         the piece's funding output and it destroys the allocation"
    );
    println!(
        "SDK78 - (c.2) SPENT: bob CONVEYED his {PIECE}-sat received piece to carol off-chain; carol \
         booked {carol_amt} of {asset_id} and validates her own ladder, bob dropped to 0, and bob's \
         plain backup {piece_backup_txid} was never broadcast"
    );

    println!(
        "SDK78 - PASS: with the flip ON, {N_CARRIERS} PRE-FLIP {LEGACY_PIECE}-sat carriers were (a) proved \
         un-colourable, (b) SPENT across the migration hatch, (d) the residue REFUSED an exit it \
         cannot perform and then MATERIALISED over the route that refusal names, with the \
         allocation intact and the plain sweep never broadcast, and (c) the piece paid out was \
         COLOURED at its receiver and then SPENT ONWARD to a third wallet that booked every unit. \
         The hatch stays shut for every carrier that \
         CAN still be coloured (unit `migration_hatch_is_narrow`; sdk75's negative control, where a \
         17_324-sat colourable carrier is still refused a unilateral exit)."
    );
    Ok(())
}
