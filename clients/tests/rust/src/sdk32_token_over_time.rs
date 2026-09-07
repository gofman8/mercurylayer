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
//! (C) THE OLD HORIZON IS GONE. The sender used to keep ONE pre-signed, RGB-unaware deposit
//!     backup over the same `F` with an absolute locktime `L0 = H_deposit + initlock` — on the flat
//!     lane a CLAWBACK, on the coloured lane a BURN — and this section measured that matured backup
//!     failing to broadcast after (B)'s walk. There is no such backup any more: the carrier's
//!     ladder is co-signed at first sight of `F` in place of the flat `tx1`, `coin.locktime` is
//!     `None` for life, and the sender holds ZERO flat rows. What (C) measures instead is the
//!     consequence the old horizon used to forbid: AFTER the "year", alice pays carol out of the
//!     change tip that sat idle the whole time, and carol books exactly what the consignment
//!     assigns, with an EMPTY parent flat chain. Long inactivity does not block sending — which is
//!     what the pre-flip test was written to show, and what the coloured-lane rewrite had to stop
//!     showing.
//! (D) NO RESIDUAL. After bob's walk the sender still holds no RGB-unaware spend of `F`: zero flat
//!     rows, no locktime, and the legacy flat-backup broadcast refuses her carrier by name.
//!
//! ## ORDERING — why (C) sends AFTER the idle, and why an earlier rewrite could not
//!
//! The pre-flip test IDLED FIRST and sent afterwards. The coloured-lane rewrite had to reverse
//! that and recorded why as a KNOWN GAP: `verify_conveyed_child`'s ancestor census validated the
//! conveyed parent's FLAT backup chain with `validate_backup_chain_v2`, which rejected a rung whose
//! absolute locktime had passed (`LocktimeTooLow`) — so with `initlock = 1000`, after 1500 idle
//! blocks a send SUCCEEDED on the sender and bob's `claim()` failed forever, with no repair
//! (`refresh` refuses a carrier). That census term no longer exists: a child bundle's
//! `parent_flat_backups` must be EMPTY, a transfer conveys `backup_transactions: []`, and the
//! receiver refuses anything else by name. The post-horizon send is therefore measured here again,
//! as (C) — placed BEFORE bob's walk in (B), because adopting a child requires the parent's `F`
//! unspent and the walk spends it.
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
/// The POST-HORIZON payment of (C), carved out of alice's idle change tip.
const PAY2: u64 = 100;

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

/// The sid of the one `spinetip-` row in `wallet_name` — the SENDER'S OWN CHANGE leg.
///
/// **This is not a stylistic sibling of [`child_sid`]; it is the whole distinction.** A CATS-B split
/// writes the payee's piece under `ctesr-` and the sender's change under `spinetip-`, because the two
/// are different shapes with different readers: a `ctesr-` row is a conveyable leaf with two tiers
/// and a payee, a `spinetip-` row is one cap over `SP.out[K]` paying this wallet's own key. Asking
/// `load_child` about the change leg returns `None`, and a test that read that as "alice has no
/// change" would report a missing coin rather than a mis-keyed lookup.
async fn tip_sid(cc: &ClientConfig, wallet_name: &str, exclude: &[&str]) -> Result<Option<String>> {
    let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins;
    for c in coins.iter().filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0) {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if exclude.contains(&sid.as_str()) {
            continue;
        }
        if mercuryrustlib::tesr::load_spine_tip(cc, wallet_name, &sid).await?.is_some() {
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
    // carol RECEIVES in (C), so she is on the coloured lane too: a coloured child is adopted by a
    // wallet with an RGB engine and `colored_ladder` on, exactly as bob.
    let mut carol_cfg = SdkConfig::regtest("sdk32_carol");
    carol_cfg.colored_ladder = true;
    let (carol, _) = UtexoWallet::initialize(carol_cfg, None).await?;
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
    // …and there is no deposit-backup calendar on it at all: no locktime, no flat row. The OLD
    // horizon `L0 = H_deposit + initlock` used to be read here; it has no source any more.
    assert_eq!(
        carrier.locktime, None,
        "a laddered carrier has no absolute calendar: coin.locktime must be None"
    );
    assert_eq!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, "sdk32_alice", &carrier_id)
            .await?
            .map_or(0, |r| r.len()),
        0,
        "the issuer holds ZERO flat backup rows for the carrier — a flat rung would be exactly the \
         RGB-unaware spend of F this test used to measure as the residual danger"
    );
    let tip_at_issue = tip(&cc)?;
    println!("SDK32 - alice issued {SUPPLY} {asset} on carrier {carrier_id}: ladder COLOURED ({tier_count} tiers, {tier_count} RGB transitions), no RGB-UNAWARE route reaches it ({carrier_sats} sat quarantined out of plain-BTC selection, uncoloured in-ladder split refused, flat conveyance refused); no locktime, no flat row; tip={tip_at_issue}, initlock={initlock}");

    // ===== 1. COOPERATIVE SEND, before the idle =================================================
    // The first send. (C) repeats the exercise AFTER the "year", out of the change tip this one
    // leaves behind — see the ORDERING note in the module docs.
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
    let alice_change = tip_sid(&cc, "sdk32_alice", &[&carrier_id, &bob_piece]).await?
        .ok_or_else(|| anyhow!("alice has no confirmed change SPINE TIP after the split"))?;
    let change_tip = mercuryrustlib::tesr::load_spine_tip(&cc, "sdk32_alice", &alice_change).await?
        .ok_or_else(|| anyhow!("alice's change leg has no spinetip- record"))?;
    assert!(change_tip.is_colored(), "alice's own change tip must be COLOURED too");
    // …and it must be a TIP, not a leaf. If the change leg were ever written under `ctesr-` it would
    // be handed to every reader that treats such a row as someone else's coin that arrived here.
    assert!(
        mercuryrustlib::tesr::load_child(&cc, "sdk32_alice", &alice_change).await?.is_none(),
        "alice's own change leg is keyed `ctesr-` — the payee-leaf key. It is her change, and the \
         flat-lane licence, the carrier exit allowlist and the tower's child loop all read that key \
         as a coin that arrived from someone else"
    );
    println!("SDK32 - (1) COOPERATIVE SEND works: alice→bob {PAY} (balances {}/{PAY}); bob's piece is COLOURED CHILD {bob_piece}, alice's change is her own COLOURED SPINE TIP {alice_change}", SUPPLY - PAY);

    // ===== 2. DO NOTHING FOR A "YEAR" ===========================================================
    let tip_yr = mine_and_sync(&cc, &core, initlock + 500)?;
    alice.claim().await?;
    bob.claim().await?;

    // ----- (A) the ISSUER's side: not lost, and it did not age ----------------------------------
    assert_eq!(token_balance(&alice, &asset).await?, SUPPLY - PAY, "alice's tokens are NOT lost after long inactivity");
    let change_tip = mercuryrustlib::tesr::load_spine_tip(&cc, "sdk32_alice", &alice_change).await?
        .ok_or_else(|| anyhow!("idling DESTROYED alice's change tip after {} blocks", initlock + 500))?;
    assert!(change_tip.is_colored(), "idling must not un-colour alice's change tip");
    // An idle coloured ladder pays 0 vB of rent: not one tier reached the chain, so `F` is untouched
    // and the whole walk is still available. This is the CTES-R form of "an idle coin never ages".
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout)?,
        "the carrier's funding {f_txid}:{f_vout} was spent while BOTH wallets sat IDLE — an idle \
         ladder must never broadcast a tier"
    );
    let alice_chain = mercuryrustlib::tesr::spine_tip_exit_chain(&change_tip);
    for (hex_tx, _) in alice_chain.iter() {
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?;
        assert!(onchain(&cc, &tx.txid().to_string()).is_none(), "a tier of an IDLE chain reached the chain — idle coins must cost 0 vB");
    }
    // …and the RGB half survived the year. `get_asset_balance` is deliberately NOT the evidence (E7
    // measured it reporting a full settled balance over a dead stock): this is the read-only,
    // stock-level `color_psbt` probe CTESR-GATE §3.3 mandates, and it discriminates.
    let (a_contract, a_assigned, _, _) = alice.colored_tip_health(&alice_change).await
        .map_err(|e| anyhow!("alice's coloured change tip did not survive {} idle blocks: {e}", initlock + 500))?;
    assert_eq!(a_contract, asset, "the surviving allocation is THIS contract");
    assert_eq!(a_assigned, SUPPLY - PAY, "the whole change allocation must still be assigned");
    alice.probe_colored_spine_tip(&alice_change, SUPPLY - PAY).await
        .map_err(|e| anyhow!("alice's stock is DEAD after {} idle blocks: {e}", initlock + 500))?;
    assert!(
        alice.probe_colored_spine_tip(&alice_change, SUPPLY - PAY + 1).await.is_err(),
        "the stock probe accepted MORE than the allocation — it is not discriminating, so its \
         success proves nothing"
    );
    println!("SDK32 - (A) after ~{} idle blocks (tip={tip_yr}) alice still holds {} {asset}; her chain is still entirely off-chain, F is unspent, and her stock still spends exactly {} ({} refused) — an idle allocation simply does not age", initlock + 500, SUPPLY - PAY, SUPPLY - PAY, SUPPLY - PAY + 1);

    // ----- (C) the OLD horizon is gone: a send AFTER the "year" still books ---------------------
    // Placed BEFORE (B)'s walk: adopting a child requires the parent's `F` unspent, and the walk
    // spends it. Under the flat chain this send completed on the sender and could never be booked
    // (the receiver's ancestor census refused the matured rung) — the KNOWN GAP this file used to
    // carry. With no flat chain there is nothing that aged, and it must simply work.
    assert!(
        tip_yr >= tip_at_issue + initlock + 500,
        "(C) test hygiene: the tip ({tip_yr}) must be past the OLD horizon (issuance tip \
         {tip_at_issue} + initlock {initlock}), or this section would not be measuring a \
         post-horizon send"
    );
    let carol_slot = carol.get_utexo_address().await?;
    add_tokens(&cc, &alice, 2).await?;
    let r2 = alice.transfer_tokens(&asset, &carol_slot, PAY2).await.map_err(|e| anyhow!(
        "(C) alice could not pay {PAY2} out of a change tip idle for {} blocks — past the OLD \
         horizon a send used to complete on the sender and fail forever at the receiver's \
         flat-chain census; with no flat chain it must simply work: {e:#}",
        initlock + 500
    ))?;
    assert!(r2.used_split, "(C) a partial pay out of the tip carves a piece");
    wait_token_balance(&carol, &asset, PAY2).await.map_err(|e| anyhow!(
        "(C) carol could not BOOK the post-horizon piece — the receiver refused a child of a \
         carrier that had sat idle past deposit + initlock, i.e. the KNOWN GAP is back: {e:#}"
    ))?;
    let carol_piece = child_sid(&cc, "sdk32_carol", &[]).await?
        .ok_or_else(|| anyhow!("(C) carol booked the tokens but adopted NO child bundle"))?;
    let carol_cb = mercuryrustlib::tesr::load_child(&cc, "sdk32_carol", &carol_piece).await?
        .ok_or_else(|| anyhow!("(C) carol's child bundle vanished"))?;
    assert!(carol_cb.is_colored(), "(C) carol's post-horizon child must be COLOURED");
    assert!(
        carol_cb.parent_flat_backups.is_empty(),
        "(C) the child bundle must convey an EMPTY parent flat chain — the census term that used to \
         age is gone; got {}",
        carol_cb.parent_flat_backups.len()
    );
    let (c_contract, c_assigned, _, _) = carol.colored_child_health(&carol_piece).await?;
    assert_eq!(c_contract, asset, "(C) carol's consignment is for THIS contract");
    assert_eq!(c_assigned, PAY2, "(C) carol books EXACTLY what the consignment assigns");
    assert_eq!(
        token_balance(&alice, &asset).await?,
        SUPPLY - PAY - PAY2,
        "(C) alice keeps the remainder on a fresh tip"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout)?,
        "(C) a cooperative send publishes nothing — F is still unspent"
    );
    println!("SDK32 - (C) POST-HORIZON SEND WORKS: {} blocks after issuance (> initlock {initlock}) alice paid carol {PAY2} out of her idle change tip and carol booked it (child {carol_piece}, empty parent flat chain); alice keeps {}", tip_yr - tip_at_issue, SUPPLY - PAY - PAY2);

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

    // ===== (D) NO RESIDUAL: the sender holds no RGB-unaware spend of F ============================
    // The section that used to sit here measured the sender's ONE retained deposit backup —
    // matured, RGB-unaware, spending the same `F` — failing to broadcast after bob's walk. There is
    // no such backup to measure: zero flat rows, no locktime, and the legacy flat-backup broadcast
    // refuses the carrier BY NAME. The only spends of `F` that exist are the coloured trigger and
    // the tiers beneath it, which is what bob just walked.
    assert_eq!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, "sdk32_alice", &carrier_id)
            .await?
            .map_or(0, |r| r.len()),
        0,
        "(D) the sender holds ZERO flat backup rows for the carrier after everything"
    );
    assert_eq!(
        coin_of(&cc, "sdk32_alice", &carrier_id).await?.locktime,
        None,
        "(D) the sender's carrier has no locktime"
    );
    let legacy_msg = mercuryrustlib::broadcast_backup_tx::execute(&cc, "sdk32_alice", &carrier_id, None, None)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        legacy_msg.contains("no flat backup transaction to broadcast"),
        "(D) the legacy flat-backup broadcast must refuse a laddered carrier by name — got: {legacy_msg:?}"
    );
    assert_eq!(token_balance(&bob, &asset).await?, PAY, "(D) bob still holds all {PAY}");
    println!("SDK32 - (D) NO RESIDUAL: the sender holds no flat row and no locktime for the carrier, and the legacy broadcast refuses it by name ({})", legacy_msg.chars().take(100).collect::<String>());

    println!("SDK32 - SUCCESS: tokens are NEVER LOST by inactivity on the CTES-R lane. The carrier is laddered at first sight of F and every rung of that ladder is COLOURED — so it never ages (no tier on chain, F unspent, 0 vB of rent after a 'year'), its stock still spends exactly the allocation, and every RGB-UNAWARE route to it (plain-BTC selection, the uncoloured in-ladder split, the flat conveyance) is refused: the invariant the pre-flip 'must NOT carry a ladder' assertion protected, now proved with the ladder PRESENT. There is NO calendar on it: no locktime, no flat row, so a send AFTER the 'year' out of the idle change tip books normally with an empty parent flat chain — the KNOWN GAP the coloured-lane rewrite carried is gone with the chain that caused it. A RECEIVED piece is a coloured CHILD, likewise RGB-aware, which after a 'year' idle still walks all five of its tiers unilaterally — no SE, no counterparty — after which the leaf consignment validates against the chain alone for the full amount. And there is no residual: the sender holds no RGB-unaware spend of F at all.");
    Ok(())
}
