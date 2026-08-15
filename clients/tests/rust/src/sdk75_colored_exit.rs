//! E2E (SDK_E2E=75) — **CTES-R: the first genuine UNILATERAL EXIT of an RGB allocation.**
//!
//! This is the decisive test. Everything CTES-R has built so far is off-chain: sdk74 proves a
//! coloured ladder can be established, renewed against rivals and conveyed, but every tier stays
//! deliberately un-broadcast, so the RGB half is only ever validated through the fork's
//! `OffchainResolver` against the ladder's own txids. That proves coherence, not survival.
//!
//! What this test proves is the product claim: **an RGB allocation can leave the statechain with no
//! cooperation from the SE at all.** Before CTES-R that was impossible in principle, not just
//! unimplemented — `unilateral_exit` refused a carrier outright (CTESR-GATE §4.1), because the only
//! pre-signed spends of a carrier's funding output `F` were RGB-unaware and broadcasting one burned
//! the asset. A carrier holder had an exit for sats they no longer wanted and none for the asset.
//!
//! THE WALK. `T` spends `F` with no timelock; `X_0` spends `T`'s payload output after 12 blocks;
//! `S_0` spends `X_0`'s after 24 (the regtest schedule). Every tier is already co-signed and stored,
//! so the walk needs no key, no server and no counterparty — only blocks. The test drives it through
//! the public `wallet.unilateral_exit()`, mining exactly the `wait_blocks` the SDK reports.
//!
//! THE PROOF THE ALLOCATION SURVIVED, and why each part is the part that can actually fail:
//!
//!  (a) CHAIN. All three tiers CONFIRMED (height > 0, not merely accepted to a mempool); each
//!      spending its parent's DECLARED `payload_vout`, which is the property a colouring off-by-one
//!      would break; each carrying exactly one OP_RETURN, at the declared vout, holding a 32-byte
//!      MPC commitment; and `F` consumed by `T` and by nothing else.
//!
//!  (b) STOCK. A READ-ONLY `color_psbt` dry-run over `(S_0.txid, S_0.payload_vout)` for the full
//!      allocation succeeds. This is the probe CTESR-GATE §3.3 mandates and the ONLY one that
//!      discriminates: E7 measured a fully invalidated stock while `get_asset_balance` and
//!      `list_unspents` both reported `settled = future = spendable = 1000`. The test runs it at
//!      `SUPPLY` (must pass) AND at `SUPPLY + 1` (must fail with "greater than available"), so a
//!      probe that trivially says yes to everything cannot be mistaken for a passing proof.
//!
//!  (c) THE ONE THAT ONLY PASSES IF THE WALK REALLY LANDED. `validate_consignment_offchain_chain`
//!      on the leaf consignment with an **EMPTY** off-chain witness set. With no ids in
//!      `offchain_witness_ids` the fork's `OffchainResolver` falls through to the plain indexer for
//!      EVERY witness, so validity is achievable only if every tier that ever carried the allocation
//!      is genuinely mined. The test asserts this call FAILS before the walk and SUCCEEDS after —
//!      the before-shot is what makes the after-shot evidence rather than decoration.
//!
//! And the exit is a real exit: `S_0`'s payload output pays `bundle.owner_exit_address`, which is
//! the wallet's own seed-derived `backup_address`, and the sats land there on-chain.
//!
//! NEGATIVE CONTROL, in the same wallet: a PLAIN (uncoloured) carrier must still be REFUSED. The
//! guard that opened is "a carrier whose ladder is coloured", not "a carrier".
//!
//! Run: SDK_E2E=75 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

const SUPPLY: u64 = 1_000;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

fn parse(hex_tx: &str) -> Result<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::consensus::deserialize;
    Ok(deserialize(&hex::decode(hex_tx)?)?)
}

/// The transaction as the CHAIN has it, or `None` if the backend has never heard of it.
fn onchain(cc: &ClientConfig, txid: &str) -> Option<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::Txid;
    use electrum_client::ElectrumApi;
    let t = Txid::from_str(txid).ok()?;
    cc.electrum_client.transaction_get(&t).ok()
}

/// Confirmation height of `txid`, looked up through the scriptPubKey of one of its own outputs.
/// `Some(h)` with `h > 0` means MINED; `Some(0)` means mempool-only; `None` means unknown.
fn confirmed_height(cc: &ClientConfig, tx: &electrum_client::bitcoin::Transaction) -> Option<i32> {
    use electrum_client::ElectrumApi;
    let txid = tx.txid();
    // Any non-OP_RETURN output will do — the indexer keys history by scriptPubKey.
    let spk = &tx.output.iter().find(|o| !o.script_pubkey.is_op_return())?.script_pubkey;
    cc.electrum_client
        .script_get_history(spk)
        .ok()?
        .into_iter()
        .find(|h| h.tx_hash == txid)
        .map(|h| h.height)
}

pub fn exit_status(statechain_id: &str, complete: bool) -> mercury_utexo_sdk::ExitStatus {
    mercury_utexo_sdk::ExitStatus {
        statechain_id: statechain_id.to_string(),
        complete,
        wait_blocks: 0,
    }
}

fn tip(cc: &ClientConfig) -> Result<usize> {
    use electrum_client::ElectrumApi;
    Ok(cc.electrum_client.block_headers_subscribe()?.height)
}

/// Mine `n` blocks and do not return until the INDEXER has caught up with each one.
///
/// Not cosmetic patience. rgb-lib's electrum witness resolver derives a mined witness's height as
/// `block_headers_subscribe().height - confirmations` and then asks for the merkle proof at that
/// height ± 2 (`rgb-ops … /indexers/electrum_blocking.rs:168-178`). The tip comes from electrs and
/// the confirmation count comes from bitcoind's verbose passthrough — so while electrs is even two
/// blocks behind, the two disagree, the merkle lookup misses, and a perfectly well-mined
/// transaction is reported as "can't be located in the blockchain". Mining a burst and asking
/// immediately walks straight into it. (Observed here on a 3-block burst; recorded as a robustness
/// finding against the resolver, which is why the proof below ALSO retries.)
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

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::ElectrumApi;
    let tx = onchain(cc, txid).ok_or_else(|| anyhow!("{txid} is not on chain"))?;
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk)?;
    Ok(!listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk75_alice");
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;

    let mut alice_cfg = SdkConfig::regtest("sdk75_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk75_alice".to_string());
    alice_cfg.colored_ladder = true;
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;

    // ---- 1. A carrier with a COLOURED ladder. ---------------------------------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let rgb_fund_addr = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund_addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset_id = alice.issue_token("CTXR", "Coloured Exit Token", 0, SUPPLY).await?;
    bitcoin_core::generatetoaddress(3, &core)?;

    let mut waited = 0;
    loop {
        alice.claim().await?;
        if !alice.get_token_balances().await?.is_empty() {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("the RGB carrier never confirmed"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    // The coloured lane refuses a carrier whose allocation is not yet BOOKED, so keep claiming
    // until the ladder appears.
    let mut carrier_sid = String::new();
    for _ in 0..12 {
        alice.claim().await?;
        let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk75_alice").await?.coins;
        let mut found = None;
        for c in coins.iter().filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0) {
            let Some(sid) = c.statechain_id.clone() else { continue };
            if let Some(b) = mercuryrustlib::tesr::load(&cc, "sdk75_alice", &sid).await? {
                if b.is_colored() {
                    found = Some(sid);
                    break;
                }
            }
        }
        if let Some(sid) = found {
            carrier_sid = sid;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if carrier_sid.is_empty() {
        return Err(anyhow!(
            "no carrier got a COLOURED ladder — CTES-R establish did not happen, so there is \
             nothing to exit"
        ));
    }
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk75_alice", &carrier_sid)
        .await?
        .ok_or(anyhow!("the coloured ladder vanished"))?;
    let rgb_half = bundle.rgb.clone().ok_or(anyhow!("not a coloured ladder"))?;
    assert_eq!(rgb_half.contract_id, asset_id);
    assert_eq!(rgb_half.amount, SUPPLY);
    let exit_addr = bundle.owner_exit_address.clone();
    let exit_value = bundle.current().state.out_value;
    let f_txid = bundle.f_txid.clone();
    let f_vout = bundle.f_vout;
    println!(
        "SDK75 - carrier {carrier_sid} ({} sat) holds {SUPPLY} of {asset_id} and a COLOURED ladder: \
         T {} -> X_0 {} (csv {:?}) -> S_0 {} (csv {:?}), all un-broadcast",
        bundle.f_value,
        &bundle.trigger.txid[..12],
        &bundle.current().extension.txid[..12],
        bundle.current().extension.csv,
        &bundle.current().state.txid[..12],
        bundle.current().state.csv
    );

    // ---- 2. BEFORE the walk: the three probes, so the after-shots are evidence. ------------------
    for tier in bundle.exit_tiers() {
        assert!(onchain(&cc, &tier.txid).is_none(), "tier {} must be un-broadcast", tier.txid);
    }
    // Off-chain coherence: the ladder validates against its OWN un-broadcast txids.
    let (_, off_amt, _, _) = alice.colored_ladder_health(&carrier_sid).await?;
    assert_eq!(off_amt, SUPPLY, "the off-chain ladder assigns the full supply to S_0");
    // The stock is alive, and the probe discriminates (SUPPLY ok, SUPPLY+1 refused).
    alice
        .probe_colored_tip(&carrier_sid, SUPPLY)
        .await
        .map_err(|e| anyhow!("the stock probe failed BEFORE the walk, so nothing is provable: {e}"))?;
    let over = alice.probe_colored_tip(&carrier_sid, SUPPLY + 1).await;
    assert!(
        over.is_err(),
        "the stock probe accepted MORE than the allocation — it is not discriminating and its \
         success proves nothing"
    );
    // (c) the decisive call, and it must FAIL now.
    let before = alice.colored_exit_proof(&carrier_sid).await;
    assert!(
        before.is_err(),
        "the leaf consignment validated against the CHAIN ALONE before any tier was broadcast — \
         the empty-offchain-set proof is vacuous and cannot be evidence of an exit"
    );
    println!(
        "SDK75 - before the walk: off-chain valid, stock alive (probe refuses {} > {SUPPLY}), and \
         the empty-offchain-set proof correctly FAILS",
        SUPPLY + 1
    );

    // ---- 3. THE UNILATERAL WALK — no SE, no counterparty, only blocks. --------------------------
    let mut statuses = alice.unilateral_exit(Some(vec![carrier_sid.clone()]), None).await.map_err(
        |e| anyhow!("unilateral_exit REFUSED a coloured carrier — CTES-R buys nothing: {e}"),
    )?;
    assert!(!statuses[0].complete, "a three-tier exit cannot complete in one pass");
    assert!(
        onchain(&cc, &bundle.trigger.txid).is_some(),
        "the first pass must have broadcast the trigger (it has no relative timelock)"
    );
    // The REST of the walk is driven through `tesr::exit_pass` directly, and its signature is the
    // point: `exit_pass(&electrum_client, &bundle)` takes a CHAIN BACKEND and the stored bundle, and
    // nothing else. There is no `ClientConfig`, no coordinator URL and no key material in scope, so
    // "with no cooperation from the SE" is a structural fact about this call, not a claim about what
    // some code path happened not to do. The SE co-signed these three tiers once, at establish, and
    // has been irrelevant since.
    let mut passes = 1;
    loop {
        let next = mercuryrustlib::tesr::next_exit_tier(&cc.electrum_client, &bundle)?;
        let Some(csv) = next else { break };
        passes += 1;
        assert!(passes < 12, "the coloured exit did not converge");
        let wait = u32::from(csv).max(1);
        println!(
            "SDK75 - tier on chain; the next matures in {wait} blocks — mining them, then advancing \
             the ladder through tesr::exit_pass(electrum, bundle) alone"
        );
        bitcoin_core::generatetoaddress(wait, &core)?;
        mine_synced(&cc, &core, 1)?;
        let progress = mercuryrustlib::tesr::exit_pass(&cc.electrum_client, &bundle)?;
        assert!(
            progress.stalled.is_none() || !progress.broadcast.is_empty(),
            "the keyless exit pass made no progress: {progress:?}"
        );
        statuses = vec![crate::sdk75_colored_exit::exit_status(&carrier_sid, progress.complete)];
        if progress.complete {
            break;
        }
    }
    assert!(statuses[0].complete, "the coloured exit never completed");
    mine_synced(&cc, &core, 3)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    // And the SDK agrees the exit is finished — idempotent, and still not refusing the carrier.
    let again = alice.unilateral_exit(Some(vec![carrier_sid.clone()]), None).await?;
    assert!(again[0].complete && again[0].wait_blocks == 0, "a completed coloured exit stays complete");

    // ---- 4. (a) CHAIN: all three tiers MINED, chained through the declared payload vouts. --------
    let tiers = bundle.exit_tiers();
    let mut prev: Option<(electrum_client::bitcoin::Txid, u32)> = None;
    for (i, tier) in tiers.iter().enumerate() {
        let tx = onchain(&cc, &tier.txid)
            .ok_or_else(|| anyhow!("tier {i} ({}) never reached the chain", tier.txid))?;
        let height = confirmed_height(&cc, &tx)
            .ok_or_else(|| anyhow!("tier {i} has no indexer history"))?;
        assert!(height > 0, "tier {i} is only in a mempool (height {height}), not mined");
        // Byte-identical to what was co-signed and stored: the chain accepted OUR tier, not a
        // malleated cousin.
        let stored = parse(&tier.signed_tx)?;
        assert_eq!(tx.txid(), stored.txid(), "tier {i} on chain differs from the stored tier");
        // Exactly one OP_RETURN, at the vout the builder returned, holding a 32-byte commitment.
        let oprets: Vec<usize> = tx
            .output
            .iter()
            .enumerate()
            .filter(|(_, o)| o.script_pubkey.is_op_return())
            .map(|(v, _)| v)
            .collect();
        assert_eq!(oprets, vec![0], "mined tier {i} must carry exactly one OP_RETURN, at vout 0");
        let opret = tx.output[0].script_pubkey.as_bytes();
        assert_eq!(
            opret.len(),
            34,
            "mined tier {i}'s opret must be OP_RETURN + a 32-byte push (found {} bytes)",
            opret.len()
        );
        assert_eq!(&opret[..2], &[0x6a, 0x20], "mined tier {i}'s opret is not a 32-byte push");
        assert_ne!(tier.payload_vout, 0, "a coloured tier's payload cannot sit at the opret");
        // Chaining, on the MINED transactions: each tier spends its parent's DECLARED payload vout.
        match prev {
            None => assert_eq!(
                (tx.input[0].previous_output.txid.to_string(), tx.input[0].previous_output.vout),
                (f_txid.clone(), f_vout),
                "the trigger must spend F"
            ),
            Some((ptxid, pvout)) => {
                assert_eq!(tx.input[0].previous_output.txid, ptxid, "tier {i} parent txid");
                assert_eq!(
                    tx.input[0].previous_output.vout, pvout,
                    "tier {i} must spend its parent's DECLARED payload vout, not the opret"
                );
            }
        }
        prev = Some((tx.txid(), tier.payload_vout));
    }
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout)?, "F must be consumed by the ladder exit");
    // The sats really are the owner's, at his own seed-derived key.
    let state_tx = onchain(&cc, &bundle.current().state.txid).unwrap();
    let payee_spk = &state_tx.output[bundle.current().state.payload_vout as usize].script_pubkey;
    let plain = mercurylib::tesr::payee_address(&exit_addr, &bundle.network)
        .map_err(|e| anyhow!("could not resolve the owner's exit address: {e:?}"))?;
    let expected_spk = electrum_client::bitcoin::Address::from_str(&plain)?
        .assume_checked()
        .script_pubkey();
    assert_eq!(
        payee_spk, &expected_spk,
        "the final state must pay the wallet's OWN backup_address"
    );
    assert_eq!(
        state_tx.output[bundle.current().state.payload_vout as usize].value,
        exit_value,
        "the exited value is the ladder's declared final state value"
    );
    // …and that address really is the wallet's own seed-derived key, not just a string the bundle
    // agrees with itself about.
    let carrier_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk75_alice")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(carrier_sid.as_str()))
        .ok_or(anyhow!("the carrier coin vanished from the wallet record"))?;
    assert_eq!(
        exit_addr, carrier_coin.backup_address,
        "the coloured ladder must exit to the wallet's own backup_address"
    );
    println!(
        "SDK75 - (a) chain: T, X_0, S_0 all MINED, each spending its parent's declared payload \
         vout, each with a 32-byte opret at vout 0; F spent; {exit_value} sat at the owner's own \
         key {exit_addr}"
    );

    // ---- 5. (c) THE DECISIVE PROOF: valid against the CHAIN ALONE (empty off-chain set). ---------
    // Retried only against the INDEXER-LAG failure documented on `mine_synced`: rgb-lib's resolver
    // reports a well-mined tx as "can't be located in the blockchain" while electrs trails
    // bitcoind. A genuine "the allocation did not survive" answer is a validation FAILURE, which is
    // stable and is NOT retried away — the loop below re-raises whatever the last error was.
    let mut proof = alice.colored_exit_proof(&carrier_sid).await;
    for _ in 0..20 {
        if proof.is_ok() {
            break;
        }
        let msg = proof.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        if !msg.contains("can't be located in the blockchain") {
            break; // a real verdict, not indexer lag
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        proof = alice.colored_exit_proof(&carrier_sid).await;
    }
    let (contract, assigned, detail) = proof.map_err(|e| {
        anyhow!(
            "THE ALLOCATION DID NOT SURVIVE THE WALK: the leaf consignment does not validate \
             against the chain alone after every tier was mined — {e}"
        )
    })?;
    assert_eq!(contract, asset_id, "the surviving allocation is THIS contract");
    assert_eq!(
        assigned, SUPPLY,
        "the full allocation must be assigned to the final state's payload output"
    );
    println!(
        "SDK75 - (c) the leaf consignment validates with an EMPTY off-chain witness set — every \
         tier resolved through the PLAIN indexer, i.e. genuinely mined — and assigns {assigned} \
         {contract} to S_0's payload output (detail: {detail:?})"
    );

    // ---- 6. (b) THE STOCK PROBE, after the walk, and still discriminating. -----------------------
    alice.probe_colored_tip(&carrier_sid, SUPPLY).await.map_err(|e| {
        anyhow!("the stock is DEAD after the exit walk — the allocation did not survive: {e}")
    })?;
    let over = alice.probe_colored_tip(&carrier_sid, SUPPLY + 1).await;
    let over_err = over.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        !over_err.is_empty(),
        "after the walk the probe accepted MORE than the allocation — it is not reading the stock"
    );
    println!(
        "SDK75 - (b) read-only color_psbt stock probe over S_0's payload output: {SUPPLY} \
         SPENDABLE, {} refused ({}) — the stash still binds the allocation to the owner's outpoint",
        SUPPLY + 1,
        over_err.lines().next().unwrap_or("").trim()
    );

    // ---- 6b. THE ENGINE'S UTXO VIEW MOVED TO THE EXIT TIP. --------------------------------------
    //
    // This block used to be a printed KNOWN GAP. The exited allocation sits at `owner_exit_address`,
    // a MERCURY seed-derived key that is not in the RGB engine's BDK descriptor, so no wallet sync
    // can ever discover it — which meant that after a coloured exit `list_allocations` /
    // `get_asset_balance` were not merely incomplete but STALE: they reported the asset at the
    // funding outpoint that the exit had just SPENT.
    //
    // `unilateral_exit` now registers the tip with `register_statechain_utxo` the moment the walk
    // completes, passing the pre-exit outpoint as the spend, so both ends move together. Asserted in
    // BOTH directions, because a view that gains the tip while keeping the stale entry double-counts
    // the asset — which is a worse answer than the gap was.
    //
    // NOTE ON E7: this is a statement about rgb-lib's UTXO BOOKKEEPING, not about whether the asset
    // survived. The survival evidence is still, and only, the stock probe and the empty-witness-set
    // validation above. A balance reading is never used as evidence here in either direction.
    let allocs = alice.list_token_allocations(&asset_id).await.unwrap_or_default();
    let bal = alice
        .get_token_balances()
        .await?
        .into_iter()
        .find(|b| b.asset_id == asset_id)
        .map(|b| b.balance);
    let f_outpoint = format!("{f_txid}:{f_vout}");
    let tip_outpoint =
        format!("{}:{}", bundle.current().state.txid, bundle.current().state.payload_vout);
    let stale = allocs.iter().any(|(op, _)| *op == f_outpoint);
    let at_tip = allocs.iter().any(|(op, _)| *op == tip_outpoint);
    assert!(
        at_tip,
        "after a completed coloured exit the allocation must be registered at the exit tip \
         {tip_outpoint}; rgb-lib reports {allocs:?}"
    );
    assert!(
        !stale,
        "the SPENT funding outpoint {f_outpoint} must not still hold the allocation — the exit tip \
         was registered but the pre-exit outpoint was not marked spent, so the asset is now \
         double-counted: {allocs:?}"
    );
    assert_eq!(
        bal,
        Some(SUPPLY),
        "the engine's balance must be the whole supply once, at the tip"
    );
    println!(
        "SDK75 - (d) the exit TIP is registered with the engine: rgb-lib now reports {allocs:?} \
         (balance {bal:?}) — the allocation is at the exit tip {tip_outpoint} and NOT at the spent \
         funding outpoint {f_outpoint}. The tip pays a Mercury seed-derived key absent from the \
         engine's descriptor, so no sync could have found it; `register_statechain_utxo` at exit \
         completion is what moved the view."
    );

    // ---- 7. NEGATIVE CONTROL: a PLAIN carrier is still refused. ----------------------------------
    //
    // The guard that opened is "a carrier whose ladder is COLOURED", not "a carrier". A wallet with
    // the coloured lane OFF produces exactly the old shape, and exiting it would burn the asset.
    let _ = std::fs::remove_dir_all("./rgb-data-sdk75_bob");
    // RE-DERIVED, and pinned in the direction the code actually has. bob opts out EXPLICITLY rather
    // than inheriting: the negative control is about a carrier with NO coloured ladder, and how that
    // wallet came to be configured that way was never the point — stating it means the control keeps
    // working whichever way the default later moves.
    //
    // The pin below is the same pin, aimed at the truth: `colored_ladder` ships **false** (2c351c6).
    // Note what that does NOT say about this test. Everything above this line is alice, running with
    // the lane explicitly ON, carrying an RGB allocation through a full unilateral exit — the lane
    // is SOUND, and that is proved, not defaulted. What keeps it opt-in is the measured economics of
    // what it switches on (`docs/utexo/current/PARTIAL-PAYMENT-ECONOMICS.md`), which is a product decision
    // and is exactly what this pin stops from moving silently.
    assert!(
        !SdkConfig::regtest("default-probe").colored_ladder,
        "colored_ladder must ship OFF by default (2c351c6) — this test proves the lane WORKS \
         (alice exits an RGB allocation unilaterally on it), but its economics keep it opt-in. \
         Flipping the default is a product decision and must not happen silently."
    );
    let mut bob_cfg = SdkConfig::regtest("sdk75_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk75_bob".to_string());
    bob_cfg.colored_ladder = false;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let t = prepaid_token(&cc).await?;
    bob.add_prepaid_token(&t).await;
    let bob_fund = bob.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &bob_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let t = prepaid_token(&cc).await?;
    bob.add_prepaid_token(&t).await;
    let _ = bob.issue_token("PLNC", "Plain Carrier", 0, SUPPLY).await?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut bob_carrier = String::new();
    for _ in 0..40 {
        bob.claim().await?;
        if !bob.get_token_balances().await?.is_empty() {
            let coins =
                mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk75_bob").await?.coins;
            if let Some(sid) = coins
                .iter()
                .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
                .find_map(|c| c.statechain_id.clone())
            {
                bob_carrier = sid;
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(!bob_carrier.is_empty(), "bob's plain carrier never confirmed");
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk75_bob", &bob_carrier)
            .await?
            .map_or(true, |b| !b.is_colored()),
        "bob's carrier must NOT be coloured (his wallet has the lane explicitly OFF)"
    );
    let refused = bob.unilateral_exit(Some(vec![bob_carrier.clone()]), None).await;
    let msg = refused.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        msg.contains("RGB allocation"),
        "a carrier with NO coloured ladder must still be refused a unilateral exit, got: {msg:?}"
    );
    // And it is excluded from the exit-everything default rather than silently swept.
    let all = bob.unilateral_exit(None, None).await?;
    assert!(
        !all.iter().any(|s| s.statechain_id == bob_carrier),
        "a plain carrier must stay out of the exit-everything default"
    );
    println!("SDK75 - negative control: an UNCOLOURED carrier is still refused ({msg})");

    println!(
        "SDK75 - PASS: an RGB allocation was carried through a full UNILATERAL exit with no SE \
         cooperation — T, X_0 and S_0 mined in sequence through their declared payload vouts, the \
         leaf consignment valid against the CHAIN ALONE (empty off-chain set), and the read-only \
         stock probe still spending {SUPPLY} of {asset_id} out of the owner's own exit outpoint. \
         Uncoloured carriers stay refused."
    );
    Ok(())
}
