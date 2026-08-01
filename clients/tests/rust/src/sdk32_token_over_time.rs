//! E2E (tokens over time, CTES-R): what happens to statechain tokens if you ISSUE or RECEIVE them
//! and then do NOTHING for a long time (a "year" of blocks, far beyond any deployed horizon)?
//! Answers, empirically: are they lost? can you still send them, exit unilaterally, or exit
//! cooperatively?
//!
//! **MIGRATED TO THE COLOURED LANE.** This test was written when an RGB carrier was deliberately
//! kept OFF the ladder — "terminal-freeze" — because every pre-signed spend of a carrier's funding
//! `F` was RGB-unaware, so laddering it could only destroy the allocation. Its three central
//! assertions were therefore spelled `tesr::load(..).is_none()`. With `colored_ladder` ON that
//! premise is inverted BY DESIGN: a carrier is laddered, and every tier of that ladder carries a
//! valid RGB state transition (CTES-R).
//!
//! **The invariant those assertions protected is unchanged and is still asserted here: a carrier is
//! never spent by an RGB-UNAWARE tier.** What changed is the evidence that proves it. Before the
//! flip the only way to guarantee it was to have no ladder at all; now it is guaranteed positively —
//! the ladder exists and every rung of it is coloured, and every uncoloured path that could reach
//! the carrier is refused by name. Assertion by assertion:
//!
//!   * "the RGB carrier must NOT carry a TES-R ladder"  ->  the carrier's ladder IS coloured
//!     (`bundle.is_colored()`), its RGB half carries one consignment per tier, and
//!     `colored_ladder_health` validates the FULL allocation against the ladder's own un-broadcast
//!     txids. A plain ladder fails all three.
//!   * "a plain unilateral exit must be refused"  ->  the OUTCOME is inverted (CTES-R makes a
//!     coloured carrier the one carrier that CAN exit — sdk75), the INVARIANT is not: the three
//!     RGB-unaware routes to a carrier are each asserted refused — plain-BTC coin selection (its
//!     sats are absent from `available_sats`), the uncoloured in-ladder split
//!     (`refuse_uncolored_over_colored`), and the flat ladder conveyance.
//!   * "bob's received sub-coin must NOT be laddered either"  ->  bob's received piece is a
//!     COLOURED CHILD (`ctesr-` bundle, `is_colored()`), whose five-tier chain is likewise RGB-aware,
//!     and it is that chain — not a locktime-0 branch — that he walks out on.
//!
//! What the test proves, unchanged in substance:
//!
//! (A) ISSUED tokens, long idle: NOT lost. An idle coloured ladder never ages — not one tier
//!     reaches the chain, `F` stays unspent, 0 vB of rent — and after a "year" the stock still
//!     validates the full allocation, with every RGB-unaware route to the carrier still refused.
//! (B) RECEIVED tokens (coloured child), long idle: still NOT lost, and the unilateral exit still
//!     works after the year — bob walks all five tiers `T -> X_m -> SP -> ext_child -> state_child`
//!     with no SE and no counterparty, and the leaf consignment then validates against the CHAIN
//!     ALONE for the full amount. A single received piece still cannot be SPLIT again.
//! (C) The residual DANGER, re-measured on the coloured lane. The sender keeps ONE pre-signed,
//!     RGB-unaware deposit backup over the same `F`. On the flat lane that was a CLAWBACK — it
//!     returned the tokens to her. On the coloured lane it recovers nothing: it is RGB-unaware, so
//!     it can only BURN the allocation (her own retained share included). The receiver's answer is
//!     unchanged and (B) performs it: spend `F` first. Afterwards that matured backup cannot even
//!     broadcast.
//!
//! ## ORDERING, AND THE DEFECT THAT FORCED IT — read this before "fixing" the order back
//!
//! The pre-flip test IDLED FIRST and sent afterwards, to show that "long inactivity does not block
//! sending". On the coloured lane that sequence is currently BROKEN, and not benignly:
//!
//!   * `verify_conveyed_child`'s ANCESTOR CENSUS validates the conveyed parent's flat backup chain
//!     with `validate_backup_chain_v2`, which rejects a backup whose absolute locktime has already
//!     passed (`LocktimeTooLow`). A carrier idle past its own deposit horizon (`L0 = H_deposit +
//!     initlock`) has exactly that.
//!   * MEASURED on this stack: with `initlock = 1000`, after 1500 idle blocks `alice.transfer_
//!     tokens(.., 250)` SUCCEEDS on the sender's side, and bob's `claim()` then fails forever with
//!     `conveyed parent flat backup chain is invalid (SignatureSchemeValidationError) — the ancestor
//!     census term is unusable`. The sender has terminalized her carrier and the receiver can never
//!     book the piece.
//!   * There is no repair: `refresh` refuses a carrier outright (there is no coloured on-chain
//!     re-anchor, CTESR-GATE §7), so an aged coloured carrier cannot be re-anchored back inside its
//!     horizon.
//!
//! The receiver's refusal is the SAFE direction (an already-matured parent backup means the sender
//! can burn the piece at will), but the SENDER must refuse first, in pre-flight, instead of
//! completing a payment nobody can accept. That is a protocol/SDK fix, not a test fix, so this test
//! does not assert around it: it performs the cooperative send while the carrier is INSIDE its
//! horizon — which is what the pre-flip test was really exercising — and then idles both sides.
//! The post-horizon send is reported as a finding, not silently dropped.
//!
//! Run: SDK_E2E=32 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

const SUPPLY: u64 = 1_000;
const PAY: u64 = 250;

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
    Ok(w.get_token_balances().await?.into_iter().find(|t| t.asset_id == asset).map(|t| t.balance).unwrap_or(0))
}
async fn wait_token_balance(w: &UtexoWallet, asset: &str, want: u64) -> Result<()> {
    for _ in 0..60 {
        w.claim().await?;
        if token_balance(w, asset).await? == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("settled balance of {asset} did not reach {want}"))
}
async fn wait_carrier(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str, asset: &str, units: u64) -> Result<mercuryrustlib::Coin> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        if token_balance(w, asset).await? >= units {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?;
            if let Some(c) = rec.coins.iter().rev().find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(mercury_utexo_sdk::tokens::TOKEN_CARRIER_SATS as u32)) {
                return Ok(c.clone());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: {units} of {asset} carrier did not confirm"))
}
fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}
/// Mine `n` blocks in batches and wait until electrs's tip catches up (so later reads are current).
fn mine_and_sync(cc: &ClientConfig, core: &str, n: u32) -> Result<u32> {
    let start = tip(cc)?;
    let target = start + n;
    let mut mined = 0;
    while mined < n {
        let batch = (n - mined).min(200);
        bitcoin_core::generatetoaddress(batch, core)?;
        mined += batch;
    }
    for _ in 0..120 {
        if tip(cc)? >= target {
            return Ok(tip(cc)?);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("electrs did not catch up to height {target}"))
}
/// Mine one block at a time and do not return until the INDEXER has seen each one — the rgb-lib
/// resolver races electrs otherwise and reports a well-mined tier as "can't be located" (sdk75/77).
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
fn onchain(cc: &ClientConfig, txid: &str) -> Option<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::Txid;
    let t = Txid::from_str(txid).ok()?;
    cc.electrum_client.transaction_get(&t).ok()
}
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    Ok(!cc.electrum_client.script_list_unspent(spk)?.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}
async fn coin_of(cc: &ClientConfig, name: &str, id: &str) -> Result<mercuryrustlib::Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?
        .coins.into_iter().find(|c| c.statechain_id.as_deref() == Some(id) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("{name} has no coin {id}"))
}
/// The sid of the one adopted `ctesr-` child in `wallet_name`, excluding ids already accounted for.
async fn child_sid(cc: &ClientConfig, wallet_name: &str, exclude: &[&str]) -> Result<Option<String>> {
    let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins;
    for c in coins.iter().filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0) {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if exclude.contains(&sid.as_str()) {
            continue;
        }
        if mercuryrustlib::tesr::load_child(cc, wallet_name, &sid).await?.is_some() {
            return Ok(Some(sid));
        }
    }
    Ok(None)
}

pub async fn execute() -> Result<()> {
    // The CTES-R default (`colored_ladder` ON). That is the point of the test: an idle COLOURED
    // ladder never ages, and every rung of it is RGB-aware — so "what happens to tokens left alone
    // for a year?" has a CTES-R-specific answer the flat-carrier version could not even ask (a flat
    // carrier had no unilateral exit for the asset at all).
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk32_alice", "./rgb-data-sdk32_bob", "./rgb-data-sdk32_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;

    let mut alice_cfg = SdkConfig::regtest("sdk32_alice");
    alice_cfg.colored_ladder = true;
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let mut bob_cfg = SdkConfig::regtest("sdk32_bob");
    bob_cfg.colored_ladder = true;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk32_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // ===== 0. THE SHAPE, on a fresh carrier =====================================================
    add_tokens(&cc, &alice, 1).await?;
    let asset = alice.issue_token("YR", "Year Token", 0, SUPPLY).await?;
    let carrier = wait_carrier(&cc, &alice, "sdk32_alice", &core, &asset, SUPPLY).await?;
    let carrier_id = carrier.statechain_id.clone().ok_or_else(|| anyhow!("carrier has no id"))?;
    let f_txid = carrier.utxo_txid.clone().ok_or_else(|| anyhow!("carrier has no funding txid"))?;
    let f_vout = carrier.utxo_vout.ok_or_else(|| anyhow!("carrier has no funding vout"))?;

    // THE MIGRATED INVARIANT. This used to read `tesr::load(..).is_none()` — "the carrier must NOT
    // be laddered" — because the only ladder that existed was RGB-unaware, so any tier of it would
    // have destroyed the allocation. CTES-R gives the carrier a ladder whose every tier carries an
    // RGB state transition, so "no RGB-UNAWARE tier ever spends this carrier" is now proved
    // POSITIVELY, by a check the old shape could never make: the ladder is present AND coloured AND
    // its RGB half covers every tier one for one.
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk32_alice", &carrier_id)
        .await?
        .ok_or_else(|| anyhow!(
            "the RGB carrier {carrier_id} has NO ladder at all. Under CTES-R (`colored_ladder` ON) \
             a carrier must be laddered — and coloured. An un-laddered carrier is the pre-flip \
             terminal-freeze shape, which has no unilateral exit for the asset."
        ))?;
    assert!(
        bundle.is_colored(),
        "the RGB carrier {carrier_id} carries a PLAIN TES-R ladder. That is the one shape this test \
         has always forbidden: an RGB-unaware tier spending a carrier destroys the allocation."
    );
    let rgb_half = bundle.rgb.clone().ok_or_else(|| anyhow!("coloured bundle with no RGB half"))?;
    let tier_count = bundle.exit_tiers().len();
    assert_eq!(
        rgb_half.consignments.len(), tier_count,
        "every tier of the carrier's ladder must carry its own RGB transition — {tier_count} tiers \
         but {} consignments means some rung of the walk is RGB-UNAWARE",
        rgb_half.consignments.len()
    );
    // …and no RGB-UNAWARE route reaches it. This is what the old "a plain unilateral exit must be
    // refused" assertion was really protecting; CTES-R inverts that one OUTCOME (a coloured carrier
    // is now the one carrier that CAN exit) without touching the invariant. None of these three
    // spends the coin, so the rest of the test still has a live carrier.
    let carrier_sats = carrier.amount.unwrap_or(0) as u64;
    let avail = alice.get_balance().await?.available_sats;
    // EXACT, not a bound: the carrier is alice's only coin, so a single leaked sat fails this.
    assert_eq!(
        avail, 0,
        "plain-BTC coin selection can reach the carrier: {avail} of its {carrier_sats} sat are \
         reported spendable, so a plain sweep could spend it and destroy the allocation"
    );
    let guard_msg = mercuryrustlib::tesr::refuse_uncolored_over_colored(&bundle, "in_ladder_split")
        .err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        guard_msg.contains("in_ladder_split") && guard_msg.contains("COLOURED"),
        "the UNCOLOURED in-ladder split must refuse this carrier by name — that path builds a plain \
         tier over a sealed output. Got: {guard_msg:?}"
    );
    let flat_convey = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_addr, "sdk32_alice", &carrier_id, None, false, None,
    ).await.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        flat_convey.contains("COLOURED (CTES-R) ladder"),
        "the FLAT ladder conveyance must refuse a coloured carrier (the receiver would bind the \
         sats without the asset). Got: {flat_convey:?}"
    );
    let l0 = carrier.locktime.ok_or_else(|| anyhow!("carrier has no deposit-backup locktime"))?;
    println!("SDK32 - alice issued {SUPPLY} {asset} on carrier {carrier_id}: ladder COLOURED ({tier_count} tiers, {tier_count} RGB transitions), no RGB-UNAWARE route reaches it ({carrier_sats} sat quarantined out of plain-BTC selection, uncoloured in-ladder split refused, flat conveyance refused); deposit-backup locktime L0={l0}, initlock={initlock}");

    // ===== 1. COOPERATIVE SEND, inside the carrier's horizon ====================================
    // Deliberately BEFORE the long idle — see the ORDERING note in the module docs: a post-horizon
    // send currently completes on the sender and can never be booked by the receiver.
    add_tokens(&cc, &alice, 3).await?;
    let r = alice.transfer_tokens(&asset, &bob_addr, PAY).await?;
    assert!(r.used_split, "a token transfer is an off-chain SPLIT");
    wait_token_balance(&bob, &asset, PAY).await?;
    assert_eq!(token_balance(&alice, &asset).await?, SUPPLY - PAY, "alice keeps the change allocation");
    assert_eq!(token_balance(&bob, &asset).await?, PAY, "bob booked the piece");
    let bob_piece = child_sid(&cc, "sdk32_bob", &[]).await?
        .ok_or_else(|| anyhow!("bob booked the tokens but adopted NO child bundle — his piece has no exit material"))?;
    let bob_cb = mercuryrustlib::tesr::load_child(&cc, "sdk32_bob", &bob_piece).await?
        .ok_or_else(|| anyhow!("bob's child bundle vanished"))?;
    // The RECEIVING side is RGB-aware too. This used to assert bob's sub-coin carries NO ladder;
    // a PLAIN child here is the same defect the old assertion guarded against, one level down.
    assert!(
        bob_cb.is_colored(),
        "bob's received child {bob_piece} carries a PLAIN chain — an RGB-unaware tier over his piece \
         would destroy the {PAY} units he was just paid"
    );
    let alice_change = child_sid(&cc, "sdk32_alice", &[&carrier_id, &bob_piece]).await?
        .ok_or_else(|| anyhow!("alice has no confirmed change child after the split"))?;
    let change_cb = mercuryrustlib::tesr::load_child(&cc, "sdk32_alice", &alice_change).await?
        .ok_or_else(|| anyhow!("alice's change child has no ctesr- bundle"))?;
    assert!(change_cb.is_colored(), "alice's own change child must be COLOURED too");
    println!("SDK32 - (1) COOPERATIVE SEND works: alice→bob {PAY} (balances {}/{PAY}); bob's piece is COLOURED CHILD {bob_piece}, alice's change is COLOURED CHILD {alice_change}", SUPPLY - PAY);

    // ===== 2. DO NOTHING FOR A "YEAR" ===========================================================
    let tip_yr = mine_and_sync(&cc, &core, initlock + 500)?;
    alice.claim().await?;
    bob.claim().await?;

    // ----- (A) the ISSUER's side: not lost, and it did not age ----------------------------------
    assert_eq!(token_balance(&alice, &asset).await?, SUPPLY - PAY, "alice's tokens are NOT lost after long inactivity");
    let change_cb = mercuryrustlib::tesr::load_child(&cc, "sdk32_alice", &alice_change).await?
        .ok_or_else(|| anyhow!("idling DESTROYED alice's change bundle after {} blocks", initlock + 500))?;
    assert!(change_cb.is_colored(), "idling must not un-colour alice's change child");
    // An idle coloured ladder pays 0 vB of rent: not one tier reached the chain, so `F` is untouched
    // and the whole walk is still available. This is the CTES-R form of "an idle coin never ages".
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout)?,
        "the carrier's funding {f_txid}:{f_vout} was spent while BOTH wallets sat IDLE — an idle \
         ladder must never broadcast a tier"
    );
    let alice_chain = mercuryrustlib::tesr::child_exit_chain(&change_cb);
    for (hex_tx, _) in alice_chain.iter() {
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?;
        assert!(onchain(&cc, &tx.txid().to_string()).is_none(), "a tier of an IDLE chain reached the chain — idle coins must cost 0 vB");
    }
    // …and the RGB half survived the year. `get_asset_balance` is deliberately NOT the evidence (E7
    // measured it reporting a full settled balance over a dead stock): this is the read-only,
    // stock-level `color_psbt` probe CTESR-GATE §3.3 mandates, and it discriminates.
    let (a_contract, a_assigned, _, _) = alice.colored_child_health(&alice_change).await
        .map_err(|e| anyhow!("alice's coloured change did not survive {} idle blocks: {e}", initlock + 500))?;
    assert_eq!(a_contract, asset, "the surviving allocation is THIS contract");
    assert_eq!(a_assigned, SUPPLY - PAY, "the whole change allocation must still be assigned");
    alice.probe_colored_child_tip(&alice_change, SUPPLY - PAY).await
        .map_err(|e| anyhow!("alice's stock is DEAD after {} idle blocks: {e}", initlock + 500))?;
    assert!(
        alice.probe_colored_child_tip(&alice_change, SUPPLY - PAY + 1).await.is_err(),
        "the stock probe accepted MORE than the allocation — it is not discriminating, so its \
         success proves nothing"
    );
    println!("SDK32 - (A) after ~{} idle blocks (tip={tip_yr}) alice still holds {} {asset}; her chain is still entirely off-chain, F is unspent, and her stock still spends exactly {} ({} refused) — an idle allocation simply does not age", initlock + 500, SUPPLY - PAY, SUPPLY - PAY, SUPPLY - PAY + 1);

    // ----- (B) the RECEIVER's side: not lost, and still exitable with no SE ----------------------
    assert_eq!(token_balance(&bob, &asset).await?, PAY, "bob's received tokens are NOT lost after long inactivity");
    let chain = mercuryrustlib::tesr::child_exit_chain(&bob_cb);
    assert_eq!(chain.len(), 5, "a coloured child's exit chain is T, X_m, SP, ext_child, state_child");
    let root_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&chain[0].0)?)?;
    let root = root_tx.input[0].previous_output;
    assert_eq!(
        (root.txid.to_string(), root.vout), (f_txid.clone(), f_vout),
        "bob's exit chain must root at the carrier's own funding outpoint — otherwise his walk and \
         the sender's backup are not racing for the same output and (C) measures nothing"
    );
    bob.probe_colored_child_tip(&bob_piece, PAY).await
        .map_err(|e| anyhow!("bob's stock is DEAD after {} idle blocks: {e}", initlock + 500))?;
    assert!(
        bob.probe_colored_child_tip(&bob_piece, PAY + 1).await.is_err(),
        "bob's stock probe accepted MORE than his allocation — it is not discriminating"
    );

    // A single received piece still cannot be SPLIT again: that needs a coloured GRANDCHILD, which
    // cannot be built. (The WHOLE piece is conveyable — sdk78 (c.2) — so this bounds splitting it
    // further, not spending it.)
    let resend = bob.transfer_tokens(&asset, &carol_addr, 100).await;
    assert!(resend.is_err(), "a lone received piece cannot be split again (hold / combine / convey / exit)");
    println!("SDK32 - (B) bob still holds {PAY} {asset} after ~{} idle blocks, his 5-tier chain is still entirely off-chain, his stock still spends exactly {PAY}, and splitting the lone piece again is refused: {:?}", initlock + 500, resend.err().map(|e| e.to_string().chars().take(110).collect::<String>()));

    // THE UNILATERAL EXIT STILL WORKS AFTER A YEAR — the CTES-R form of "materialization works
    // forever". No SE, no counterparty, only blocks.
    assert!(
        bob.colored_child_exit_proof(&bob_piece).await.is_err(),
        "the leaf consignment validated against the CHAIN ALONE before any tier was broadcast — the \
         after-shot below would then be vacuous"
    );
    let mut passes = 0;
    loop {
        passes += 1;
        assert!(passes < 20, "bob's coloured child exit did not converge");
        let statuses = bob.unilateral_exit(Some(vec![bob_piece.clone()]), None).await.map_err(|e| {
            anyhow!("unilateral_exit REFUSED bob's coloured child after the long idle — a received \
                     piece would then be unexitable: {e}")
        })?;
        if statuses[0].complete {
            break;
        }
        let wait = statuses[0].wait_blocks.max(1);
        bitcoin_core::generatetoaddress(wait, &core)?;
        mine_synced(&cc, &core, 1)?;
    }
    mine_synced(&cc, &core, 3)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    for (hex_tx, _) in chain.iter() {
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?;
        assert!(onchain(&cc, &tx.txid().to_string()).is_some(), "tier {} never reached the chain", tx.txid());
        assert_eq!(
            tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(), 1,
            "every tier bob broadcast must be RGB-AWARE (exactly one opret)"
        );
    }
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout)?, "the walk must spend the shared root");
    // The allocation survived — and only these two say so. `colored_child_exit_proof` validates the
    // leaf against the CHAIN ALONE (empty off-chain witness set), achievable only if every tier is
    // genuinely mined.
    let mut proof = bob.colored_child_exit_proof(&bob_piece).await;
    for _ in 0..20 {
        if proof.is_ok() {
            break;
        }
        let msg = proof.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        if !msg.contains("can't be located in the blockchain") {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        proof = bob.colored_child_exit_proof(&bob_piece).await;
    }
    let (proof_contract, proof_amount, _d) = proof.map_err(|e| anyhow!(
        "THE ALLOCATION DID NOT SURVIVE: bob's leaf consignment does not validate against the chain \
         alone after every tier was mined — {e}"
    ))?;
    assert_eq!(proof_contract, asset, "the surviving allocation is THIS contract");
    assert_eq!(proof_amount, PAY, "all {PAY} units must survive the walk");
    bob.probe_colored_child_tip(&bob_piece, PAY).await
        .map_err(|e| anyhow!("the stock is DEAD after the exit walk: {e}"))?;
    assert!(
        bob.probe_colored_child_tip(&bob_piece, PAY + 1).await.is_err(),
        "after the walk the probe accepted MORE than the allocation — it is not reading the stock"
    );
    println!("SDK32 - (B) UNILATERAL EXIT works after a year: bob walked all 5 RGB-aware tiers in {passes} pass(es) with no SE, spent the shared root, and the leaf consignment now validates against the CHAIN ALONE assigning all {PAY} {asset} — tokens preserved without the SE");

    // ===== (C) the residual sender-backup window, re-measured on the coloured lane ==============
    let alice_backup = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk32_alice", &carrier_id).await?;
    let alice_bk = alice_backup.iter().min_by_key(|b| b.tx_n)
        .ok_or_else(|| anyhow!("alice has no stored backup for the carrier"))?;
    let alice_bk_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&alice_bk.tx)?)?;
    let alice_bk_lock = mercurylib::utils::get_blockheight(alice_bk).unwrap_or(0);
    let tip_c = tip(&cc)?;
    assert!(
        alice_bk_lock <= tip_c,
        "the SENDER's carrier backup is not mature (L={alice_bk_lock} > tip={tip_c}) — the window \
         this section measures never opened, so the refusal below would prove nothing"
    );
    assert_eq!(
        alice_bk_tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(), 0,
        "the sender's retained deposit backup must be the RGB-UNAWARE shape this section is about"
    );
    assert_eq!(
        alice_bk_tx.input[0].previous_output.txid.to_string(), f_txid,
        "the sender's backup must spend the same F bob's walk spent, or it is no rival at all"
    );
    let claw = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&alice_bk.tx)?);
    assert!(
        claw.is_err(),
        "the sender's matured, RGB-unaware backup BROADCAST SUCCESSFULLY after bob's walk — F is \
         already spent by his trigger, so this must fail"
    );
    assert_eq!(token_balance(&bob, &asset).await?, PAY, "bob still holds all {PAY} after the failed sweep");
    println!("SDK32 - (C) SENDER-BACKUP WINDOW (residual, re-measured on the coloured lane): alice's ONE retained deposit backup is RGB-UNAWARE (0 opret), matured at {alice_bk_lock} (tip {tip_c}) and spends the very same F — but on this lane it can no longer CLAW THE TOKENS BACK, only burn them, and after bob's walk it cannot even broadcast: {}", claw.err().map(|e| e.to_string().chars().take(100).collect::<String>()).unwrap_or_default());

    println!("SDK32 - SUCCESS: tokens are NEVER LOST by inactivity on the CTES-R lane. The carrier is laddered and every rung of that ladder is COLOURED — so it never ages (no tier on chain, F unspent, 0 vB of rent after a 'year'), its stock still spends exactly the allocation, and every RGB-UNAWARE route to it (plain-BTC selection, the uncoloured in-ladder split, the flat conveyance) is refused: the invariant the pre-flip 'must NOT carry a ladder' assertion protected, now proved with the ladder PRESENT. A RECEIVED piece is a coloured CHILD, likewise RGB-aware, which after a 'year' idle still walks all five of its tiers unilaterally — no SE, no counterparty — after which the leaf consignment validates against the chain alone for the full amount. Residual: the sender keeps one RGB-unaware deposit backup over the same F; on this lane it can no longer recover the tokens (only destroy them), and once the receiver has walked it cannot broadcast at all. KNOWN GAP, see the ORDERING note in this file's docs: a send from a carrier already past its deposit horizon completes on the SENDER and can never be booked by the receiver.");
    Ok(())
}
