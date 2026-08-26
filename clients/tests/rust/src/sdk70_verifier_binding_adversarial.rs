//! E2E (SDK_E2E=70) — **ADVERSARIAL: the `payload_vout` migration fails CLOSED, and the ladder
//! verifier is BOUND TO THE COIN [C-1] / cannot double-count a disclosed tier [C-2]**.
//!
//! Three independent properties, all against REAL ladders co-signed by the live SE:
//!
//! **A — `payload_vout` (CTES-R commit 1).** Colouring a tier puts the opret at index 0 and shifts
//! every ladder vout by one, so every chaining site now reads an explicit `payload_vout` accessor
//! instead of a literal `0`. The gate experiment (E1, CTESR-GATE (retired 2026-08-15) §2.1a′) predicted
//! there is **no silent-loss path** because every site cross-checks the index against transaction
//! CONTENT. That prediction is what this part proves, by breaking it: for each migrated site a bundle
//! declaring the WRONG `payload_vout` must be REJECTED with a NAMED error — never accepted, never a
//! panic, never a silent fallback to `output[0]`.
//!
//! **B — the verifier is bound to the coin [C-1].** `verify_bundle` checks the trigger against
//! `bundle.f_txid`/`f_vout` and the tier payees against `bundle.agg_address` — all fields of the very
//! bundle under test. It proves internal consistency, NOT that the ladder describes the coin being
//! accepted. This part builds the actual attack: a genuine, fully co-signed DECOY ladder over an
//! attacker-owned outpoint, exiting to the victim's own key (so the Model-A gate passes) and with a
//! tier count that balances the VICTIM's `num_sigs` (so the census passes). `verify_bundle` ACCEPTS
//! it — that is the bug — and `verify_bundle_bound` must REJECT it.
//!
//! **C — one co-sign, one census slot [C-2].** Repeating a genuine disclosed tier (or re-declaring a
//! live tier as superseded) inflates `expected` by one and absorbs a hidden co-signed state, while
//! passing every per-tier check because the padded entry IS a real tier.
//!
//! **D — the child lane.** The `live_csv_by_outpoint` census map is KEYED rather than content-checked
//! there (CTESR-GATE §3.2, the one site that warrants explicit care), so its key must move with the
//! payload vout of the tier it describes.
//!
//! Every part ends with an HONEST CONTROL that must still PASS — without it a verifier that rejected
//! everything would pass this suite.
//!
//! Run with SDK_E2E=70 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, str::FromStr};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercuryrustlib::{
    client_config::ClientConfig,
    tesr::{verify_bundle, verify_bundle_bound, CoinAuthority, TesrBundle, TesrTier},
};

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";
/// **[D44] The COIN's committed rate, read from the preset — not written down.**
///
/// Line 535 sizes a child off `x_m.out_value`, which belongs to a ladder the SDK built at the
/// PRESET's rate. Writing 2.0 here made the two disagree the moment D44 moved the preset to 3.0, and
/// the split's own conservation law refused the result: "child values sum to 98280 but must equal
/// 98155". Anything that must agree with a preset-built ladder has to come from the preset.
fn fee_rate() -> f64 {
    mercurylib::tesr::TesrParams::for_network("regtest").committed_fee_rate
}

async fn num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc).await?.ok_or(anyhow!("no info"))?.num_sigs)
}
async fn aggregate(cc: &ClientConfig, sid: &str) -> Result<Option<String>> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no info"))?
        .aggregate_pubkey)
}
async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let t = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &t).await
}

/// The funding transaction, read FROM THE CHAIN — the only trustworthy source for a coin's aggregate
/// scriptPubKey and value.
fn tx0_hex_from_chain(cc: &ClientConfig, txid: &str) -> Result<String> {
    let id = electrum_client::bitcoin::Txid::from_str(txid).map_err(|_| anyhow!("bad txid"))?;
    let raw = cc.electrum_client.transaction_get_raw(&id).map_err(|_| anyhow!("F not on chain"))?;
    Ok(hex::encode(raw))
}

/// A rejection is only a pass if it is THE rejection the attack targets. `expect` is a substring of
/// the named error the check under test raises; anything else (a parse slip, a network hiccup, an
/// unrelated guard firing first) fails the test loudly instead of being laundered into a green run.
fn assert_named_reject(r: Result<()>, attack: &str, expect: &str) -> Result<()> {
    match r {
        Ok(()) => Err(anyhow!("SECURITY: {attack} was ACCEPTED")),
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains(expect) {
                return Err(anyhow!(
                    "{attack} was rejected for the WRONG reason — expected an error containing {expect:?}, got: {msg}"
                ));
            }
            println!("SDK70 - {attack} correctly REJECTED: {msg}");
            Ok(())
        }
    }
}

/// Assert a tampered bundle is refused by the BOUND verifier, and surface WHY — a rejection for the
/// wrong reason is not a pass.
fn must_reject_bound(
    b: &TesrBundle,
    se: u32,
    flat_backups: u32,
    coin: &CoinAuthority,
    attack: &str,
    expect: &str,
) -> Result<()> {
    assert_named_reject(verify_bundle_bound(b, se, flat_backups, coin), attack, expect)
}

/// As above, for the structural (unbound) verifier — used where the tampering is inside the ladder
/// itself, so the coin binding is not what should be doing the rejecting.
fn must_reject(b: &TesrBundle, se: u32, flat_backups: u32, attack: &str, expect: &str) -> Result<()> {
    assert_named_reject(verify_bundle(b, se, flat_backups), attack, expect)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk70");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // ================= THE VICTIM COIN: a real ladder, co-signed by the live SE. =================
    let mut victim = deposit_coin(&cc, "sdk70_alice").await?;
    let victim_sid = victim.statechain_id.clone().ok_or(anyhow!("no victim sid"))?;
    let victim_exit = crate::bitcoin_core::getnewaddress()?;
    let bundle = mercuryrustlib::tesr::establish_auto(&cc, &mut victim, &victim_exit, NETWORK).await?;
    let se = num_sigs(&cc, &victim_sid).await?;
    let victim_agg = aggregate(&cc, &victim_sid).await?;
    let victim_tx0 = tx0_hex_from_chain(&cc, &bundle.f_txid)?;

    // The authority a receiver derives from the COIN: its funding outpoint, that output's on-chain
    // value and scriptPubKey, and the coordinator's recorded aggregate for the sid. Nothing here is
    // read from the conveyed bundle.
    let authority = mercuryrustlib::tesr::coin_authority_from_tx0(
        &victim_sid,
        &bundle.f_txid,
        bundle.f_vout,
        &victim_tx0,
        victim_agg.clone(),
    )?;

    // ---- CONTROL 0: the honest ladder passes BOTH verifiers. ------------------------------------
    verify_bundle(&bundle, se, 1).map_err(|e| anyhow!("honest bundle must verify (unbound): {e}"))?;
    verify_bundle_bound(&bundle, se, 1, &authority)
        .map_err(|e| anyhow!("honest bundle must verify (bound to its own coin): {e}"))?;
    println!("SDK70 - control 0: the honest ladder verifies, bound and unbound (num_sigs={se})");

    // ======================= PART A — payload_vout fails CLOSED, per site =======================

    // A1 [verify_bundle_ex check 1, `spk != agg_spk`]: the trigger declares its payload at index 1,
    //    which is the P2A anchor — so it no longer pays the aggregate key A.
    {
        let mut a = bundle.clone();
        a.trigger.payload_vout = 1;
        must_reject(
            &a,
            se,
            1,
            "A1 (trigger payload_vout=1 → payload output is the P2A anchor, not A)",
            "trigger does not pay the aggregate key A",
        )?;
    }

    // A2 [the accessor itself]: an OUT-OF-RANGE payload_vout must be a rejection, not a panic and not
    //    a silent fallback to output[0].
    {
        let mut a = bundle.clone();
        a.trigger.payload_vout = 9;
        must_reject(
            &a,
            se,
            1,
            "A2 (trigger payload_vout=9 → out of range)",
            "declared payload_vout 9 is out of range",
        )?;
    }

    // A3 [tier payee check]: same, one rung up — the extension's declared payload is the anchor.
    {
        let mut a = bundle.clone();
        let last = a.levels.len() - 1;
        a.levels[last].extension.payload_vout = 1;
        must_reject(
            &a,
            se,
            1,
            "A3 (extension payload_vout=1 → pays the wrong output)",
            "tier 1 pays the wrong output",
        )?;
    }

    // A4 [FINAL state payee check — the Model-A exit leg]: the receiver-paying output must be read
    //    through the accessor too, or a coloured ladder would "pay the owner" at the opret.
    {
        let mut a = bundle.clone();
        let last = a.levels.len() - 1;
        a.levels[last].state.payload_vout = 1;
        must_reject(
            &a,
            se,
            1,
            "A4 (final state payload_vout=1 → exit leg pays the wrong output)",
            "tier 2 pays the wrong output",
        )?;
    }

    // A5 [verify_bundle_ex check 2, the LINKAGE `vout != parent payload_vout`]. Isolating this check
    //    needs a parent whose payload really does sit at index 1, otherwise the payee check above
    //    fires first. Build one: swap the trigger's two outputs so it is [P2A, P2TR(A)] and declare
    //    payload_vout=1 (check 1 now passes), then hang the honest extension off its `out[0]`. The
    //    child spends output 0 while the parent's payload is at 1 ⟹ the ladder is not chained, and
    //    the linkage check must say so. (Both checks run before any signature check, so an unsigned
    //    re-build is enough to reach it — which is the point: it fails CLOSED, early.)
    {
        use electrum_client::bitcoin::{
            consensus::{deserialize, serialize},
            Transaction,
        };
        let mut a = bundle.clone();
        let mut t: Transaction = deserialize(&hex::decode(&bundle.trigger.signed_tx)?)?;
        t.output.swap(0, 1); // [P2TR(A), P2A] -> [P2A, P2TR(A)]
        let t_txid = t.txid().to_string();
        let payload_value = t.output[1].value;
        let csv_e = bundle.current().extension.csv.ok_or(anyhow!("no ext csv"))?;
        let x = mercurylib::tesr::build_extension(
            &t_txid,
            payload_value,
            &bundle.agg_address,
            NETWORK,
            csv_e,
            fee_rate(),
        )?;
        a.trigger = TesrTier {
            txid: t_txid,
            signed_tx: hex::encode(serialize(&t)),
            out_value: payload_value,
            csv: None,
            payload_vout: 1,
        };
        let last = a.levels.len() - 1;
        a.levels[last].extension = TesrTier {
            txid: x.txid,
            signed_tx: x.tx_hex,
            out_value: x.out_value,
            csv: Some(csv_e),
            payload_vout: 0,
        };
        must_reject(
            &a,
            se,
            1,
            "A5 (child spends out[0] while the parent's payload_vout is 1 — broken linkage)",
            "tier 1 does not spend its parent's payload output",
        )?;
    }

    // A6 [verify_tier_cosigned, wrong prevout AMOUNT]. A tier's taproot key-spend sighash commits to
    //    the value of the output it spends, so reading the wrong output (a 0-value opret, or here a
    //    restated `f_value`) breaks the signature rather than mis-paying. Uses the UNBOUND verifier
    //    deliberately: `verify_bundle_bound` would reject on the value binding first, and the point
    //    here is that even without the binding the sighash catches it.
    {
        let mut a = bundle.clone();
        a.f_value += 1;
        must_reject(
            &a,
            se,
            1,
            "A6 (restated F value → the trigger's co-sign sighash no longer verifies)",
            "exit tier 0 is not co-signed by A",
        )?;
    }

    // A7 [`tier_out_value` → None → FeeTooHigh]. This is what a chaining site sees if it reads the
    //    OPRET as the payload: `encode` would report `out_value = 0`, and the child builder then
    //    cannot cover its own fee. It must be a typed error, not a coin built on a nonsense value.
    {
        let e = mercurylib::tesr::build_extension(
            &bundle.trigger.txid,
            0, // the value an opret output carries
            &bundle.agg_address,
            NETWORK,
            bundle.current().extension.csv.ok_or(anyhow!("no ext csv"))?,
            fee_rate(),
        );
        match e {
            Err(mercurylib::error::MercuryError::FeeTooHigh) => {
                println!("SDK70 - A7 (payload value 0 → tier_out_value None) correctly REJECTED: FeeTooHigh")
            }
            Err(other) => return Err(anyhow!("A7 rejected with the WRONG error: {other:?}")),
            Ok(_) => return Err(anyhow!("SECURITY: A7 built a tier on a 0-value payload")),
        }
    }

    // ---- CONTROL A: the untouched ladder still verifies after all of that. ----------------------
    verify_bundle_bound(&bundle, se, 1, &authority)
        .map_err(|e| anyhow!("honest bundle must still verify after PART A: {e}"))?;
    println!("SDK70 - control A: the honest ladder still verifies (a reject-everything verifier would fail here)");

    // ============ PART B — [C-1] the decoy ladder: self-consistent, and NOT this coin ============
    //
    // Mallory holds her own laddered coin. She builds its ladder to exit to ALICE's key, so the
    // Model-A fund-safety gate (`owner_exit_address == the receiver's own backup address`) passes,
    // and conveys it as if it were the ladder of the coin she is sending Alice — while keeping the
    // REAL trigger of that coin. Her ladder is entirely genuine: every tier is co-signed, the chain
    // links, and the tier count balances a `num_sigs` identical to the victim's (both coins have the
    // same shape). Nothing INSIDE the bundle is wrong. Only the coin it describes is.
    let mut mallory = deposit_coin(&cc, "sdk70_mallory").await?;
    let mallory_sid = mallory.statechain_id.clone().ok_or(anyhow!("no mallory sid"))?;
    let decoy = mercuryrustlib::tesr::establish_auto(&cc, &mut mallory, &victim_exit, NETWORK).await?;
    let decoy_se = num_sigs(&cc, &mallory_sid).await?;
    assert_eq!(
        decoy_se, se,
        "the decoy is built to the same shape as the victim coin, so its census balances the victim's num_sigs"
    );
    assert_eq!(
        decoy.owner_exit_address, bundle.owner_exit_address,
        "the decoy exits to the VICTIM's own key — the Model-A gate cannot see the difference"
    );
    assert_ne!(decoy.f_txid, bundle.f_txid, "the decoy is rooted at Mallory's outpoint, not the coin's");

    // B0 — THE BUG, demonstrated: the unbound verifier ACCEPTS the decoy at the victim's count.
    verify_bundle(&decoy, se, 1).map_err(|e| {
        anyhow!("the decoy must be internally self-consistent for this test to mean anything: {e}")
    })?;
    println!("SDK70 - B0: the UNBOUND verifier accepts the decoy ladder at the victim's num_sigs — this is audit C-1");

    // B1 — bound to the coin: rejected on the statechain id (the census would balance another coin).
    must_reject_bound(
        &decoy,
        se,
        1,
        &authority,
        "B1 (decoy ladder for another statechain id)",
        "the census would balance a different coin",
    )?;

    // B2 — the attacker forges the sid field. The funding outpoint binding still catches it.
    {
        let mut d = decoy.clone();
        d.statechain_id = victim_sid.clone();
        must_reject_bound(
            &d,
            se,
            1,
            &authority,
            "B2 (decoy with the victim's sid — wrong funding outpoint)",
            "the coin's funding outpoint is",
        )?;
    }

    // B3 — and forges the funding outpoint too. The aggregate read from the coin's ON-CHAIN funding
    //      output still catches it: Mallory's tiers are co-signed under HER aggregate.
    {
        let mut d = decoy.clone();
        d.statechain_id = victim_sid.clone();
        d.f_txid = bundle.f_txid.clone();
        d.f_vout = bundle.f_vout;
        d.f_value = bundle.f_value;
        must_reject_bound(
            &d,
            se,
            1,
            &authority,
            "B3 (decoy with the victim's sid AND outpoint — wrong aggregate)",
            "aggregate address does not match the coin's on-chain funding key",
        )?;
    }

    // B4 — a MISSING coordinator aggregate is a rejection, not a fallback (fail-closed).
    {
        let mut no_agg = authority.clone();
        no_agg.se_aggregate_pubkey = None;
        must_reject_bound(
            &bundle,
            se,
            1,
            &no_agg,
            "B4 (coordinator recorded no aggregate — fail-closed)",
            "recorded no aggregate for statechain id",
        )?;
    }

    // B5 — a VALID but wrong coordinator aggregate (exercises the `!=`, not a parse failure).
    {
        let mut decoy_agg = authority.clone();
        decoy_agg.se_aggregate_pubkey =
            Some("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".to_string());
        must_reject_bound(
            &bundle,
            se,
            1,
            &decoy_agg,
            "B5 (coordinator aggregate != the funding output key)",
            "does not match the funding output key",
        )?;
    }

    // B6 [`taproot_key_hex` on a NON-TAPROOT spk] — the coin's funding output must be a v1 taproot
    //     output. Hand it the P2A anchor script (`OP_1 <0x4e73>`, a witness program that is NOT
    //     P2TR — exactly the shape that made `create_colored_split_tx`'s `!is_op_return` filter
    //     wrong, CTESR-GATE §2.1d) and it must fail with a named error.
    {
        let mut bad_spk = authority.clone();
        bad_spk.f_spk_hex = "51024e73".to_string();
        must_reject_bound(
            &bundle,
            se,
            1,
            &bad_spk,
            "B6 (funding spk is not a v1 taproot output)",
            "coin funding output is not a v1 taproot output",
        )?;
    }

    // B7 — restated funding VALUE (the amount every tier's sighash commits to).
    {
        let mut bad_val = authority.clone();
        bad_val.f_value += 1;
        must_reject_bound(
            &bundle,
            se,
            1,
            &bad_val,
            "B7 (funding value restated)",
            "the tier sighashes commit to the real value",
        )?;
    }

    // ---- CONTROL B ----
    verify_bundle_bound(&bundle, se, 1, &authority)
        .map_err(|e| anyhow!("honest bundle must still verify after PART B: {e}"))?;
    println!("SDK70 - control B: the honest ladder still verifies against its own coin");

    // ================= PART C — [C-2] one co-sign may be counted only once =================
    //
    // Renew first, so the bundle carries GENUINE superseded tiers (a superseded extension and the
    // state hanging off it). Every attack below pads with a real, fully co-signed tier — the [S1]
    // battery cannot see them, because there is nothing wrong with the tier itself.
    let mut renewed = bundle.clone();
    mercuryrustlib::tesr::renew_auto(&cc, &mut victim, &mut renewed).await?;
    let se_renew = num_sigs(&cc, &victim_sid).await?;
    verify_bundle_bound(&renewed, se_renew, 1, &authority)
        .map_err(|e| anyhow!("the renewed bundle must verify (control): {e}"))?;
    println!("SDK70 - control C: the renewed ladder verifies at num_sigs={se_renew} (1 superseded extension + 1 superseded state)");

    // C1 — disclose the SAME superseded state twice: `expected` grows by one for free and absorbs a
    //      hidden co-signed state, while every per-entry check still passes.
    {
        let mut c = renewed.clone();
        let dup = c.superseded_states.last().cloned().ok_or(anyhow!("renew produced no superseded state"))?;
        c.superseded_states.push(dup);
        must_reject_bound(
            &c,
            se_renew + 1,
            1,
            &authority,
            "C1 (a superseded tier disclosed twice)",
            "is disclosed more than once",
        )?;
    }

    // C2 — disclose the same superseded EXTENSION twice (the other list).
    {
        let mut c = renewed.clone();
        let dup = c
            .superseded_extensions
            .last()
            .cloned()
            .ok_or(anyhow!("renew produced no superseded extension"))?;
        c.superseded_extensions.push(dup);
        must_reject_bound(
            &c,
            se_renew + 1,
            1,
            &authority,
            "C2 (a superseded extension disclosed twice)",
            "is disclosed more than once",
        )?;
    }

    // C3 — re-declare a LIVE exit tier as superseded. It is a genuine co-sign of this ladder, already
    //      counted once by `tiers.len()`; counting it again is the same free slot. The set spans BOTH
    //      lists AND the live tiers precisely so this cannot be split across them.
    {
        let mut c = renewed.clone();
        c.superseded_extensions.push(c.current().extension.clone());
        must_reject_bound(
            &c,
            se_renew + 1,
            1,
            &authority,
            "C3 (a LIVE tier re-declared as superseded)",
            "is disclosed more than once",
        )?;
    }

    // C4 [the ROOT ladder's race map is CONTENT-derived] — on this lane `live_csv_by_outpoint` is
    //     keyed from `txs[i].input[0].previous_output`, i.e. from the live transaction itself, so a
    //     declared `payload_vout` can never move its key. Mis-declaring the live extension's payload on
    //     a RENEWED ladder (one that really does carry superseded tiers to be raced) therefore does NOT
    //     desync the census map — it is caught earlier and unconditionally by the CONTENT check that
    //     the extension pays `A` at its declared payload index. That is what this asserts, by name.
    //
    //     The genuinely KEYED instance of the map — the one CTESR-GATE §3.2 singles out, built from the
    //     DECLARED `payload_vout` — lives on the child lane only, and is attacked in PART D (D4/D4′)
    //     with a real co-signed rival so the map is actually consulted.
    {
        let mut c = renewed.clone();
        let last = c.levels.len() - 1;
        c.levels[last].extension.payload_vout = 1;
        must_reject_bound(
            &c,
            se_renew,
            1,
            &authority,
            "C4 (live extension payload_vout=1 on a renewed ladder — content-checked payee)",
            "tier 1 pays the wrong output",
        )?;
    }

    // ---- CONTROL C ----
    verify_bundle_bound(&renewed, se_renew, 1, &authority)
        .map_err(|e| anyhow!("the renewed bundle must still verify after PART C: {e}"))?;
    println!("SDK70 - control C′: the renewed ladder still verifies");

    // ============== PART D — the CHILD lane (the keyed census map lives here too) ==============
    //
    // A split child's segment is HEADLESS: its extension has no payee check at all (it is verified by
    // co-sign + CSV), so the linkage `state spends ext.payload_out` is reachable in isolation here,
    // and so is the `sp_vout` guard.
    let wallet = "sdk70_split";
    let mut parent = deposit_coin(&cc, wallet).await?;
    let parent_sid = parent.statechain_id.clone().ok_or(anyhow!("no split parent sid"))?;
    let parent_baseline = num_sigs(&cc, &parent_sid).await?;
    let parent_exit = crate::bitcoin_core::getnewaddress()?;
    let pbundle = mercuryrustlib::tesr::establish_auto(&cc, &mut parent, &parent_exit, NETWORK).await?;

    let x_m = pbundle.current().extension.clone();
    let child_value = mercurylib::tesr::tier_out_total(x_m.out_value, 1, fee_rate())
        .ok_or(anyhow!("committed fee too high for one child"))?;
    let child_token = prepaid_token(&cc).await?;
    let child_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
        &cc,
        wallet,
        &child_token,
        u32::try_from(child_value)?,
    )
    .await?;
    let child = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(&child_addr))
        .cloned()
        .ok_or(anyhow!("child slot not found"))?;
    let child_sid = child.statechain_id.clone().ok_or(anyhow!("no child sid"))?;
    let child_baseline = num_sigs(&cc, &child_sid).await?;

    let receiver = crate::sdk58_inladder_split::taproot_addr();
    let mut children = vec![(child.clone(), receiver.clone(), child_value)];
    // [CATS change 2] One recipient, NO change leg — see sdk58. The child under attack here is a
    // PIECE, whose two-tier shape change 2 deliberately leaves alone.
    let cb = mercuryrustlib::tesr::in_ladder_split(
        &cc,
        wallet,
        &mut parent,
        &pbundle,
        &mut children,
        &[],
        mercuryrustlib::tesr::ChangeLeg::None,
        // [K>1 prerequisite 2] No conveyance plan: this test drives the hand-over itself (or does
        // not hand over at all), so the journal records no recipient for the leg and the resume
        // driver correctly reports it `unactionable` rather than conveying it behind the test.
        &[],
    )
    .await?
    .pieces
    .into_iter()
    .next()
    .ok_or(anyhow!("in_ladder_split returned no child bundle"))?;

    let f_txid = electrum_client::bitcoin::Txid::from_str(&pbundle.f_txid).map_err(|_| anyhow!("bad f_txid"))?;
    let f_tx = cc.electrum_client.transaction_get(&f_txid).map_err(|_| anyhow!("F not on chain"))?;
    let f_spk_hex = hex::encode(f_tx.output[pbundle.f_vout as usize].script_pubkey.as_bytes());
    // …and its VALUE, from the same fetched transaction — the anchor the parent's trigger is bound to.
    let f_value_onchain = f_tx.output[pbundle.f_vout as usize].value;
    let p_ns = num_sigs(&cc, &parent_sid).await?;
    let p_agg = aggregate(&cc, &parent_sid).await?;
    let c_ns = num_sigs(&cc, &child_sid).await?;
    let c_agg = aggregate(&cc, &child_sid).await?;
    let (_, _, p_term) = mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_sid).await?;

    let vcb = |b: &mercuryrustlib::tesr::ChildTesrBundle| {
        mercuryrustlib::tesr::verify_child_bundle(
            b,
            &f_spk_hex,
            f_value_onchain,
            p_ns,
            parent_baseline,
            p_agg.as_deref(),
            p_term,
            c_ns,
            child_baseline,
            c_agg.as_deref(),
            &[],
            &receiver,
        )
    };
    let must_reject_child =
        |b: &mercuryrustlib::tesr::ChildTesrBundle, attack: &str, expect: &str| -> Result<()> {
            assert_named_reject(vcb(b), attack, expect)
        };

    // ---- CONTROL D: the honest split child is accepted. -----------------------------------------
    vcb(&cb).map_err(|e| anyhow!("the honest split child must verify (control): {e}"))?;
    println!("SDK70 - control D: the honest split child verifies");

    // D1 [the LINKAGE check, isolated] — a child extension has no payee check, so declaring its
    //    payload at index 1 (the P2A anchor) is caught purely by "the child state does not spend
    //    ext_child's payload output". This is the `vout != 0` site on the child lane.
    {
        let mut d = cb.clone();
        d.child_extension.as_mut().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").payload_vout = 1;
        must_reject_child(
            &d,
            "D1 (child extension payload_vout=1 — the child state no longer chains to it)",
            "child state does not spend ext_child's payload output",
        )?;
    }

    // D2 [the accessor, out of range] — on the child state, whose payload output carries BOTH the
    //    Model-A payee check and the value binding.
    {
        let mut d = cb.clone();
        d.child_state.payload_vout = 9;
        must_reject_child(
            &d,
            "D2 (child state payload_vout=9 — out of range)",
            "declared payload_vout 9 is out of range",
        )?;
    }

    // D3 [the funding-slot guard] — `SP.out[j]` means the j-th PAYLOAD output. A child slot that
    //    claims to sit BEFORE its funding tier's payload vout (index 0 becomes the opret once a tier
    //    is coloured) must be refused rather than silently resolved against the wrong output.
    {
        let mut d = cb.clone();
        let last = d.parent.levels.len() - 1;
        d.parent.levels[last].state.payload_vout = 1;
        must_reject_child(
            &d,
            "D3 (child funding vout precedes SP's declared payload vout)",
            "precedes the funding tier's payload vout",
        )?;
    }

    // ---- CONTROL D′ ----
    vcb(&cb).map_err(|e| anyhow!("the honest split child must still verify after PART D: {e}"))?;
    println!("SDK70 - control D′: the honest split child still verifies");

    // ===== D4 — the genuinely KEYED census map, with the map ACTUALLY CONSULTED =====
    //
    // `verify_child_bundle` builds the child segment's race map as
    //     child_live[(ext_child.txid, cb.child_extension.as_ref().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").payload_vout)] = state_child's CSV
    // — the ONE census key derived from a DECLARED field rather than from transaction content
    // (CTESR-GATE §3.2). Nothing above reaches it: a rival has to EXIST for the map to be looked up at
    // all, and no structural tampering can manufacture one, because a disclosed superseded tier is only
    // counted if it carries a genuine co-sign. So build the real thing.
    //
    // The blind SE co-signs whatever it is handed while the child slot is non-terminal, which is
    // precisely the attacker capability the race check exists to defeat: here it co-signs a SECOND
    // child state over `ext_child`'s payload output, paying an attacker key, at the SAME CSV as the
    // live one — so it does NOT lose the maturity race. Its txid is distinct (the [C-2] dedup does not
    // fire), it is ladder-linked, and it is genuinely co-signed by A_child (the [S1] battery does not
    // fire). The ONLY thing between it and a census slot is the per-outpoint maturity check reached
    // through that key. The count passed is the one that MAKES it balance (c_ns + 1, the real count
    // after the extra co-sign), so a count mismatch cannot be doing the rejecting either.
    let attacker_key = "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9";
    let attacker_addr = {
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        let x = XOnlyPublicKey::from_slice(&hex::decode(attacker_key)?)?;
        Address::p2tr(&Secp256k1::new(), x, None, Network::Regtest).to_string()
    };
    let live_child_csv = cb.child_state.csv.ok_or(anyhow!("live child state has no CSV"))?;
    let rival = mercurylib::tesr::build_state_from(
        &cb.child_extension.as_ref().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").txid,
        cb.child_extension.as_ref().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").payload_vout,
        cb.child_extension.as_ref().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").out_value,
        &attacker_addr,
        NETWORK,
        live_child_csv, // EQUAL to the live state's ⟹ it does not lose the race
        pbundle.fee_rate,
    )?;
    let rival_signed = mercuryrustlib::tesr::cosign_tier(
        &cc,
        &mut children[0].0,
        rival.tx_hex.clone(),
        cb.child_extension.as_ref().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").out_value,
        NETWORK,
    )
    .await?;
    let rival_tier = TesrTier {
        txid: rival.txid.clone(),
        signed_tx: rival_signed,
        out_value: rival.out_value,
        csv: Some(live_child_csv),
        payload_vout: rival.payload_vout,
    };
    let c_ns_after = num_sigs(&cc, &child_sid).await?;
    assert_eq!(
        c_ns_after,
        c_ns + 1,
        "the SE really did co-sign the rival child state (so this is a genuine co-sign, not a forgery)"
    );

    // D4 — the map is consulted and returns the LIVE child state's CSV: the rival ties it and is
    //      refused by the maturity race, not by dedup, not by the signature check, not by the count.
    {
        let mut d = cb.clone();
        d.child_superseded_states.push(rival_tier.clone());
        assert_named_reject(
            mercuryrustlib::tesr::verify_child_bundle(
                &d,
                &f_spk_hex,
                f_value_onchain,
                p_ns,
                parent_baseline,
                p_agg.as_deref(),
                p_term,
                c_ns_after,
                child_baseline,
                c_agg.as_deref(),
                &[],
                &receiver,
            ),
            "D4 (a genuinely co-signed rival child state at the LIVE state's CSV — raced through the keyed census map)",
            "race",
        )?;
    }

    // D4′ — THE KEY ITSELF. Same rival, but the declared `child_extension.payload_vout` is moved to 1.
    //       That declaration is the census key: at index 1 the map would describe `(ext_child, 1)` — an
    //       outpoint no live tier spends — so the rival would stop being raced at all and would fall
    //       through to the orphan branch instead of the maturity check. A key that disagrees with its
    //       tier must therefore be DETECTED, and it is: the content guard "state_child spends
    //       ext_child's declared payload output" runs first and refuses the bundle, which is exactly
    //       why the keyed map can never be silently desynced from the tier it describes.
    {
        let mut d = cb.clone();
        d.child_superseded_states.push(rival_tier.clone());
        d.child_extension.as_mut().expect("this E2E builds a TWO-RUNG piece; a thin one would have no extension").payload_vout = 1;
        assert_named_reject(
            mercuryrustlib::tesr::verify_child_bundle(
                &d,
                &f_spk_hex,
                f_value_onchain,
                p_ns,
                parent_baseline,
                p_agg.as_deref(),
                p_term,
                c_ns_after,
                child_baseline,
                c_agg.as_deref(),
                &[],
                &receiver,
            ),
            "D4′ (census KEY moved off its tier while a real rival is disclosed)",
            "child state does not spend ext_child's payload output",
        )?;
    }

    // ---- CONTROL D″: the untouched child still verifies at its own (now advanced) count. ----------
    // The rival co-sign really happened, so the honest bundle must now FAIL the census at the old
    // count and PASS at neither — it discloses no rival, so `c_ns_after` exceeds what it accounts for.
    // That is itself the census doing its job, and it is asserted rather than glossed over.
    assert_named_reject(
        mercuryrustlib::tesr::verify_child_bundle(
            &cb,
            &f_spk_hex,
            f_value_onchain,
            p_ns,
            parent_baseline,
            p_agg.as_deref(),
            p_term,
            c_ns_after,
            child_baseline,
            c_agg.as_deref(),
            &[],
            &receiver,
        ),
        "D4″ (the hidden rival is not disclosed at all — the exact-equality census catches it)",
        "possible hidden child state",
    )?;
    vcb(&cb).map_err(|e| anyhow!("the honest split child must still verify at its own count: {e}"))?;

    println!(
        "SDK70 - ✓ PASS: (A) every migrated payload_vout site fails CLOSED with a named error — no silent-loss path, as E1 predicted; \
         (B) a fully co-signed DECOY ladder that the UNBOUND verifier accepts at the victim's num_sigs is REJECTED once the authority is \
         derived from the coin (sid + funding outpoint + on-chain value + on-chain aggregate + the coordinator's recorded aggregate, fail-closed); \
         (C) a genuine tier cannot be counted twice, across both superseded lists and the live tiers; (D) on the child lane — the only place the \
         race map is keyed by a DECLARED payload_vout — a GENUINELY CO-SIGNED rival child state is raced through that key and refused, a key moved \
         off its tier is detected before the map is built, and an undisclosed rival is caught by the exact-equality census. Every rejection is \
         asserted BY NAME, so a rejection for the wrong reason fails the suite; honest ladders and an honest split child still verify."
    );
    Ok(())
}
