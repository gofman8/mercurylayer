//! E2E (SDK_E2E=80) — **ADVERSARIAL: the D1 window that A1/A2 did NOT close.**
//!
//! A1 fixed the ordering on `child_retransfer` / `cosign_colored_child_retransfer`; A2 made the
//! liveness key durable on those two lanes and on `transfer_sender::execute_ex`. Every one of those
//! moves the coin OUT of CONFIRMED **before** the superseding co-sign.
//!
//! `UtexoWallet::child_in_ladder_pay_many` (clients/libs/rust-sdk/src/transfer.rs:930) — the
//! `transfer_many` route for a RECEIVED child, and the lane `transfer()` takes for a plain payment
//! out of a received child — was not changed. Its order is:
//!
//!   1. `tesr::child_in_ladder_split` — `set_spend_budget(child, 1)` then `cosign_tier(CSP)`.
//!      `CSP` spends `ext_child`'s payload output, i.e. the SAME outpoint the child's current
//!      `child_state` spends, and every grandchild's ladder is rooted on `CSP`;
//!   2. `persist_child(change grandchild)`;
//!   3. `convey_child_bundle` × N — **the recipients now hold rival material**;
//!   4. …and only THEN the coin record is written, with the child set WITHDRAWN.
//!
//! Meanwhile `ctesr-<child>` is never rewritten (the terminalized child segment is handed to the
//! grandchildren as an ANCESTOR and dropped), so this wallet's row goes on naming the superseded
//! `child_state` as live for the whole call.
//!
//! `defend_ladders_inner`'s child loop (clients/libs/rust-sdk/src/wallet.rs:1863-1877) has exactly
//! ONE filter: `live_sids.contains(cid)` — the coin's DURABLE status. It has no L2 supersession
//! check at all, and it could not have one here: L2 is keyed on `ChildTesrBundle::
//! parent_statechain_id` / `parent.current().state.txid`, and a grandchild bundle carries the ROOT
//! parent's ids unchanged — the child's own terminalization lives in `ancestors`, which L2 never
//! reads.
//!
//! So for the whole of step 3 the tower is admitted to drive a state the recipients' chains
//! supersede. This test measures that window with the same instrument sdk79 used, and with both
//! halves observed independently of any fix:
//!
//!  * the SE's `num_sigs` for the CHILD — the coordinator's own counter — rising above baseline is
//!    the CSP co-sign, i.e. the instant a superseding state exists;
//!  * the RECIPIENT's mailbox depth at the coordinator (`transfer/get_msg_addr/<auth_pubkey>`)
//!    rising above baseline is a CONVEYANCE having landed, i.e. the instant a third party holds
//!    rival material. Neither marker is anything this wallet writes.
//!
//! A sample where the mailbox has grown AND the wallet DB still reads CONFIRMED for the child is a
//! watchtower pass that L1 would have admitted while a stranger held the superseding bundle. That
//! count must be zero. It is not.
//!
//! Two further assertions keep the finding from resting on the sampler alone:
//!
//!  * STRUCTURAL RIVALRY — the state `ctesr-<child>` still names live and the `CSP` the recipients
//!    depend on are deserialised from their stored hex and shown to spend the SAME outpoint. That
//!    is what makes an admitted pass a theft rather than noise.
//!  * BEHAVIOURAL ADMISSION — with the child's trigger broadcast so the pass has something to do,
//!    `defend_ladders` is run twice on the very same row and backend: once with the durable status
//!    forced back to CONFIRMED (it ACTS) and once as booked, WITHDRAWN (it does not). The durable
//!    status is therefore the only thing standing between this row and the recipients, exactly as
//!    A2 says — which is why writing it LAST is the defect.
//!
//! Run: SDK_E2E=80 ML_NETWORK=regtest cargo +stable run

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

const DEPOSIT: u64 = 200_000;
/// Non-exact, so `transfer()` takes the in-ladder split and bob ends up holding a RECEIVED CHILD.
const PAY: u64 = 60_000;
/// What bob then pays out of that child. THREE recipients, not one: the window this test measures
/// opens at the FIRST `convey_child_bundle` and closes at the single coin-record write after the
/// LAST one, so every extra recipient widens it by one conveyance. Two recipients already resolve
/// it; three make the measurement comfortable rather than marginal.
const PAY_C: u64 = 12_000;
const PAY_D: u64 = 11_000;
const PAY_E: u64 = 10_000;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn ladder_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn se_num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or_else(|| anyhow!("no statechain_info for {sid}"))?
        .num_sigs)
}

/// How many conveyance messages the coordinator is holding for `auth_pubkey`. The coordinator's own
/// view of "a bundle has been handed to this recipient" — nothing this wallet writes, and readable
/// without the recipient's keys.
async fn mailbox_depth(cc: &ClientConfig, auth_pubkey: &str) -> Result<usize> {
    #[derive(serde::Deserialize)]
    struct Resp {
        list_enc_transfer_msg: Vec<String>,
    }
    let client = cc.get_reqwest_client()?;
    let url = format!("{}/transfer/get_msg_addr/{auth_pubkey}", cc.statechain_entity);
    let r = client.get(&url).send().await?;
    if !r.status().is_success() {
        return Err(anyhow!("get_msg_addr {auth_pubkey} -> {}", r.status()));
    }
    Ok(r.json::<Resp>().await?.list_enc_transfer_msg.len())
}

/// A concurrent, read-only witness of exactly the join `defend_ladders`' child loop performs, gated
/// on two coordinator-side markers so that neither half of the sample depends on the code under
/// test.
#[derive(Clone)]
struct Witness {
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    samples: Arc<AtomicU32>,
    /// Samples taken after the SE showed the superseding `CSP` co-sign.
    post_cosign: Arc<AtomicU32>,
    /// Samples taken after a child bundle had actually reached a recipient's mailbox.
    post_convey: Arc<AtomicU32>,
    /// Of THOSE, the ones where the wallet DB still read CONFIRMED for the child — a pass L1 admits
    /// while a stranger holds the superseding bundle.
    admitted: Arc<AtomicU32>,
}

fn spawn_witness(
    wallet_name: &str,
    child_sid: &str,
    baseline_sigs: u32,
    recipient_auth: &str,
    baseline_msgs: usize,
) -> (Witness, tokio::task::JoinHandle<()>) {
    let w = Witness {
        stop: Arc::new(AtomicBool::new(false)),
        ready: Arc::new(AtomicBool::new(false)),
        samples: Arc::new(AtomicU32::new(0)),
        post_cosign: Arc::new(AtomicU32::new(0)),
        post_convey: Arc::new(AtomicU32::new(0)),
        admitted: Arc::new(AtomicU32::new(0)),
    };
    let me = w.clone();
    let wallet_name = wallet_name.to_string();
    let child_sid = child_sid.to_string();
    let recipient_auth = recipient_auth.to_string();
    let handle = tokio::spawn(async move {
        // Its own config — its own pool and HTTP client — so it never serialises behind the call it
        // is watching.
        let cc = mercuryrustlib::client_config::load().await;
        me.ready.store(true, Ordering::SeqCst);
        while !me.stop.load(Ordering::SeqCst) {
            // Every failed read is recorded as "not yet", which can only UNDER-count the defect and
            // can never invent one. `post_convey` is what stops the under-count from turning into a
            // silently green test.
            let ns = match mercuryrustlib::utils::get_statechain_info(&child_sid, &cc).await {
                Ok(Some(i)) => i.num_sigs,
                _ => 0,
            };
            let msgs = mailbox_depth(&cc, &recipient_auth).await.unwrap_or(0);
            let status = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &wallet_name)
                .await
                .ok()
                .and_then(|wl| {
                    wl.coins.into_iter().find(|c| {
                        c.statechain_id.as_deref() == Some(child_sid.as_str())
                            && c.duplicate_index == 0
                    })
                })
                .map(|c| c.status);
            me.samples.fetch_add(1, Ordering::SeqCst);
            if ns > baseline_sigs {
                me.post_cosign.fetch_add(1, Ordering::SeqCst);
                if msgs > baseline_msgs {
                    me.post_convey.fetch_add(1, Ordering::SeqCst);
                    if status == Some(CoinStatus::CONFIRMED) {
                        me.admitted.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    (w, handle)
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

/// Durably set a coin's status — the ONE field the child loop's liveness allowlist reads.
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

fn signed_tier_txid(signed_tx_hex: &str) -> Result<String> {
    use electrum_client::bitcoin as btc;
    let tx: btc::Transaction = btc::consensus::deserialize(&hex::decode(signed_tx_hex)?)?;
    Ok(tx.txid().to_string())
}

fn prevout_of(signed_tx_hex: &str) -> Result<String> {
    use electrum_client::bitcoin as btc;
    let tx: btc::Transaction = btc::consensus::deserialize(&hex::decode(signed_tx_hex)?)?;
    let i = tx
        .input
        .first()
        .ok_or_else(|| anyhow!("tier transaction has no input"))?;
    Ok(format!("{}:{}", i.previous_output.txid, i.previous_output.vout))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let alice = ladder_wallet("sdk80_alice").await?;
    // The watchtower is driven EXPLICITLY here; a background pass would make "which pass did what"
    // unanswerable.
    let mut bob_cfg = SdkConfig::regtest("sdk80_bob");
    bob_cfg.auto_exit = false;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let carol = ladder_wallet("sdk80_carol").await?;
    let dave = ladder_wallet("sdk80_dave").await?;

    let bob_address = bob.get_utexo_address().await?;
    let carol_address = carol.get_utexo_address().await?;
    let dave_address = dave.get_utexo_address().await?;
    let core = bitcoin_core::getnewaddress()?;

    // ---- 1. alice deposits, ladders, and pays bob a non-exact amount: bob holds a RECEIVED CHILD.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    mine_synced(&cc, &core, 3)?;
    for i in 0..60 {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats == DEPOSIT {
            break;
        }
        if i == 59 {
            return Err(anyhow!("alice's deposit never confirmed"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk80_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or_else(|| anyhow!("alice has no confirmed coin"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk80_alice", &alice_sid).await?.is_some(),
        "alice's coin must be LADDERED — otherwise bob never receives a child at all"
    );
    let r = alice.transfer(&bob_address, PAY).await?;
    assert!(r.used_split, "a {PAY} payment out of one {DEPOSIT} laddered coin must split in-ladder");

    let mut bob_child_sid = String::new();
    for i in 0..60 {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats == PAY {
            bob_child_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk80_bob")
                .await?
                .coins
                .iter()
                .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(PAY as u32))
                .and_then(|c| c.statechain_id.clone())
                .ok_or_else(|| anyhow!("bob has no confirmed child coin"))?;
            break;
        }
        if i == 59 {
            return Err(anyhow!("bob never adopted the split child"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let cb0 = mercuryrustlib::tesr::load_child(&cc, "sdk80_bob", &bob_child_sid)
        .await?
        .ok_or_else(|| anyhow!("bob did not persist a child bundle"))?;
    let live_before = cb0.child_state.txid.clone();
    println!(
        "SDK80 - (1) bob holds RECEIVED CHILD {bob_child_sid} ({PAY} sat); its `ctesr-` row names \
         child_state {} as live, over ext_child {}",
        &live_before[..12],
        &cb0.child_extension.txid[..12]
    );

    // ---- 2. bob pays TWO recipients out of that child, RACED by a read-only witness. -------------
    for _ in 0..4 {
        let t = prepaid_token(&cc).await?;
        bob.add_prepaid_token(&t).await;
    }
    let (_, _, carol_auth) = mercurylib::decode_transfer_address(&carol_address)?;
    let carol_auth = carol_auth.to_string();
    let baseline_sigs = se_num_sigs(&cc, &bob_child_sid).await?;
    let baseline_msgs = mailbox_depth(&cc, &carol_auth).await?;
    println!(
        "SDK80 - (2) baseline: SE num_sigs({}) = {baseline_sigs}, carol's coordinator mailbox holds \
         {baseline_msgs} message(s)",
        &bob_child_sid[..12]
    );

    let (wit, wit_handle) =
        spawn_witness("sdk80_bob", &bob_child_sid, baseline_sigs, &carol_auth, baseline_msgs);
    while !wit.ready.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let recipients = vec![
        (carol_address.clone(), PAY_C),
        (dave_address.clone(), PAY_D),
        (carol_address.clone(), PAY_E),
    ];
    let (piece_sids, change_sid) = bob
        .child_in_ladder_pay_many(&bob_child_sid, &recipients)
        .await?;
    wit.stop.store(true, Ordering::SeqCst);
    let _ = wit_handle.await;

    let samples = wit.samples.load(Ordering::SeqCst);
    let post_cosign = wit.post_cosign.load(Ordering::SeqCst);
    let post_convey = wit.post_convey.load(Ordering::SeqCst);
    let admitted = wit.admitted.load(Ordering::SeqCst);
    println!(
        "SDK80 - (2) call returned: {} piece(s) {piece_sids:?}, change {change_sid}. Witness: \
         {samples} sample(s), {post_cosign} after the CSP co-sign, {post_convey} after a bundle had \
         reached carol's mailbox, {admitted} of those with bob's child still CONFIRMED on disk",
        piece_sids.len()
    );
    assert!(
        post_convey > 0,
        "the witness never saw a sample in which a bundle had been conveyed AND the CSP was \
         co-signed — the instrument did not observe the window at all, so this run proves nothing \
         either way"
    );

    // ---- 3. STRUCTURAL RIVALRY — what an admitted pass would broadcast. -------------------------
    let cb1 = mercuryrustlib::tesr::load_child(&cc, "sdk80_bob", &bob_child_sid)
        .await?
        .ok_or_else(|| anyhow!("bob's `ctesr-` row vanished"))?;
    let gc = mercuryrustlib::tesr::load_child(&cc, "sdk80_bob", &change_sid)
        .await?
        .ok_or_else(|| anyhow!("bob did not persist the change grandchild"))?;
    let csp_seg = gc
        .ancestors
        .last()
        .ok_or_else(|| anyhow!("the grandchild bundle carries no ancestor child segment"))?;
    assert_eq!(
        cb1.child_state.txid, live_before,
        "bob's `ctesr-` row was never rewritten by the split — this is the premise of the finding"
    );
    assert_ne!(
        csp_seg.state.txid, cb1.child_state.txid,
        "CSP must differ from the state the row still names, or there is no rivalry to report"
    );
    let a = prevout_of(&cb1.child_state.signed_tx)?;
    let b = prevout_of(&csp_seg.state.signed_tx)?;
    assert_eq!(
        a, b,
        "the state bob's row still drives and the CSP every recipient's chain hangs off must spend \
         the SAME outpoint for this to be a theft"
    );
    println!(
        "SDK80 - (3) RIVALS: the row still names {} live while the recipients depend on CSP {}; \
         both spend {a}",
        &cb1.child_state.txid[..12],
        &csp_seg.state.txid[..12]
    );

    // ---- 4. BEHAVIOURAL ADMISSION — the durable status is the ONLY thing that stops the pass. ----
    //
    // `watch_child_pass` is REACTIVE: while `F` is unspent it is verifiably Idle, which is the whole
    // point of a ladder that never ages. So the exit has to be genuinely CONTESTED before "would the
    // tower drive this row?" has an answer at all.
    //
    // The SHARED PREFIX is put on chain by this test, not by the tower — `T -> X_m -> SP ->
    // ext_child` are broadcast from bob's own STORED bundle (no key material, no SE), exactly as a
    // prior owner or a griefer would do it, and exactly the situation sdk79 PART C engineers for the
    // same reason. Two things follow, both needed:
    //
    //  * the ONLY tier left for the tower to act on is `child_state` — the state the recipients'
    //    `CSP` supersedes. That makes it a DISCRIMINATING marker. `X_m` is not: bob's own change
    //    grandchild's chain shares the whole prefix, so a pass legitimately defending the change
    //    puts `X_m` on chain whatever the child row's status is (measured — an earlier revision of
    //    this test asserted on `X_m` and saw it land under both statuses);
    //  * `CSP` is broadcastable at this height and has NOT been broadcast, so the harm is live: if
    //    `child_state` lands, `CSP` can no longer be spent, which is every recipient's coin.
    //
    // The change grandchild's own row is held OUT of the pass throughout (its coin forced WITHDRAWN)
    // so the measurement is about one row. That is not a contrivance: it is precisely the state the
    // window in step 2 leaves behind on the two SINGLE-recipient lanes — `in_ladder_pay` and
    // `child_in_ladder_pay` both convey BEFORE `persist_child`, so during their window the change
    // bundle is not on disk at all and nothing but the stale row is eligible.
    let booked = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk80_bob")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(&bob_child_sid) && c.duplicate_index == 0)
        .map(|c| c.status.clone())
        .ok_or_else(|| anyhow!("bob's child coin vanished"))?;
    println!("SDK80 - (4) the call left bob's child booked as {booked:?}");
    let change_prev = force_status(&cc, "sdk80_bob", &change_sid, CoinStatus::WITHDRAWN).await?;

    let known = |txid: &str| -> bool {
        use electrum_client::bitcoin::Txid;
        use electrum_client::ElectrumApi;
        Txid::from_str(txid)
            .ok()
            .is_some_and(|t| cc.electrum_client.transaction_get(&t).is_ok())
    };
    let push = |hexs: &str| -> Result<bool> {
        use electrum_client::ElectrumApi;
        Ok(cc
            .electrum_client
            .transaction_broadcast_raw(&hex::decode(hexs)?)
            .is_ok())
    };

    let chain = mercuryrustlib::tesr::child_exit_chain(&cb1);
    let last = chain.len() - 1;
    assert_eq!(
        signed_tier_txid(&chain[last].0)?,
        cb1.child_state.txid,
        "the last tier of the child exit chain must be the state the row names live"
    );
    for (i, (hexs, csv)) in chain.iter().enumerate().take(last) {
        let txid = signed_tier_txid(hexs)?;
        for _ in 0..60 {
            if known(&txid) {
                break;
            }
            if push(hexs)? {
                mine_synced(&cc, &core, 1)?;
                break;
            }
            mine_synced(&cc, &core, 4)?;
        }
        assert!(
            known(&txid),
            "the shared prefix tier {i} ({txid}, csv {csv:?}) could not be put on chain — the \
             control cannot be set up"
        );
    }
    // Mature the final rung so the ONLY thing between `child_state` and the chain is L1.
    let rival_txid = cb1.child_state.txid.clone();
    let rival_hex = chain[last].0.clone();
    let csp_txid = csp_seg.state.txid.clone();
    let csp_hex = csp_seg.state.signed_tx.clone();
    mine_synced(&cc, &core, chain[last].1.unwrap_or(0) as u32 + 2)?;
    assert!(!known(&rival_txid), "the rival must still be absent before the A/B");
    assert!(
        !known(&csp_txid),
        "CSP must still be absent — otherwise there is nothing left for the rival to destroy"
    );
    println!(
        "SDK80 - (4) shared prefix on chain; the only tier left is the rival {} (csv {:?}), and the \
         recipients' CSP {} is still un-broadcast",
        &rival_txid[..12],
        chain[last].1,
        &csp_txid[..12]
    );

    // A — as `child_in_ladder_pay_many` left it (WITHDRAWN). The pass must refuse.
    let mut acted_as_booked = false;
    for _ in 0..6 {
        match bob.defend_ladders().await {
            Ok(v) => acted_as_booked |= v.iter().any(|s| s == &bob_child_sid),
            Err(e) => println!("SDK80 - (4/A) pass reported blindness: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let rival_after_a = known(&rival_txid);

    // B — the ONE field changed. Same row, same backend, same chain state.
    let prev = force_status(&cc, "sdk80_bob", &bob_child_sid, CoinStatus::CONFIRMED).await?;
    let mut acted_confirmed = false;
    for _ in 0..6 {
        match bob.defend_ladders().await {
            Ok(v) => acted_confirmed |= v.iter().any(|s| s == &bob_child_sid),
            Err(e) => println!("SDK80 - (4/B) pass reported blindness: {e}"),
        }
        if known(&rival_txid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let rival_after_b = known(&rival_txid);
    force_status(&cc, "sdk80_bob", &bob_child_sid, prev).await?;
    force_status(&cc, "sdk80_bob", &change_sid, change_prev).await?;
    println!(
        "SDK80 - (4) WITHDRAWN -> acted={acted_as_booked}, rival on chain = {rival_after_a}; \
         forced CONFIRMED -> acted={acted_confirmed}, rival on chain = {rival_after_b}"
    );
    assert!(
        !acted_as_booked && !rival_after_a,
        "as booked (WITHDRAWN) the tower must not drive this row"
    );
    assert!(
        acted_confirmed && rival_after_b,
        "with the durable status CONFIRMED the tower DOES drive bob's `ctesr-<child>` row, all the \
         way to the SUPERSEDED state {rival_txid} — from the very same row that refused a moment \
         ago. That is the admission that makes the window in step 2 a theft."
    );

    // …and the harm, measured rather than argued: with the rival on chain the recipients' `CSP` is
    // no longer spendable, so the three conveyed grandchildren (and bob's own change) are void.
    mine_synced(&cc, &core, 1)?;
    let csp_ok = push(&csp_hex).unwrap_or(false);
    println!(
        "SDK80 - (4) HARM: with the rival {} confirmed, re-broadcasting the recipients' CSP {} \
         succeeds = {csp_ok}",
        &rival_txid[..12],
        &csp_txid[..12]
    );
    assert!(
        !csp_ok && !known(&csp_txid),
        "CSP must be UNSPENDABLE once the rival lands — that is what makes an admitted pass a loss \
         of the recipients' coins rather than a harmless duplicate"
    );
    let _ = rival_hex;

    // ---- 5. THE FINDING. ------------------------------------------------------------------------
    assert_eq!(
        admitted, 0,
        "THE D1 WINDOW ON `child_in_ladder_pay_many`: in {admitted} of {post_convey} samples a \
         child bundle had already reached a recipient's coordinator mailbox and the SE had already \
         co-signed the superseding CSP, while bob's wallet DB still read CONFIRMED for child \
         {bob_child_sid}. That is exactly the join `defend_ladders_inner`'s child loop performs \
         (clients/libs/rust-sdk/src/wallet.rs:1875 `live_sids.contains(cid)`), and step 4 shows a \
         CONFIRMED read is sufficient for that loop to broadcast — over the outpoint step 3 shows \
         the recipients' CSP depends on. `child_in_ladder_pay_many` \
         (clients/libs/rust-sdk/src/transfer.rs:930) writes the status LAST, after every \
         `convey_child_bundle`; the same is true of `child_in_ladder_pay` (:820) and of \
         `in_ladder_pay` (:1068, which conveys before `persist_child` and so has no L2 evidence \
         either). A2's durable arm-down was applied to `execute_ex`, `child_retransfer` and \
         `cosign_colored_child_retransfer` — not to these."
    );
    println!("SDK80 - PASS (no window observed)");
    Ok(())
}
