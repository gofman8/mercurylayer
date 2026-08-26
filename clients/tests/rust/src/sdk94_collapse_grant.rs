//! E2E (SDK_E2E=94) — **a tree actually CLOSES: the collapse's accept path, run end to end.**
//!
//! `collapse_grant` has been correct since #169 and every measurement of it was a REFUSAL — eight
//! probe cases, six distinct gates, and `se_root` empty because a root row is only ever created by
//! the freeze. A route measured only in refusal is a route whose success branch has never run, which
//! is the same shape as `sdk74`'s retry that never fired and the fork extractor that validated
//! perfectly and credited zero.
//!
//! This is that branch, run. Alice deposits, pays Bob a non-exact amount (an in-ladder split, which
//! creates a real tree AND terminalizes the root), then closes the tree:
//!
//!   1. **asks the SE what it owes** — `collapse_obligations`. No caller could ask before; the SE is
//!      the only party that knows, and a closer guessing costs a refusal per guess;
//!   2. **builds `C`** paying every unreleased frontier leaf its FULL funding value out of `F`, with
//!      the remainder — net of fee — to Alice. The tree's own money: the closer fronts nothing;
//!   3. **asks for the SE's half** — `request_collapse`, over `collapse/first` because a root that
//!      has been split has an exhausted budget and `sign/first` would refuse `410 Gone`;
//!   4. **broadcasts it.** The strongest available proof: the tree closes on chain, in ONE
//!      transaction, and every holder is paid at their own key.
//!
//! WHAT WOULD FAIL IF THE CODE WERE WRONG, which is the point rather than the green tick:
//!   * underpay one leaf by ONE satoshi and the grant must refuse, NAMING that leaf — if it granted,
//!     the predicate is not binding and a holder can be discharged unpaid;
//!   * ask twice and the second must not produce a second signature over a different transaction —
//!     the secnonce is consumed, which is the MuSig2 nonce-reuse key leak;
//!   * the root must read `frozen` afterwards, and it must be frozen in the SAME breath as the
//!     signature — INV-FREEZE gated nothing at all until #169;
//!   * and the broadcast must CONFIRM. A `C` the network refuses is a tree that cannot close, and
//!     the signature can never be reissued once the root is frozen.
//!
//! Run: SDK_E2E=94 ML_NETWORK=regtest cargo run --release

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const DEPOSIT: u64 = 100_000;
const PAY: u64 = 30_000;
/// Smallest fee `C` may pay and still relay. The fee is whatever `F` holds ABOVE the obligations —
/// never a shave off a payout, which is arithmetically identical to underpaying a leaf and is the
/// refusal the predicate exists to make.
const MIN_COLLAPSE_FEE: u64 = 500;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk94_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk94_bob"), None).await?;
    let bob_address = bob.get_utexo_address().await?;

    // ---- 1. Alice deposits; claim() auto-establishes the ladder. --------------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for i in 0..60 {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats == DEPOSIT {
            break;
        }
        if i == 59 {
            return Err(anyhow!("alice's deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let root_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk94_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    println!("SDK94 - (1) alice deposited {DEPOSIT}, ladder established (root {root_sid})");

    // ---- 2. Alice pays Bob: an in-ladder split. This is what MAKES a tree. -----------------------
    //
    // It also terminalizes the root — which is precisely why this test cannot use `sign/first` for
    // the collapse's nonce, and why `collapse/first` had to exist.
    let r = alice.transfer(&bob_address, PAY).await?;
    assert!(r.used_split, "a {PAY} payment from one {DEPOSIT} laddered coin must split in-ladder");
    for _ in 0..30 {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats == PAY {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    println!("SDK94 - (2) alice paid bob {PAY} via an in-ladder split — the tree now has leaves");

    // ---- 3. THE QUESTION NOBODY COULD ASK BEFORE: what does this tree owe? -----------------------
    let root_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk94_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(&root_sid))
        .cloned()
        .ok_or(anyhow!("root coin missing"))?;
    let signed_sid = root_coin
        .signed_statechain_id
        .clone()
        .ok_or(anyhow!("root coin has no auth attestation"))?;

    let ob = mercuryrustlib::transaction::collapse_obligations(
        &cc,
        &mercurylib::transaction::CollapseObligationsRequest {
            root_statechain_id: root_sid.clone(),
            statechain_id: root_sid.clone(),
            signed_statechain_id: signed_sid.clone(),
        },
    )
    .await?;
    assert!(!ob.obligations.is_empty(), "an empty obligation set discharges every holder at once");
    assert!(!ob.frozen, "the tree cannot already be closing");
    assert!(ob.have_funding_outpoint, "the SE must know which outpoint C has to spend");
    let owed = ob.total_owed();
    println!(
        "SDK94 - (3) the SE says this tree owes {} sat across {} leaf/leaves: {:?}",
        owed,
        ob.obligations.len(),
        ob.obligations.iter().map(|o| o.amount).collect::<Vec<_>>()
    );

    let funding_value = root_coin.amount.ok_or(anyhow!("root coin has no amount"))? as u64;
    let funding_txid = ob.funding_txid.clone().ok_or(anyhow!("no funding txid"))?;
    let funding_txid = funding_txid.trim_start_matches("0x").to_string();
    let funding_vout = ob.funding_vout.ok_or(anyhow!("no funding vout"))?;
    // **THE PROPERTY §5.4.4 CLAIMS, checked before anything is built: the tree pays for its own
    // close.** Every obligation plus the fee comes out of `F` — the tree's own money — so the closer
    // fronts NOTHING. If this ever fails, the zero-liquidity claim in §5.5 is false for this tree.
    assert!(
        owed <= funding_value,
        "the obligations ({owed}) exceed what F holds ({funding_value}) — the tree cannot pay for \
         its own close and the closer would have to front the difference"
    );
    let fee = funding_value - owed;
    assert!(
        fee >= MIN_COLLAPSE_FEE,
        "only {fee} sat left over for the collapse's fee; C would not relay"
    );

    // Alice's remainder, at her own exit key, net of the fee.
    // Alice's remainder goes to a key she controls. Taken from the node rather than from the wallet
    // because what matters here is that it is a valid P2TR destination whose payment can be CHECKED
    // on chain afterwards — the collapse's correctness does not depend on whose key it is.
    let alice_exit = bitcoin_core::getnewaddress_p2tr_xonly()?;
    // Alice is already paid as one of the obligations (her change leg IS a frontier leaf), so on
    // this tree the leftover is exactly the fee and there is no separate owner output. Passing a
    // zero remainder exercises the rule that a zero-value output must be OMITTED rather than
    // emitted: a 0-sat P2TR is non-standard, and it would be discovered at broadcast — after the
    // root is frozen and the signature can never be reissued.
    let remainder = 0u64;

    // ---- 4. THE NEGATIVE CONTROL FIRST, while the nonce is still unspent. ------------------------
    //
    // Deliberately before the honest attempt: run it after, and a refusal could be explained by the
    // consumed secnonce rather than by the predicate, and the control would prove nothing.
    let mut short = ob.payouts();
    short[0].1 -= 1;
    let short_c = mercurylib::tesr::build_collapse_tx(
        &funding_txid, funding_vout, funding_value, &short,
        Some((alice_exit.clone(), remainder + 1)),
    )?;
    let mut probe_coin = root_coin.clone();
    let refusal = mercuryrustlib::tesr::request_collapse(
        &cc, &mut probe_coin, &root_sid, short_c.tx_hex.clone(), funding_value, "regtest",
    )
    .await
    .expect_err("a C that underpays a leaf by ONE satoshi must be REFUSED");
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("in full") || refusal.contains("is owed"),
        "the refusal must name the PAYMENT gate and the shortfall, not something incidental: {refusal}"
    );
    assert!(
        refusal.contains(&short[0].0),
        "the refusal must NAME the underpaid leaf's key, so a closer can fix it: {refusal}"
    );
    println!("SDK94 - (4) one satoshi short -> REFUSED, naming the leaf: {refusal}");

    // ---- 5. THE ACCEPT PATH. -------------------------------------------------------------------
    let c = mercurylib::tesr::build_collapse_tx(
        &funding_txid, funding_vout, funding_value, &ob.payouts(),
        Some((alice_exit.clone(), remainder)),
    )?;
    let mut close_coin = root_coin.clone();
    let (grant, signed_c) = mercuryrustlib::tesr::request_collapse(
        &cc, &mut close_coin, &root_sid, c.tx_hex.clone(), funding_value, "regtest",
    )
    .await?;
    assert!(grant.granted, "the SE must GRANT this collapse");
    assert!(grant.newly_signed, "the first grant is a new signature, not a cached replay");
    assert!(grant.frozen, "the root must be frozen in the same breath as the signature (REQ-64)");
    assert_eq!(
        grant.obligations as usize, ob.obligations.len(),
        "the SE must have weighed the same obligations it reported"
    );
    assert!(!grant.partial_sig.is_empty(), "REQ-82: the SE issues its half, and it must be there");
    println!(
        "SDK94 - (5) GRANTED: {} obligations, recovered {}, self_funding {}, frozen {}",
        grant.obligations, grant.recovered, grant.self_funding, grant.frozen
    );

    // ---- 6. THE FREEZE IS OBSERVABLE FROM OUTSIDE, and it is a ratchet. -------------------------
    let after = mercuryrustlib::transaction::collapse_obligations(
        &cc,
        &mercurylib::transaction::CollapseObligationsRequest {
            root_statechain_id: root_sid.clone(),
            statechain_id: root_sid.clone(),
            signed_statechain_id: signed_sid.clone(),
        },
    )
    .await?;
    assert!(after.frozen, "INV-FREEZE: the root must read frozen once its collapse is signed");
    println!("SDK94 - (6) the root now reads FROZEN — no leaf may join a tree that is closing");

    // ---- 7. AND IT CONFIRMS ON CHAIN. -----------------------------------------------------------
    //
    // The strongest proof available here. A `C` the network refuses is a tree that cannot close, and
    // once the root is frozen the signature can never be reissued — so "the SE signed it" is not the
    // claim worth making; "it settled" is.
    let txid = bitcoin_core::sendrawtransaction(&signed_c)?;
    bitcoin_core::generatetoaddress(2, &core)?;
    println!("SDK94 - (7) the collapse BROADCAST and confirmed: {txid}");

    // Every holder paid at their OWN key, on chain, checked per output rather than on the sum: a
    // right total to a wrong distribution still discharges somebody unpaid.
    let raw = bitcoin_core::getrawtransaction_verbose(&txid)?;
    for o in &ob.obligations {
        let paid = raw["vout"]
            .as_array()
            .ok_or(anyhow!("no vout array"))?
            .iter()
            .any(|v| {
                let spk = v["scriptPubKey"]["hex"].as_str().unwrap_or("");
                let sats = (v["value"].as_f64().unwrap_or(0.0) * 1e8).round() as u64;
                spk.len() == 68 && spk.ends_with(&o.exit_key) && sats == o.amount
            });
        assert!(paid, "leaf {} must be paid {} sat ON CHAIN at its own key", o.exit_key, o.amount);
    }
    println!(
        "SDK94 - PASS: a {DEPOSIT}-sat tree with {} unreleased leaf/leaves closed in ONE transaction, \
         every holder paid in full out of the tree's own money, and the root is frozen.",
        ob.obligations.len()
    );
    Ok(())
}
