//! E2E (SDK_E2E=74) — **CTES-R: colour the ladder at establish**.
//!
//! The blocker CTES-R exists to remove is TERMINAL FREEZE: an RGB allocation is bound to a UTXO
//! (its seal), `T` spends the funding UTXO `F` directly, and an UNCOLOURED trigger therefore
//! destroys the allocation on first exit. So today `claim()` refuses to ladder a carrier at all and
//! `unilateral_exit` refuses a carrier outright — RGB has no unilateral exit.
//!
//! This test proves the fix at ESTABLISH: with `SdkConfig::colored_ladder` on, `claim()` builds a
//! ladder whose every tier carries a valid RGB state transition, so laddering MOVES the allocation
//! instead of destroying it.
//!
//! What is asserted, and why each one is the assertion that can actually fail:
//!
//!   1. the carrier's ladder EXISTS and is COLOURED (`TesrBundle::rgb`), while the plain deposit in
//!      the SAME wallet is laddered exactly as before — `rgb == None`, payload at vout 0, no
//!      OP_RETURN anywhere. The plain path must be byte-identical;
//!   2. every coloured tier carries exactly ONE OP_RETURN, at vout 0, with the PAYLOAD at vout 1 and
//!      the P2A anchor at vout 2 — and the bundle's declared `payload_vout` is 1, threaded from the
//!      builder's returned index, never assumed;
//!   3. each tier chains through its parent's DECLARED payload vout (`T:1 -> X`, `X:1 -> S`), which
//!      is the property an off-by-one would break;
//!   4. the committed fee of each coloured tier is exactly `committed_fee_for_outputs(n + 1, rate)`
//!      = 336 sat at 2 sat/vB, i.e. 86 sat more than the uncoloured 250 — the constant `43 vB * rate`
//!      surcharge for the opret (CTESR-GATE §3.4). Underpaying it breaks the self-funding property
//!      that lets a pre-signed tier relay standalone;
//!   5. the LEAF consignment validates against the ladder's own UN-BROADCAST txids through the
//!      fork's `OffchainResolver`, and assigns the FULL allocation to the final state's payload
//!      output. This is the stock-level probe CTESR-GATE §3.3 mandates: `get_asset_balance` is
//!      BLIND to a dead stash (E7 measured `settled=future=spendable=1000` with the stock at zero),
//!      so it is checked as well but is never the evidence;
//!   6. the allocation is INTACT: the carrier outpoint still holds the full supply and is still
//!      quarantined from plain-BTC selection. Colouring the ladder must not make `F` look spent;
//!   7. the CENSUS balances at the value the plain path uses: `num_sigs == flat_backups(1) +
//!      tiers(3)`, verified by the same bound verifier a receiver runs. Colouring adds ZERO SE
//!      co-signs — the SE stays blind;
//!   8. NO PLAIN-SPLIT PATH CAN REACH THE CARRIER: its sats are quarantined from plain-BTC
//!      selection (so no plain split builder can even select it) and the uncoloured in-ladder split
//!      refuses this bundle by name. This replaces the old "the legacy colored split refuses" probe,
//!      which is no longer falsifiable at the E2E level because the legacy lane is now RETIRED and
//!      unreachable from a coloured carrier — the gate is asserted over the source instead, by
//!      `tokens::retired_split_lane_census`;
//!   9. RENEWAL and TRANSFER — the RIVAL-TIER case, which is where a coloured ladder is actually
//!      hard. Every renewal replaces `X_m` over the trigger's payload output and every transfer
//!      replaces `S_k` over the extension's, so rivals over ONE outpoint are the NORMAL case. Under
//!      a shared blinding they collapse to one `OpId`/`BundleId` and rgb-lib keeps whichever
//!      witness has the smallest INTERNAL (little-endian) txid — a hash lottery. So the test renews
//!      until there are **>= 3 rivals** AND the live extension is deliberately **not** the
//!      internal-txid minimum (a 2-rival test passes half the time by luck; a 3-rival test still
//!      passes if the lottery happens to pick right), then asserts the leaf consignment embeds the
//!      LIVE extension and none of the superseded ones;
//!  10. the coin then moves alice -> bob -> carol **entirely off-chain**, every hop validating. The
//!      second hop is the one that can only pass if the receiver's tier seals were really opened:
//!      to build its own `S''` bob must colour a transition spending `X_m`'s payload output, which
//!      is an INTERNAL seal of the chain it was handed, not the output that pays it;
//!  11. and a wallet that OPTS OUT is unchanged: with `colored_ladder` explicitly off, carol's own
//!      carrier stays flat (`LadderSkipped { RgbCarrier }`) and still transfers tokens end-to-end on
//!      the legacy lane. (`colored_ladder` also SHIPS off — alice and bob opt in by name; that
//!      default is pinned separately, at the top of the test, so it cannot move silently either.)
//!
//! Run: SDK_E2E=74 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{LadderSkipReason, SdkConfig, UtexoWallet, WalletEvent};
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

const PLAIN_AMOUNT: u32 = 123_456;
const SUPPLY: u64 = 1_000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<WalletEvent>) -> Vec<WalletEvent> {
    let mut out = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(ev) => out.push(ev),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    out
}

/// Chain tip height, read from electrum. Used to prove a hop was ENTIRELY off-chain: nothing
/// confirmed, because nothing was broadcast.
fn chain_height(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<usize> {
    use electrum_client::ElectrumApi;
    Ok(cc.electrum_client.block_headers_subscribe()?.height)
}

fn parse(hex_tx: &str) -> Result<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::consensus::deserialize;
    Ok(deserialize(&hex::decode(hex_tx)?)?)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk74_alice", "./rgb-data-sdk74_bob", "./rgb-data-sdk74_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;

    // alice: the CTES-R wallet — coloured ladders ON.
    let mut alice_cfg = SdkConfig::regtest("sdk74_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk74_alice".to_string());
    alice_cfg.colored_ladder = true;
    // carol: the CONTROL wallet — coloured ladders explicitly OFF.
    //
    // RE-DERIVED, and pinned in the direction the code actually has. carol has always had to prove
    // the same two things — a wallet running with the lane OFF leaves its own carrier flat, and can
    // still RECEIVE a coloured ladder built by someone else — and both halves are still asserted
    // below. She sets the flag by hand regardless of what the default is, so the control states the
    // lane it is controlling for rather than inheriting it.
    //
    // The pin below is the same pin, aimed at the truth: `colored_ladder` ships **false** (2c351c6).
    // The lane is SOUND — that is what alice proves in this very test — but it is not the default,
    // because of the measured economics of what it switches on
    // (`docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md`: one coloured partial payment per carrier, ever,
    // and a 4_284-block unilateral exit for the child it produces). Keeping the pin means the
    // shipping default still cannot move without a test saying so; alice and bob opt IN explicitly,
    // ten lines up and down, so what this test proves about the lane is untouched by it.
    assert!(
        !SdkConfig::regtest("default-probe").colored_ladder,
        "colored_ladder must ship OFF by default (2c351c6) — the lane is sound (this test proves \
         it) but its economics are not, so it is opt-in. Flipping the default is a product \
         decision and must not happen silently."
    );
    let mut carol_cfg = SdkConfig::regtest("sdk74_carol");
    carol_cfg.rgb_data_dir = Some("./rgb-data-sdk74_carol".to_string());
    carol_cfg.colored_ladder = false;
    // bob and carol are the onward hops of the coloured coin.
    let mut bob_cfg = SdkConfig::regtest("sdk74_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk74_bob".to_string());
    bob_cfg.colored_ladder = true;

    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (carol, _) = UtexoWallet::initialize(carol_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_address = bob.get_utexo_address().await?;
    let mut alice_events = alice.subscribe();

    // ---- 1. One wallet, two coins: a PLAIN deposit and an RGB carrier. --------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let plain_addr = alice.get_deposit_address(PLAIN_AMOUNT as u64).await?;
    bitcoin_core::sendtoaddress(PLAIN_AMOUNT, &plain_addr)?;
    let rgb_fund_addr = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund_addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await; // electrs indexing

    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset_id = alice.issue_token("CTES", "Coloured Ladder Token", 0, SUPPLY).await?;
    bitcoin_core::generatetoaddress(3, &core)?;

    // No `establish` call anywhere: claim() is the ONLY thing that ladders, coloured or plain.
    let mut seen: Vec<WalletEvent> = Vec::new();
    let mut waited = 0;
    loop {
        alice.claim().await?;
        seen.extend(drain(&mut alice_events));
        let b = alice.get_balance().await?;
        if b.available_sats >= PLAIN_AMOUNT as u64 && !b.tokens.is_empty() {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("plain coin + carrier did not both confirm: {b:?}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    // One more pass: the carrier's allocation is booked only after the deposit confirms, and the
    // coloured lane deliberately refuses a carrier whose allocation is not yet BOOKED.
    for _ in 0..10 {
        alice.claim().await?;
        seen.extend(drain(&mut alice_events));
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(alice.get_token_balances().await?[0].balance, SUPPLY, "full supply on alice");

    let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk74_alice").await?.coins;
    let confirmed: Vec<_> = coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .collect();
    let plain = confirmed
        .iter()
        .find(|c| c.amount == Some(PLAIN_AMOUNT))
        .ok_or(anyhow!("plain coin not found"))?;
    let plain_sid = plain.statechain_id.clone().ok_or(anyhow!("plain has no sid"))?;
    let carrier = confirmed
        .iter()
        .find(|c| c.amount != Some(PLAIN_AMOUNT))
        .ok_or(anyhow!("carrier coin not found"))?;
    let carrier_sid = carrier.statechain_id.clone().ok_or(anyhow!("carrier has no sid"))?;
    let carrier_op = format!(
        "{}:{}",
        carrier.utxo_txid.clone().unwrap_or_default(),
        carrier.utxo_vout.unwrap_or_default()
    );

    // ---- 2. The PLAIN deposit is laddered exactly as before. ------------------------------------
    let plain_bundle = mercuryrustlib::tesr::load(&cc, "sdk74_alice", &plain_sid)
        .await?
        .ok_or(anyhow!("the plain deposit was not laddered"))?;
    assert!(
        !plain_bundle.is_colored() && plain_bundle.rgb.is_none(),
        "a plain deposit must produce a PLAIN ladder — the plain path is byte-identical"
    );
    for (name, tier) in [
        ("trigger", &plain_bundle.trigger),
        ("extension", &plain_bundle.current().extension),
        ("state", &plain_bundle.current().state),
    ] {
        assert_eq!(tier.payload_vout, 0, "plain {name} keeps payload_vout 0");
        let tx = parse(&tier.signed_tx)?;
        assert_eq!(tx.output.len(), 2, "plain {name} is [payload, P2A] — no opret");
        assert!(
            !tx.output.iter().any(|o| o.script_pubkey.is_op_return()),
            "plain {name} must carry NO OP_RETURN"
        );
    }
    println!("SDK74 - the plain deposit {plain_sid} is laddered PLAIN (2 outputs, payload at vout 0)");

    // ---- 3. The CARRIER is laddered, and the ladder is COLOURED. --------------------------------
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk74_alice", &carrier_sid)
        .await?
        .ok_or(anyhow!(
            "the RGB carrier was NOT laddered — CTES-R's whole point is that it can be"
        ))?;
    let rgb_half = bundle
        .rgb
        .clone()
        .ok_or(anyhow!("the carrier's ladder is not COLOURED (TesrBundle::rgb is None)"))?;
    assert!(bundle.is_colored());
    assert_eq!(rgb_half.contract_id, asset_id, "the ladder carries THIS contract");
    assert_eq!(rgb_half.amount, SUPPLY, "the whole allocation rides the ladder");
    assert_eq!(rgb_half.consignments.len(), 3, "one consignment per tier (T, X_0, S_0)");
    assert!(
        seen.iter().any(|e| matches!(
            e,
            WalletEvent::LadderEstablished { statechain_id } if *statechain_id == carrier_sid
        )),
        "LadderEstablished was not emitted for the carrier"
    );

    // ---- 4. Shape: opret at 0, PAYLOAD at 1, P2A at 2 — and the declared index is the real one. --
    let rate = bundle.fee_rate;
    let expected_fee = mercuryrustlib::rgb::colored_committed_fee(1, rate);
    let plain_fee = mercurylib::tesr::committed_fee(rate);
    assert_eq!(
        expected_fee - plain_fee,
        43 * rate as u64,
        "the coloured surcharge is exactly the opret's 43 vB at the committed rate"
    );
    let tiers = bundle.exit_tiers();
    let mut prev: Option<(electrum_client::bitcoin::Txid, u32, u64)> = None;
    for (i, tier) in tiers.iter().enumerate() {
        let tx = parse(&tier.signed_tx)?;
        assert_eq!(tx.txid().to_string(), tier.txid, "tier {i} txid mismatch");
        assert_eq!(tx.version, 3, "tier {i} is v3/TRUC");
        assert_eq!(tx.output.len(), 3, "coloured tier {i} is [opret, payload, P2A]");
        let oprets: Vec<usize> = tx
            .output
            .iter()
            .enumerate()
            .filter(|(_, o)| o.script_pubkey.is_op_return())
            .map(|(v, _)| v)
            .collect();
        assert_eq!(oprets, vec![0], "coloured tier {i} carries exactly one OP_RETURN, at vout 0");
        assert_eq!(
            tier.payload_vout, 1,
            "coloured tier {i} declares its payload at vout 1 (opret shifted it)"
        );
        assert_eq!(
            tx.output[2].script_pubkey.as_bytes(),
            &mercurylib::tesr::P2A_SCRIPT_BYTES,
            "coloured tier {i}'s P2A anchor moved to vout 2"
        );
        assert_eq!(tx.output[2].value, mercurylib::tesr::P2A_VALUE);
        assert_eq!(
            tx.output[tier.payload_vout as usize].value,
            tier.out_value,
            "coloured tier {i}'s declared out_value is the value AT its declared payload vout"
        );
        // Chaining: each tier spends its PARENT'S DECLARED payload vout, and the fee is the
        // coloured one. This pair is what an off-by-one in the migration would break.
        if let Some((ptxid, pvout, pvalue)) = prev {
            assert_eq!(tx.input[0].previous_output.txid, ptxid, "tier {i} parent txid");
            assert_eq!(
                tx.input[0].previous_output.vout, pvout,
                "tier {i} must spend its parent's PAYLOAD vout, not vout 0 (the opret)"
            );
            let out_total: u64 = tx.output.iter().map(|o| o.value).sum();
            assert_eq!(
                pvalue - out_total,
                expected_fee,
                "coloured tier {i} must commit committed_fee_for_outputs(2, {rate}) = {expected_fee}"
            );
        } else {
            assert_eq!(tx.input[0].previous_output.vout, bundle.f_vout, "the trigger spends F");
            let out_total: u64 = tx.output.iter().map(|o| o.value).sum();
            assert_eq!(bundle.f_value - out_total, expected_fee, "the coloured trigger's fee");
        }
        prev = Some((tx.txid(), tier.payload_vout, tier.out_value));
    }
    println!(
        "SDK74 - all 3 tiers COLOURED: opret@0, payload@1, P2A@2, fee {expected_fee} sat each \
         (uncoloured would be {plain_fee})"
    );

    // ---- 5. The CENSUS balances at exactly the plain path's value. ------------------------------
    //
    // Colouring adds ZERO SE co-signs — one input, one sighash, one cosign_tier — so the equation is
    // `num_sigs == flat_backups(1, the deposit-anchored tx1) + tiers(3) + superseded(0)`. Verified
    // with the SAME bound verifier a receiver runs, against the coordinator's live count.
    let info = mercuryrustlib::utils::get_statechain_info(&carrier_sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain info for the carrier"))?;
    assert_eq!(
        info.num_sigs, 4,
        "a coloured ladder must consume exactly 3 co-signs on top of the deposit's tx1"
    );
    {
        use electrum_client::ElectrumApi;
        let f_txid: electrum_client::bitcoin::Txid = bundle.f_txid.parse()?;
        let tx0 = cc.electrum_client.transaction_get(&f_txid)?;
        let tx0_hex = hex::encode(electrum_client::bitcoin::consensus::serialize(&tx0));
        let authority = mercuryrustlib::tesr::coin_authority_from_tx0(
            &carrier_sid,
            &bundle.f_txid,
            bundle.f_vout,
            &tx0_hex,
            info.aggregate_pubkey.clone(),
        )?;
        mercuryrustlib::tesr::verify_bundle_bound(&bundle, info.num_sigs, 1, &authority).map_err(
            |e| anyhow!("the coloured ladder does not pass the receiver's bound verifier: {e}"),
        )?;
    }
    println!("SDK74 - census balances: num_sigs 4 == 1 flat backup + 3 tiers (colouring adds none)");

    // ---- 6. The RGB half: the consignment validates against the UN-BROADCAST ladder. ------------
    //
    // The decisive probe. Not `get_asset_balance` — E7 measured that reporting a full, settled,
    // spendable balance with the stock at ZERO. This resolves every tier through the fork's
    // OffchainResolver using the ladder's own txids, which is only possible if each tier's bundle
    // really is in the stash and really does close its parent's seal.
    for txid in tiers.iter().map(|t| &t.txid) {
        use electrum_client::ElectrumApi;
        let id: electrum_client::bitcoin::Txid = txid.parse()?;
        assert!(
            cc.electrum_client.transaction_get(&id).is_err(),
            "tier {txid} must still be UN-BROADCAST — the whole point is that idle coins never age"
        );
    }
    let (contract, assigned, probed_txids, detail) =
        alice.colored_ladder_health(&carrier_sid).await.map_err(|e| {
            anyhow!("the coloured ladder's own consignment does not validate off-chain: {e}")
        })?;
    assert_eq!(contract, asset_id, "the consignment validates under THIS contract");
    assert_eq!(
        assigned, SUPPLY,
        "the leaf consignment must assign the FULL allocation to the final state's payload output"
    );
    assert_eq!(probed_txids.len(), 3);
    println!(
        "SDK74 - leaf consignment valid against the 3 un-broadcast tier txids; it assigns \
         {assigned} {contract} to S_0's payload output (detail: {detail:?})"
    );

    // ---- 7. The allocation is INTACT and the carrier is still quarantined. ----------------------
    //
    // Colouring must not move the allocation off `F` in the engine's own view: if it did, `F` would
    // stop looking like a carrier and plain-BTC selection would happily spend it — a fail-OPEN that
    // destroys the asset. Checked as a supplement to (6), never as the evidence.
    assert_eq!(
        alice.get_token_balances().await?[0].balance,
        SUPPLY,
        "the allocation is intact after laddering"
    );
    let allocs = alice.list_token_allocations(&asset_id).await?;
    assert!(
        allocs.iter().any(|(op, amt)| *op == carrier_op && *amt == SUPPLY),
        "the carrier outpoint must still hold the full allocation (and so stay quarantined from \
         plain-BTC selection): {allocs:?}"
    );
    println!("SDK74 - allocation intact: {SUPPLY} still bound to the carrier outpoint {carrier_op}");

    // ---- 8. THE LANE INTERLOCK: one carrier, one spend of F. ------------------------------------
    //
    // WHAT THIS STEP USED TO ASSERT, AND WHY IT NO LONGER HOLDS AS WRITTEN. Until the CTES-R
    // in-ladder split existed, `transfer_tokens` on a coloured carrier had exactly one route — the
    // legacy `create_colored_split_tx`, which spends `F` directly. `T` already spends `F` with no
    // timelock, so that split is a rival the previous owner out-races instantly; the SDK therefore
    // REFUSED the whole operation, and this step asserted the refusal.
    //
    // The property that mattered was never "a coloured carrier cannot pay" — it was "**nothing but
    // `T` may ever spend `F`**". There is now a second route that satisfies it: the coloured
    // in-ladder split carves the payment out of `SP`, a descendant of `T` over `X_m`'s payload
    // output, so `F` gains no rival. So the assertion is not deleted or relaxed — it is restated as
    // the invariant it was standing in for, and checked directly on the transaction that was built:
    // the payment must SUCCEED and no tier of it may spend `F`. (The refusal itself is still tested,
    // at its own level, by `refuse_if_colored_ladder`'s unit coverage and by sdk77's plain-lane
    // negative controls.)
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    // THE PROBE MOVED AGAIN, AND THIS TIME THERE IS NOWHERE LEFT TO POINT IT.
    //
    // The previous revision asserted the refusal on `batch_transfer_tokens`, the one call site that
    // was still legacy-only. That lane is now ported too: a coloured carrier routes into the N-ary
    // coloured in-ladder split, so the batch SUCCEEDS — and succeeding CONSUMES the carrier, which
    // this test still needs for its renewal and conveyance steps below.
    //
    // That is not the assertion decaying; it is the objective being met. There is no longer ANY
    // route from a coloured carrier into `create_colored_split_tx`: the single-carrier transfer, the
    // multi-carrier combine and the N-recipient batch all fork to the coloured lane before the
    // legacy one, and `refuse_legacy_colored_split_lane` refuses the legacy lane outright while
    // `SdkConfig::colored_ladder` is on. "The legacy lane refuses a coloured carrier" has become
    // unfalsifiable at the E2E level because the legacy lane is unreachable — which is exactly what
    // retiring it means. It is asserted where it can still fail: `tokens::retired_split_lane_census`
    // greps the SDK source and fails if any route reaches those primitives without the gate.
    //
    // What IS still falsifiable on this carrier, and is what the step was ever standing in for, is
    // the invariant itself — **nothing but a coloured tier may spend one of this coin's sealed
    // outputs, and nothing but `T` may spend `F`**. Both halves are checked here, and neither
    // touches the carrier:
    //
    //  (a) the carrier's sats are QUARANTINED from plain-BTC spendable balance, so no plain split
    //      builder can even SELECT it — the payment path that would spend `F` cannot see the coin.
    //      Measured against the PLAIN deposit in the same wallet rather than against a refusal:
    //      alice deliberately holds ordinary sats too, so `transfer()` succeeding proves nothing;
    //      what proves it is that the carrier's sats are absent from `available_sats`;
    //  (b) the PLAIN in-ladder split builder, handed this exact bundle, refuses by name.
    let carrier_sats = {
        let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk74_alice").await?.coins;
        coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(carrier_sid.as_str()))
            .and_then(|c| c.amount)
            .ok_or(anyhow!("carrier coin not found"))? as u64
    };
    let avail = alice.get_balance().await?.available_sats;
    // EXACT, not a bound: alice's spendable balance must be the plain deposit and nothing else.
    // `> 0` alone would pass on a quarantine that hid everything, and `< plain + carrier` would pass
    // on one that leaked a single sat of the carrier.
    assert_eq!(
        avail,
        u64::from(PLAIN_AMOUNT),
        "alice's plain-BTC spendable balance must be exactly her plain deposit — the carrier's \
         {carrier_sats} sat must be quarantined out of it entirely"
    );
    let guard = mercuryrustlib::tesr::refuse_uncolored_over_colored(&bundle, "in_ladder_split");
    let guard_msg = guard.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        guard_msg.contains("in_ladder_split") && guard_msg.contains("COLOURED"),
        "the PLAIN in-ladder split must refuse this coloured carrier by name, got: {guard_msg:?}"
    );
    println!(
        "SDK74 - no plain-split path can reach this carrier: its {carrier_sats} sat are absent from \
         the plain-BTC spendable balance ({avail} available, plain deposit alone is {PLAIN_AMOUNT}) \
         and the uncoloured in-ladder split refuses it ({})",
        guard_msg.lines().next().unwrap_or("").trim()
    );

    // Same refusal one layer down: conveying the coin itself is refused too, because a coloured
    // ladder has no receiver-side consignment validation yet.
    let err = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk74_alice", &carrier_sid, None, false, None,
    )
    .await
    .err()
    .map(|e| e.to_string())
    .unwrap_or_else(|| panic!("a COLOURED ladder must not be conveyed: the receiver would bind the sats without the asset"));
    assert!(
        err.contains("COLOURED (CTES-R) ladder"),
        "unexpected conveyance refusal: {err}"
    );
    println!("SDK74 - interlock holds: no plain-split path reaches the carrier AND the flat ladder conveyance still refuses");

    // ---- 10. RENEWAL: >=3 RIVAL extensions over ONE parent output, and the live one is NOT the
    //          internal-txid minimum. -----------------------------------------------------------
    //
    // THE case CTESR-GATE §2.2 measured collapsing. Every renewal replaces `X_m` over the trigger's
    // payload output, so after k renewals there are k+1 rival transitions over ONE outpoint. Under a
    // shared blinding they merge into a single BundleId and rgb-lib keeps whichever witness has the
    // numerically smallest INTERNAL (little-endian) txid — an arbitrary hash lottery. The loser's
    // consignment then embeds the rival's witness and NO branch validates.
    //
    // A 2-rival test passes ~50% of the time by luck, and even a 3-rival test passes if the live
    // tier happens to be the lottery winner. So renew until BOTH hold: at least 3 rivals exist, and
    // the LIVE extension is deliberately not the internal-txid minimum. Under collapse that is
    // exactly the configuration in which the leaf consignment comes back carrying the wrong witness.
    let mut rivals: Vec<electrum_client::bitcoin::Txid> = vec![bundle.current().extension.txid.parse()?];
    let mut csv_e = bundle.current().extension.csv.ok_or(anyhow!("no ext csv"))?;
    let mut renewals = 0u32;
    let params = bundle.params;
    let live_is_not_minimum = |rivals: &Vec<electrum_client::bitcoin::Txid>| {
        rivals.len() >= 3 && rivals.iter().min() != rivals.last()
    };
    while !live_is_not_minimum(&rivals) {
        csv_e = csv_e
            .checked_sub(1)
            .filter(|c| *c >= params.e_floor)
            .ok_or(anyhow!("ran out of extension CSV headroom before the rival condition held"))?;
        let m = alice.renew_colored_ladder_with(&carrier_sid, csv_e, params.state_csv(0)).await?;
        renewals += 1;
        assert_eq!(m, renewals, "each renewal advances the counter the seal rung folds in");
        let b = mercuryrustlib::tesr::load(&cc, "sdk74_alice", &carrier_sid)
            .await?
            .ok_or(anyhow!("ladder vanished"))?;
        rivals.push(b.current().extension.txid.parse()?);
    }
    let renewed = mercuryrustlib::tesr::load(&cc, "sdk74_alice", &carrier_sid)
        .await?
        .ok_or(anyhow!("ladder vanished"))?;
    let live_x = renewed.current().extension.clone();
    let min_rival = *rivals.iter().min().unwrap();
    assert!(rivals.len() >= 3, "need >=3 rivals over one parent output, have {}", rivals.len());
    assert_ne!(
        min_rival.to_string(),
        live_x.txid,
        "the live extension must NOT be the internal-txid minimum — otherwise a collapse would be \
         invisible because the lottery happens to pick the right witness"
    );
    assert_eq!(
        renewed.superseded_extensions.len(),
        rivals.len() - 1,
        "every rival but the live one is disclosed as superseded"
    );
    // Every rival is a DISTINCT transition, because every rival carries a distinct seal blinding.
    {
        use mercuryrustlib::rgb::TierRole;
        let mut blindings = std::collections::HashSet::new();
        for (i, t) in std::iter::once(&renewed.trigger)
            .chain(renewed.superseded_extensions.iter())
            .chain(std::iter::once(&live_x))
            .enumerate()
        {
            let role = if i == 0 { TierRole::Trigger } else { TierRole::Extension };
            let m = if i == 0 { 0 } else { i as u32 };
            let b = mercuryrustlib::tesr::colored_tier_seal(&carrier_sid, role, 0, m, t.csv)
                .blinding();
            assert!(blindings.insert(b), "seal blinding collision at rival {i}");
        }
    }
    // THE decisive assertion: the leaf consignment embeds the LIVE extension's witness, not the
    // lottery winner's. Under the collapse this is precisely what comes back wrong.
    let leaf = renewed.leaf_consignment().cloned().ok_or(anyhow!("no leaf consignment"))?;
    let witnesses = mercury_rgb::consignment_witness_txids(&leaf)?;
    assert!(
        witnesses.contains(&live_x.txid),
        "the leaf consignment does not carry the LIVE extension {} — bundle collapse (witnesses {witnesses:?})",
        live_x.txid
    );
    for dead in renewed.superseded_extensions.iter() {
        assert!(
            !witnesses.contains(&dead.txid),
            "the leaf consignment carries a SUPERSEDED extension {} — the collapse picked a rival",
            dead.txid
        );
    }
    // And it still resolves as a whole ladder through the OffchainResolver.
    let (_, assigned, _, _) = alice.colored_ladder_health(&carrier_sid).await.map_err(|e| {
        anyhow!("the renewed coloured ladder does not validate off-chain: {e}")
    })?;
    assert_eq!(assigned, SUPPLY, "the renewed ladder still carries the whole allocation");
    println!(
        "SDK74 - {renewals} coloured renewal(s): {} RIVAL extensions over {}:{}; live = {} (internal-txid \
         minimum is {}, deliberately NOT the live one); leaf consignment embeds the live one and no rival",
        rivals.len(),
        renewed.trigger.txid,
        renewed.trigger.payload_vout,
        live_x.txid,
        min_rival
    );

    // ---- 11. alice -> bob -> carol, entirely OFF-CHAIN. -----------------------------------------
    //
    // A transfer co-signs a fresh state `S'` one delta LOWER over the SAME extension output the
    // sender's own state spends — a second rival family. bob then does it again, which is the
    // assertion that only passes if the receiver's seals were really opened: to build its own `S''`
    // bob must colour a transition spending `X_m`'s payload output, and that outpoint's seal is an
    // INTERNAL seal of the chain it was handed, not the output that pays it.
    let tip = chain_height(&cc)?;
    let mut bob_events2 = bob.subscribe();
    alice.transfer_colored_carrier(&carrier_sid, &bob_address).await?;
    let mut waited = 0;
    loop {
        bob.claim().await?;
        if bob.get_token_balances().await?.iter().any(|b| b.asset_id == asset_id && b.balance == SUPPLY) {
            break;
        }
        waited += 1;
        if waited > 40 {
            return Err(anyhow!("bob did not book the coloured carrier: {:?}", bob.get_token_balances().await?));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let _ = drain(&mut bob_events2);
    assert_eq!(
        chain_height(&cc)?,
        tip,
        "the whole hop must be OFF-CHAIN — not one block was needed"
    );
    assert!(
        alice.get_token_balances().await?.iter().all(|b| b.asset_id != asset_id || b.balance == 0),
        "alice must no longer hold the asset she conveyed"
    );
    let bob_bundle = mercuryrustlib::tesr::load(&cc, "sdk74_bob", &carrier_sid)
        .await?
        .ok_or(anyhow!("bob did not adopt the ladder"))?;
    assert!(bob_bundle.is_colored(), "bob adopted a ladder that lost its colour");
    assert_eq!(
        bob_bundle.current().extension.txid,
        live_x.txid,
        "bob adopted the ladder over alice's LIVE extension"
    );
    assert!(
        bob_bundle.current().state.csv < renewed.current().state.csv,
        "the receiver-paying S' must mature BEFORE the sender's retained state"
    );
    let (_, bob_assigned, bob_txids, _) = bob.colored_ladder_health(&carrier_sid).await?;
    assert_eq!(bob_assigned, SUPPLY, "bob's own leaf validates against the un-broadcast ladder");
    assert_eq!(bob_txids.len(), 3);
    for txid in &bob_txids {
        use electrum_client::ElectrumApi;
        let id: electrum_client::bitcoin::Txid = txid.parse()?;
        assert!(
            cc.electrum_client.transaction_get(&id).is_err(),
            "tier {txid} must still be UN-BROADCAST after the hop"
        );
    }
    println!(
        "SDK74 - hop 1 alice -> bob validates: {SUPPLY} booked off-chain, S' at CSV {} out-races \
         alice's retained {} , all 3 tiers still un-broadcast",
        bob_bundle.current().state.csv.unwrap_or_default(),
        renewed.current().state.csv.unwrap_or_default()
    );

    let carol_address = carol.get_utexo_address().await?;
    bob.transfer_colored_carrier(&carrier_sid, &carol_address).await?;
    let mut waited = 0;
    loop {
        carol.claim().await?;
        if carol.get_token_balances().await?.iter().any(|b| b.asset_id == asset_id && b.balance == SUPPLY) {
            break;
        }
        waited += 1;
        if waited > 40 {
            return Err(anyhow!("carol did not book the coloured carrier: {:?}", carol.get_token_balances().await?));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(chain_height(&cc)?, tip, "hop 2 must be OFF-CHAIN too — still not one block");
    assert!(
        bob.get_token_balances().await?.iter().all(|b| b.asset_id != asset_id || b.balance == 0),
        "bob must no longer hold the asset he conveyed"
    );
    let carol_bundle = mercuryrustlib::tesr::load(&cc, "sdk74_carol", &carrier_sid)
        .await?
        .ok_or(anyhow!("carol did not adopt the ladder"))?;
    assert!(carol_bundle.is_colored());
    assert_eq!(
        carol_bundle.current().extension.txid,
        live_x.txid,
        "the extension is invariant across hops — only the state is replaced"
    );
    assert!(
        carol_bundle.current().state.csv < bob_bundle.current().state.csv,
        "each hop's state must mature before the previous owner's"
    );
    let (carol_contract, carol_assigned, _, _) = carol.colored_ladder_health(&carrier_sid).await?;
    assert_eq!(carol_contract, asset_id);
    assert_eq!(carol_assigned, SUPPLY, "the full allocation arrived at carol");
    // The census still balances at the far end, verified with the receiver's own bound verifier.
    {
        use electrum_client::ElectrumApi;
        let info = mercuryrustlib::utils::get_statechain_info(&carrier_sid, &cc)
            .await?
            .ok_or(anyhow!("no statechain info"))?;
        let f_txid: electrum_client::bitcoin::Txid = carol_bundle.f_txid.parse()?;
        let tx0 = cc.electrum_client.transaction_get(&f_txid)?;
        let tx0_hex = hex::encode(electrum_client::bitcoin::consensus::serialize(&tx0));
        let authority = mercuryrustlib::tesr::coin_authority_from_tx0(
            &carrier_sid,
            &carol_bundle.f_txid,
            carol_bundle.f_vout,
            &tx0_hex,
            info.aggregate_pubkey.clone(),
        )?;
        // 2 flat backups: the deposit's tx1 plus one per hop... one per conveyance that co-signed a
        // backup to the receiver. Read it from the coin rather than guessing.
        let flat = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk74_carol", &carrier_sid)
            .await?
            .len() as u32;
        mercuryrustlib::tesr::verify_bundle_bound(&carol_bundle, info.num_sigs, flat, &authority)
            .map_err(|e| anyhow!("carol's conveyed coloured ladder fails the bound verifier: {e}"))?;
        println!(
            "SDK74 - census still exact after 2 hops + {renewals} renewal(s): num_sigs {} == {flat} flat \
             backups + 3 tiers + {} superseded",
            info.num_sigs,
            carol_bundle.superseded_states.len() + carol_bundle.superseded_extensions.len()
        );
    }
    println!("SDK74 - hop 2 bob -> carol validates: the RECEIVER can continue the ladder, so the seals really were opened");

    // ---- 12. The DEFAULT is unchanged: carol's OWN carrier stays flat, and still pays tokens. ---
    //
    // carol runs with `colored_ladder` OFF and has just RECEIVED a coloured ladder, which is the
    // sharper version of this control: the flag gates ESTABLISHING colour, never accepting it. Her
    // own freshly-issued carrier must still take the legacy flat lane, unchanged.
    let mut carol_events = carol.subscribe();
    let t = prepaid_token(&cc).await?;
    carol.add_prepaid_token(&t).await;
    let carol_fund = carol.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &carol_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let t = prepaid_token(&cc).await?;
    carol.add_prepaid_token(&t).await;
    let carol_asset = carol.issue_token("CTRL", "Control Token", 0, SUPPLY).await?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut carol_seen: Vec<WalletEvent> = Vec::new();
    let mut waited = 0;
    loop {
        carol.claim().await?;
        carol_seen.extend(drain(&mut carol_events));
        if carol
            .get_token_balances()
            .await?
            .iter()
            .any(|b| b.asset_id == carol_asset && b.balance == SUPPLY)
        {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("carol's carrier did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    for _ in 0..5 {
        carol.claim().await?;
        carol_seen.extend(drain(&mut carol_events));
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let carol_coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk74_carol").await?.coins;
    let carol_carriers: Vec<String> = carol_coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .filter_map(|c| c.statechain_id.clone())
        // …excluding the coloured coin she was HANDED: it arrived with a ladder, and adopting one is
        // not the same decision as establishing one.
        .filter(|id| *id != carrier_sid)
        .collect();
    assert!(!carol_carriers.is_empty(), "carol has a carrier");
    for sid in &carol_carriers {
        assert!(
            mercuryrustlib::tesr::load(&cc, "sdk74_carol", sid).await?.is_none(),
            "with colored_ladder OFF, a carrier must stay UN-laddered exactly as before"
        );
        assert_eq!(
            mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk74_carol", sid, 0)
                .await
                .as_deref(),
            Some(mercuryrustlib::transfer_sender::FLAT_RGB_CARRIER),
            "the default path still records the carrier as flat-lane"
        );
    }
    assert!(
        carol_seen.iter().any(|e| matches!(
            e,
            WalletEvent::LadderSkipped { reason, .. } if *reason == LadderSkipReason::RgbCarrier
        )),
        "the default path still surfaces LadderSkipped{{RgbCarrier}}"
    );
    // And the legacy lane still pays, so the default really is untouched.
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        carol.add_prepaid_token(&t).await;
    }
    let r = carol.transfer_tokens(&carol_asset, &bob_address, 250).await?;
    assert!(r.used_split, "the legacy colored split still runs on the default path");
    let recv = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TokenTransferClaimed { asset_id: a, amount, .. })
                    if a == carol_asset =>
                {
                    break amount
                }
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim carol's token transfer in time"))?;
    bob_bg.abort();
    assert_eq!(recv, 250, "bob booked 250 CTRL off-chain on the unchanged default path");
    println!("SDK74 - default path unchanged: carol's carrier stays flat and still pays 250 CTRL");

    println!(
        "SDK74 - PASS: a COLOURED ladder is established over an RGB carrier (opret@0, payload@1, \
         coloured fee, allocation intact, census unchanged), RENEWED against >=3 rivals over one \
         parent output with the live tier deliberately not the internal-txid minimum, and conveyed \
         alice -> bob -> carol entirely off-chain with every hop validating; the plain path and the \
         default (flag-off) path are untouched"
    );
    Ok(())
}
