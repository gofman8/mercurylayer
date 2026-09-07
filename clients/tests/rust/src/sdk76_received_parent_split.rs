//! E2E (SDK_E2E=76) — **splitting a RECEIVED laddered coin: the child is adoptable with an EMPTY
//! parent chain.**
//!
//! A laddered coin carries NO flat backup — not at deposit and not at any hop. Its only exit
//! material is the ladder `T -> X_0 -> S_0`, co-signed at first sight of the funding transaction,
//! and every whole-coin hop co-signs exactly ONE receiver-paying state `S'` and demotes the state it
//! replaces into `superseded_states`. So the receiver's census over a parent segment is exactly
//! `num_sigs(parent) == tiers + superseded`, with a flat term of ZERO (`PARENT_V2_BASELINE == 0`)
//! whatever the coin's history, and a conveyed `ChildTesrBundle::parent_flat_backups` vector must
//! be EMPTY (`refuse_conveyed_flat_backups`).
//!
//! This file used to guard the OPPOSITE shape: a deposit co-signed one flat `tx1` and each hop one
//! more, so a RECEIVED parent carried `1 + k` flat backups and a child of it was adoptable only if
//! the sender conveyed that whole chain and the receiver counted it. sdk58, sdk59 and sdk69 all
//! DEPOSIT the parent they split, so this is still the one test that puts a hop in front of the
//! split — and what it measures is now the empty-chain rule, at every step where the old shape
//! would have shown up:
//!
//!   1. alice deposits and `claim()` ladders the coin: `num_sigs == 3` (no flat `tx1`), ZERO
//!      backup rows, `locktime == None`, and `verify_bundle(.., 3, 0)` accepts it;
//!   2. alice transfers the WHOLE coin to bob; bob claims it. His coin is a RECEIVED laddered coin
//!      at `num_sigs == 4` = 3 tiers + 1 superseded (`S_0`), STILL with zero backup rows and no
//!      locktime — and the same bundle censused with a flat term of 1 is REJECTED;
//!   3. bob pays carol a NON-EXACT amount, which routes through the in-ladder split;
//!   4. **carol claims and ADOPTS the child** with `parent_flat_backups.is_empty()`, and the
//!      parent census she balanced is exactly `exit_tiers + superseded`;
//!   5. the controls: the REAL receiver path (`verify_conveyed_child`) accepts the adopted bundle;
//!      the same bundle censused with a flat term of 1 is rejected with the census's own
//!      "num_sigs mismatch"; and a copy that CONVEYS one flat backup beside its ladder — a prior
//!      owner's retained spend of `F`, the exact thing the rule removes — is refused BY NAME;
//!   6. carol exits the child unilaterally and the sats land at her own key.
//!
//! Plus [S7]: carol's adopted child is in her exported watch bundle as an EVENT entry — a trigger on
//! the parent's `F`, `deadline_block == u32::MAX` (a leaf has no calendar: no ancestor holds a
//! matured spend of `F`), a head start over the BOUND chain, and no absolute-locktime sweep.
//!
//! Run: SDK_E2E=76 ML_NETWORK=regtest cargo +stable run

use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const DEPOSIT: u64 = 100_000;
/// Non-exact w.r.t. every coin bob holds, so `transfer()` must split rather than hand over a whole
/// coin — that is the path this test exists to reach.
const PAY: u64 = 30_000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn ladder_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain info for {sid}"))?
        .num_sigs)
}

async fn aggregate(
    cc: &mercuryrustlib::client_config::ClientConfig,
    sid: &str,
) -> Result<Option<String>> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain info for {sid}"))?
        .aggregate_pubkey)
}

/// How many flat backup rows the wallet holds for `sid` — ZERO for a laddered coin. An ABSENT row
/// and an empty row are the same answer here; a row that cannot be read is an error, never a zero.
async fn flat_rows(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet_name: &str,
    sid: &str,
) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, sid)
        .await?
        .map_or(0, |rows| rows.len()))
}

/// The `duplicate_index == 0` coin row for `sid`.
async fn coin_of(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet_name: &str,
    sid: &str,
) -> Result<mercurylib::wallet::Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("{wallet_name} holds no coin {sid}"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = ladder_wallet("sdk76_alice").await?;
    let bob = ladder_wallet("sdk76_bob").await?;
    let carol = ladder_wallet("sdk76_carol").await?;
    let bob_address = bob.get_utexo_address().await?;
    let carol_address = carol.get_utexo_address().await?;

    // The constant the census runs on. Pinned here so a drift back to a non-zero baseline fails
    // this test by name rather than through some downstream mismatch.
    assert_eq!(
        mercuryrustlib::tesr::PARENT_V2_BASELINE,
        0,
        "a laddered parent's flat-backup census term is ZERO — no tx1 at deposit, no per-hop backup"
    );

    // ---- 1. alice deposits; claim() establishes the ladder. No flat backup exists anywhere. ------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;

    let mut waited = 0;
    loop {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats == DEPOSIT {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("alice's deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk76_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    let alice_bundle = mercuryrustlib::tesr::load(&cc, "sdk76_alice", &alice_sid)
        .await?
        .ok_or(anyhow!("alice's coin must be laddered — a coin without a ladder has no exit and no lane"))?;
    let alice_ns = num_sigs(&cc, &alice_sid).await?;
    assert_eq!(
        alice_ns, 3,
        "a fresh laddered deposit costs exactly its three tiers T, X_0, S_0 — a fourth co-sign \
         would be the flat tx1 the rule removed (got {alice_ns})"
    );
    assert_eq!(alice_bundle.exit_tiers().len(), 3, "the ladder is T -> X_0 -> S_0");
    assert!(alice_bundle.superseded_states.is_empty(), "nothing has been superseded yet");
    mercuryrustlib::tesr::verify_bundle(&alice_bundle, alice_ns, 0)
        .map_err(|e| anyhow!("alice's deposited ladder failed the census with a flat term of 0: {e}"))?;
    let alice_flat = flat_rows(&cc, "sdk76_alice", &alice_sid).await?;
    assert_eq!(
        alice_flat, 0,
        "a DEPOSITED laddered coin holds ZERO flat backup rows — the ladder was signed at first \
         sight in place of tx1 (got {alice_flat})"
    );
    let alice_coin = coin_of(&cc, "sdk76_alice", &alice_sid).await?;
    assert!(
        alice_coin.locktime.is_none(),
        "a laddered coin carries no absolute calendar: locktime must be None for life"
    );
    println!(
        "SDK76 - alice deposited {DEPOSIT} and laddered it (sid {alice_sid}); num_sigs {alice_ns} = 3 \
         tiers, flat backup rows {alice_flat}, locktime None"
    );

    // ---- 2. THE HOP the other split E2Es never make: the WHOLE coin moves to bob. ----------------
    let r = alice.transfer(&bob_address, DEPOSIT).await?;
    assert!(!r.used_split, "an exact-amount payment must hand over the WHOLE coin, not split it");
    let mut waited = 0;
    let bob_sid = loop {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats == DEPOSIT {
            break mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk76_bob")
                .await?
                .coins
                .iter()
                .find(|c| {
                    c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                        && c.amount == Some(DEPOSIT as u32)
                })
                .and_then(|c| c.statechain_id.clone())
                .ok_or(anyhow!("bob has no confirmed whole coin"))?;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("bob did not receive the whole coin"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let bob_bundle = mercuryrustlib::tesr::load(&cc, "sdk76_bob", &bob_sid)
        .await?
        .ok_or(anyhow!("bob's received coin must still be laddered"))?;

    // THE PREMISE, MEASURED. One whole-coin hop == ONE co-sign (the receiver-paying S'), and the
    // replaced S_0 is disclosed as superseded. No flat backup is minted by the hop: bob's flat rows
    // are still zero and his coin has no locktime. If a hop ever costs two co-signs again, a
    // per-hop backup is back and this fails here, before the split.
    let bob_ns = num_sigs(&cc, &bob_sid).await?;
    assert_eq!(
        bob_ns,
        alice_ns + 1,
        "a whole-coin hop costs exactly ONE co-sign — the receiver-paying S' — never a flat backup \
         beside it (got {bob_ns} after one hop from {alice_ns})"
    );
    assert_eq!(bob_bundle.exit_tiers().len(), 3, "bob's exit is still T -> X_0 -> S'");
    assert_eq!(
        bob_bundle.superseded_states.len(),
        1,
        "the S_0 that S' replaced must be disclosed as superseded — it is the co-sign the census \
         counts for the hop"
    );
    mercuryrustlib::tesr::verify_bundle(&bob_bundle, bob_ns, 0)
        .map_err(|e| anyhow!("bob's RECEIVED ladder failed the census with a flat term of 0: {e}"))?;
    let bob_at_flat_one = mercuryrustlib::tesr::verify_bundle(&bob_bundle, bob_ns, 1)
        .expect_err(
            "bob's ladder censused with a flat term of 1 must be REJECTED: there is no flat \
             backup for that slot to account for, so accepting it would admit a hidden state",
        )
        .to_string();
    assert!(
        bob_at_flat_one.contains("num_sigs mismatch"),
        "the flat-term-1 census must fail on the count itself, got: {bob_at_flat_one}"
    );
    let bob_flat = flat_rows(&cc, "sdk76_bob", &bob_sid).await?;
    assert_eq!(
        bob_flat, 0,
        "a RECEIVED laddered coin holds ZERO flat backup rows after a hop — got {bob_flat}"
    );
    assert!(
        coin_of(&cc, "sdk76_bob", &bob_sid).await?.locktime.is_none(),
        "a received laddered coin carries no absolute calendar: locktime must be None"
    );
    println!(
        "SDK76 - bob RECEIVED the whole coin (sid {bob_sid}); num_sigs {alice_ns} -> {bob_ns} (one \
         co-sign, S_0 superseded), flat backup rows {bob_flat}, locktime None; censused at a flat \
         term of 1 it is rejected ({bob_at_flat_one})"
    );

    // ---- 3. bob splits the RECEIVED coin in-ladder to pay carol. ---------------------------------
    let r = bob.transfer(&carol_address, PAY).await?;
    assert!(
        r.used_split,
        "a {PAY} payment out of a single {DEPOSIT} laddered coin must take the in-ladder split"
    );
    assert_eq!(r.total_sats, PAY, "the payment total is the piece amount");
    println!("SDK76 - bob split his RECEIVED laddered coin in-ladder and paid carol {PAY}");

    // ---- 4. THE PROPERTY: carol ADOPTS the child, with an EMPTY parent chain. --------------------
    let mut waited = 0;
    let carol_child_sid = loop {
        carol.claim().await?;
        let bal = carol.get_balance().await?;
        if bal.available_sats == PAY {
            break mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk76_carol")
                .await?
                .coins
                .iter()
                .find(|c| {
                    c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                        && c.amount == Some(PAY as u32)
                })
                .and_then(|c| c.statechain_id.clone())
                .ok_or(anyhow!("carol has no confirmed child coin"))?;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!(
                "carol could NOT adopt the child of a RECEIVED laddered parent (balance {bal:?})"
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let cb = mercuryrustlib::tesr::load_child(&cc, "sdk76_carol", &carol_child_sid)
        .await?
        .ok_or(anyhow!("carol did not persist the adopted child bundle"))?;
    println!("SDK76 - carol ADOPTED the child (sid {carol_child_sid}, {PAY} sat) — the census balanced");

    // The conveyed bundle carries NO flat backup chain — there is none to carry.
    assert!(
        cb.parent_flat_backups.is_empty(),
        "a child of a laddered parent conveys an EMPTY parent_flat_backups — got {} entries",
        cb.parent_flat_backups.len()
    );
    // …and the parent census carol balanced is exactly tiers + superseded: T, X_0, SP live, with
    // S_0 (superseded by the hop) and S' (superseded by the split) disclosed.
    let p_ns = num_sigs(&cc, &cb.parent_statechain_id).await?;
    assert_eq!(
        p_ns,
        (cb.parent.exit_tiers().len() + cb.parent.superseded_states.len()) as u32,
        "the parent census is `num_sigs == tiers + superseded` with a flat term of ZERO"
    );
    assert_eq!(
        cb.parent.superseded_states.len(),
        2,
        "a received-then-split parent discloses exactly two superseded states: S_0 (replaced by \
         the hop's S') and S' (replaced by the split's SP)"
    );
    assert!(
        coin_of(&cc, "sdk76_carol", &carol_child_sid).await?.locktime.is_none(),
        "an adopted child has no absolute-locktime backup; locktime must be None"
    );

    // ---- 4b. [S7] THE DELEGATED TOWER MUST COVER THIS CHILD, AS AN EVENT. -----------------------
    //
    // A `ctesr-` row is neither the `tesr-` row nor the `branch-` row `export_watch_bundle` looks
    // for, and it once fell through to the `continue` written for a coin nothing can race. The
    // export still returned `Ok`, so a THIRD-PARTY tower silently watched none of a wallet's leaves.
    //
    // What the entry must look like is the second half: a leaf's parent is a laddered coin with NO
    // flat backup, so no ancestor holds a matured spend of `F` and there is no height at which the
    // race is lost on its own. Its race starts on an EVENT — somebody spending `F` — so the entry
    // carries a trigger on `F` and `deadline_block == u32::MAX` (the height predicate permanently
    // false). A REAL height here would be a calendar the coin does not have.
    let bundle: mercury_utexo_sdk::watchtower::WatchBundle =
        serde_json::from_str(&carol.export_watch_bundle().await?)?;
    let leaf = bundle
        .entries
        .iter()
        .find(|e| e.statechain_id == carol_child_sid)
        .ok_or_else(|| {
            anyhow!(
                "[S7] carol's adopted child {carol_child_sid} is ABSENT from her own exported watch \
                 bundle ({} entries) — a delegated watchtower would not be watching it at all, and \
                 the export reported success",
                bundle.entries.len()
            )
        })?;
    let trig = leaf.trigger.as_ref().ok_or_else(|| {
        anyhow!("[S7] a leaf's race starts on an EVENT (an ancestor spending F); the trigger is unarmed")
    })?;
    assert_eq!(trig.watch_txid, cb.parent.f_txid, "[S7] the watched outpoint is the parent's F");
    assert_eq!(trig.watch_vout, cb.parent.f_vout);
    assert_eq!(
        leaf.deadline_block,
        u32::MAX,
        "[S7] a leaf has NO height deadline: no ancestor holds a matured spend of F, so the height \
         predicate must be permanently false (got {})",
        leaf.deadline_block
    );
    assert!(!trig.push_txs.is_empty(), "[S7] nothing to broadcast is nothing to protect");
    // The head start must be the whole BOUND chain, so the tower starts the walk early enough to
    // finish it — the same call the in-process `defend_ladders` child pass makes.
    let bound = mercuryrustlib::tesr::child_exit_chain_bound(&cb)?;
    let csvs: Vec<Option<u16>> = bound.iter().map(|(_, c)| *c).collect();
    assert_eq!(
        trig.csv_blocks,
        mercurylib::transfer::receiver::exit_wait_blocks(&csvs),
        "[S7] the head start must be exit_wait_blocks over the BOUND chain"
    );
    assert!(
        leaf.backup_tx.is_none(),
        "[S7] a leaf has no absolute-locktime sweep; its exit IS the chain"
    );
    println!(
        "SDK76 - [S7] carol's child IS in her watch bundle as an EVENT entry: deadline u32::MAX, head \
         start {} over {} bound tiers, trigger {}:{}",
        trig.csv_blocks,
        bound.len(),
        trig.watch_txid,
        trig.watch_vout
    );

    // ---- 5. THE CONTROLS. --------------------------------------------------------------------
    let f_txid = electrum_client::bitcoin::Txid::from_str(&cb.parent.f_txid)
        .map_err(|_| anyhow!("bad parent f_txid"))?;
    let f_tx = cc.electrum_client.transaction_get(&f_txid).map_err(|_| anyhow!("F not on chain"))?;
    let f_spk_hex =
        hex::encode(f_tx.output[cb.parent.f_vout as usize].script_pubkey.as_bytes());
    // …and its VALUE, from the same fetched transaction — the anchor the parent's trigger is bound to.
    let f_value_onchain = f_tx.output[cb.parent.f_vout as usize].value;
    let p_agg = aggregate(&cc, &cb.parent_statechain_id).await?;
    let c_ns = num_sigs(&cc, &cb.child_statechain_id).await?;
    let c_agg = aggregate(&cc, &cb.child_statechain_id).await?;
    let (_, _, p_term) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &cb.parent_statechain_id).await?;
    let carol_backup_addr = {
        let coin = coin_of(&cc, "sdk76_carol", &carol_child_sid).await?;
        mercurylib::transaction::get_user_backup_address(&coin, "regtest".to_string())?
    };

    // 5a. Positive control, the REAL receiver path: the very call carol's claim made.
    let admitted_value = mercuryrustlib::tesr::verify_conveyed_child(&cc, &carol_backup_addr, &cb)
        .await
        .map_err(|e| anyhow!("the real receiver path REJECTED the adopted child of a RECEIVED parent: {e}"))?;
    assert_eq!(
        admitted_value, cb.child_state.out_value,
        "the receiver's census-bound exit value is the child state's committed value"
    );
    // 5b. Positive control, the pure verifier at the ZERO flat term.
    mercuryrustlib::tesr::verify_child_bundle(
        &cb,
        &f_spk_hex,
        f_value_onchain,
        p_ns,
        mercuryrustlib::tesr::PARENT_V2_BASELINE,
        p_agg.as_deref(),
        p_term,
        c_ns,
        mercuryrustlib::tesr::CHILD_V2_BASELINE,
        c_agg.as_deref(),
        &[],
        &carol_backup_addr,
    )
    .map_err(|e| anyhow!("the child of a RECEIVED parent was REJECTED at the zero flat term: {e}"))?;

    // 5c. Negative control: the same bundle censused with a flat term of 1 — the old deposit tx1
    // — must be REJECTED by the PARENT census. Without this the test would still pass if
    // `verify_child_bundle` stopped censusing the ancestor segment at all.
    let err = mercuryrustlib::tesr::verify_child_bundle(
        &cb,
        &f_spk_hex,
        f_value_onchain,
        p_ns,
        1,
        p_agg.as_deref(),
        p_term,
        c_ns,
        mercuryrustlib::tesr::CHILD_V2_BASELINE,
        c_agg.as_deref(),
        &[],
        &carol_backup_addr,
    )
    .expect_err(
        "SECURITY/REGRESSION: censusing the ancestor segment with a flat term of 1 must NOT accept \
         a child of a laddered parent — no flat backup exists for that slot to account for",
    )
    .to_string();
    assert!(
        err.contains("parent segment/census invalid: num_sigs mismatch"),
        "the flat-term-1 census must fail on the PARENT census specifically, got: {err}"
    );
    println!("SDK76 - negative control: the same bundle censused at a flat term of 1 is REJECTED ({err})");

    // 5d. Negative control: a bundle that CONVEYS a flat backup beside its ladder. The entry is a
    // prior owner's retained spend of `F` — here, the parent's own trigger, the most realistic
    // thing a sender could keep — and it must be refused BY NAME by the real receiver path, before
    // any census is run: a co-sign the census cannot account for, and a spend of the funding
    // output in someone else's hands.
    let mut forged = cb.clone();
    forged.parent_flat_backups.push(mercurylib::wallet::BackupTx {
        tx_n: 1,
        tx: cb.parent.trigger.signed_tx.clone(),
        client_public_nonce: String::new(),
        server_public_nonce: String::new(),
        client_public_key: String::new(),
        server_public_key: String::new(),
        blinding_factor: String::new(),
        rgb_consignment: None,
        rgb_blinding: None,
    });
    let err = mercuryrustlib::tesr::verify_conveyed_child(&cc, &carol_backup_addr, &forged)
        .await
        .expect_err(
            "SECURITY: a child bundle conveying a flat backup beside its ladder was ACCEPTED — a \
             prior owner's retained spend of F would ride along with every adopted child",
        )
        .to_string();
    assert!(
        err.contains("flat backup transaction(s) beside its ladder"),
        "a conveyed flat backup must be refused BY NAME (refuse_conveyed_flat_backups), got: {err}"
    );
    println!("SDK76 - negative control: a conveyed flat backup beside the ladder is REFUSED by name ({err})");

    // ---- 6. carol exits the child unilaterally; the sats land at her own key. --------------------
    let mut passes = 0;
    loop {
        let st = carol.unilateral_exit(Some(vec![carol_child_sid.clone()]), None).await?;
        let s = st.into_iter().next().ok_or(anyhow!("no exit status"))?;
        if s.complete {
            break;
        }
        bitcoin_core::generatetoaddress(s.wait_blocks.max(1) + 1, &core)?;
        passes += 1;
        if passes > 40 {
            return Err(anyhow!("carol's child exit did not complete"));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    bitcoin_core::generatetoaddress(1, &core)?;
    let child_value = cb.child_state.out_value;
    assert!(
        crate::sdk40_tesr_consensus::wait_for_address(&cc, &carol_backup_addr, child_value as u32)
            .await
            .is_ok(),
        "the child's {child_value} sat must land at carol's own key"
    );

    println!(
        "SDK76 - ✓ PASS: a RECEIVED (transferred-once) laddered coin was split IN-LADDER and its \
         child was ADOPTED by the receiver and EXITED for {child_value} sat. No flat backup existed \
         at any step (deposit num_sigs 3, hop +1, zero backup rows, locktime None throughout); the \
         child conveyed an EMPTY parent chain; the ancestor census ran on `tiers + superseded` \
         with a flat term of 0 — a flat term of 1 is rejected, and a conveyed flat backup is \
         refused by name."
    );
    Ok(())
}

use std::str::FromStr;
