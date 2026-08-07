//! E2E (SDK_E2E=83) — **THE LEAF COMBINE: N confirmed leaves, ONE transaction, ONE consolidated UTXO.**
//!
//! An in-ladder split makes the final tier a SPINE tier `SP` with one payload output per leaf. The
//! leaves share the prefix `T -> X_m -> SP`; each carries its own tail (`ext_child` + `state_child`).
//! Walking those tails has been the only way a leaf ever reached chain, and it is expensive in every
//! dimension at once — 60 leaves cost prefix 3 084 vB + 60 x 250 vB of tails across 129 transactions
//! and 2 160 blocks of accumulated CSV, and they end as 60 dust UTXOs.
//!
//! **THE STRUCTURAL FACT UNDER TEST.** Once `SP` CONFIRMS, every `SP.out[j]` is an ordinary on-chain
//! P2TR paying that leaf's aggregate key. The owner spends it with a FRESH co-signature carrying NO
//! timelock — it does not have to be the pre-signed extension tier — and N such outputs go in ONE
//! transaction. This test is the first time that claim is exercised against a live SE and a live
//! chain: `mercuryrustlib::combine::combine_leaves` had never been executed anywhere when it landed
//! (its author's own report lists "THE DRIVER HAS NEVER RUN" as residual risk #1), so everything
//! below — N live `sign_first`/`sign_second` round-trips aggregating into a valid multi-input
//! witness, the electrum observation, the journal, the park/un-park interlock and the broadcast — is
//! unproven until this run.
//!
//! ## What is proved, in the order the numbered steps run
//!
//!  1. alice deposits and `claim()` ladders the coin (unconditional laddering, sdk71).
//!  2. alice splits it into FIVE leaves through ONE spine batch (`transfer_many` -> the multi-child
//!     in-ladder route, sdk69): four conveyed to bob, one to carol, plus alice's own change leg.
//!     `SP` is un-broadcast at this point — the whole batch is still off-chain.
//!  3. the shared prefix `T -> X_0 -> SP` is walked ON CHAIN and mined until `SP` is CONFIRMED.
//!  4. every `SP.out[j]` is now a separate, ordinary, unspent on-chain UTXO — asserted through
//!     `combine::observe_leaf`, the same observation the driver itself makes.
//!  5. bob combines THREE of his four leaves into ONE address.
//!  6. exactly ONE new UTXO exists at that address, worth `sum(combined) - fee`; the three leaf
//!     outpoints are spent; carol's leaf and bob's fourth leaf are untouched.
//!  7. bob's un-combined leaf is STILL independently exitable through its own pre-signed tiers —
//!     the safety property the whole feature is conditioned on.
//!
//! ## The negative paths, each as its own numbered step
//!
//!  * **[N1] BLIND IS A REFUSAL.** A combine attempted while `SP` is not on chain at all must be
//!    refused, not waved through. This is the repo's recurring silent-degradation shape ("failure
//!    looks like idle"): the backend cannot answer, and an answer of "0 confirmations, unspent"
//!    would be a fabrication.
//!  * **[N2] THE MEMPOOL WINDOW.** With `SP` broadcast but UNCONFIRMED, the combine must still be
//!    refused: an `SP` in the mempool is replaceable, and replacing it invalidates every
//!    co-signature in the batch at once.
//!  * **[N3] THE COLOURED REFUSAL.** A leaf carrying an RGB allocation is sealed to `SP.out[j]`;
//!    sweeping it in a plain transaction destroys the allocation irrecoverably. Refused on the BUILD
//!    path — in `plan_leaf_combine`, the only function that can mint a `CombinePlan` — and refused
//!    again through the full driver, side-effect free.
//!
//! Run: SDK_E2E=83 ML_NETWORK=regtest cargo +stable run

use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::wallet::{Coin, CoinStatus};
use mercuryrustlib::client_config::ClientConfig;

use crate::bitcoin_core;

/// The only on-chain funding in the whole flow.
const DEPOSIT: u64 = 200_000;

/// bob's four leaves. The first three are combined; `LEAF_KEPT` is deliberately left out so step 7
/// has a leaf to exit through its own tail while its siblings have been swept.
const LEAF_1: u64 = 30_000;
const LEAF_2: u64 = 25_000;
const LEAF_3: u64 = 20_000;
const LEAF_KEPT: u64 = 18_000;
/// carol's leaf — the one bob does NOT own, and which must be untouched by his sweep.
const LEAF_CAROL: u64 = 15_000;

/// Sweep fee rate. 2 sat/vB is the regtest committed rate the ladder itself uses, so the sweep is
/// priced on the same scale as everything else in the run.
const SWEEP_FEE_RATE: f64 = 2.0;
/// One confirmation of `SP` is the precondition. It is what turns `SP.out[j]` from a replaceable
/// mempool output into an ordinary UTXO.
const CONFIRMATION_TARGET: u32 = 1;

/// How many leaves the sweep consumes. Named because three separate assertions are priced off it.
const N_COMBINED: usize = 3;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn v2_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

fn parse_tx(signed_hex: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(signed_hex)?)?)
}

/// Is this txid known to the chain backend at all (mempool or mined)?
///
/// `Err` is NOT folded into `false`: a backend that could not answer has told us nothing, and every
/// caller below uses this to decide whether to keep mining. Reporting "not there" for "could not
/// look" is the exact defect class this test's [N1] step exists to pin.
fn tx_known(cc: &ClientConfig, txid: &str) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let id = Txid::from_str(txid).map_err(|e| anyhow!("bad txid {txid}: {e}"))?;
    match cc.electrum_client.transaction_get_raw(&id) {
        std::result::Result::Ok(_) => Ok(true),
        // electrum answers "missing" for an unknown tx with an error; there is no way to tell that
        // apart from a transport failure at this layer, so the caller must not treat a `false` as
        // authoritative for anything but "keep waiting".
        Err(_) => Ok(false),
    }
}

/// Is `txid:vout` still unspent, per the indexer? Fails CLOSED — an unreadable backend is an error,
/// never a cheerful "yes, still there".
fn outpoint_unspent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let id = Txid::from_str(txid).map_err(|e| anyhow!("bad txid {txid}: {e}"))?;
    let raw = cc
        .electrum_client
        .transaction_get_raw(&id)
        .map_err(|e| anyhow!("{txid} is not readable from the chain backend: {e}"))?;
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw)?;
    let spk = tx
        .output
        .get(vout as usize)
        .ok_or_else(|| anyhow!("{txid} has no output {vout}"))?
        .script_pubkey
        .clone();
    let utxos = cc
        .electrum_client
        .script_list_unspent(&spk)
        .map_err(|e| anyhow!("cannot list unspent for {txid}:{vout}: {e}"))?;
    Ok(utxos.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

/// Every coin a wallet currently holds, read straight from the wallet DB.
async fn coins_of(cc: &ClientConfig, wallet_name: &str) -> Result<Vec<Coin>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins)
}

/// The one coin of `wallet_name` worth exactly `amount` sats (any status).
async fn coin_worth(cc: &ClientConfig, wallet_name: &str, amount: u64) -> Result<Coin> {
    coins_of(cc, wallet_name)
        .await?
        .into_iter()
        .find(|c| c.amount == Some(u32::try_from(amount).unwrap_or(u32::MAX)))
        .ok_or_else(|| anyhow!("{wallet_name} holds no {amount}-sat coin"))
}

/// A FRESH P2TR destination for the sweep, derived from `SP`'s txid so it is unique to this run.
///
/// Freshness is load-bearing twice over. Step 6 asserts "exactly ONE UTXO at this address", which a
/// hardcoded literal would break the second time the suite ran on the same chain; and P2TR (not the
/// mining wallet's bech32 `getnewaddress`) is what makes the sweep's real serialisation comparable
/// to `sweep_tx_vsize`, whose output term is the 43-byte P2TR one.
fn sweep_destination(sp_txid: &str) -> Result<String> {
    use electrum_client::bitcoin::{
        secp256k1::{Secp256k1, SecretKey},
        Address, Network,
    };
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("sdk83-leaf-combine-destination:{sp_txid}").as_bytes());
    let sk = SecretKey::from_slice(&digest)
        .map_err(|e| anyhow!("derived sweep destination key is invalid: {e}"))?;
    let secp = Secp256k1::new();
    let (xonly, _) = sk.public_key(&secp).x_only_public_key();
    Ok(Address::p2tr(&secp, xonly, None, Network::Regtest).to_string())
}

/// Walk a pre-signed tier chain ON CHAIN, mining out each tier's relative timelock, and return the
/// txid of the last tier.
///
/// Used for the SHARED PREFIX only (`T -> X_0 -> SP`). It deliberately stops there rather than
/// calling `exit_child_pass`, which would continue into `ext_child` and spend the very leaf output
/// the combine is about to consume.
async fn broadcast_chain(
    cc: &ClientConfig,
    tiers: &[(String, Option<u16>)],
    core: &str,
    label: &str,
) -> Result<String> {
    let mut last = String::new();
    for (i, (signed_hex, csv)) in tiers.iter().enumerate() {
        let raw = hex::decode(signed_hex)
            .map_err(|e| anyhow!("{label} tier {i} carries unusable signed hex: {e}"))?;
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&raw)?;
        let txid = tx.txid().to_string();
        last = txid.clone();
        if tx_known(cc, &txid)? {
            continue;
        }
        // Retry-with-mining rather than "mine csv blocks then broadcast": BIP-68 counts from the
        // confirmation height of the INPUT, and the input's own confirmation height is not known
        // until it is mined. A rejection here is the chain telling us the timelock is not met yet.
        let mut attempts = 0u32;
        loop {
            match cc.electrum_client.transaction_broadcast_raw(&raw) {
                std::result::Result::Ok(_) => break,
                Err(e) => {
                    attempts += 1;
                    if attempts > 40 {
                        return Err(anyhow!(
                            "{label} tier {i} ({txid}) never relayed after {attempts} attempts \
                             (csv={csv:?}): {e}"
                        ));
                    }
                    bitcoin_core::generatetoaddress(u32::from(csv.unwrap_or(1)).max(1), core)?;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            }
        }
        bitcoin_core::generatetoaddress(1, core)?;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(last)
}

/// Poll `claim()` until `w` books `want` CONFIRMED coins, each with an adopted child bundle.
async fn claim_until_leaves(
    cc: &ClientConfig,
    w: &UtexoWallet,
    name: &str,
    want: usize,
) -> Result<Vec<String>> {
    for _ in 0..60 {
        w.claim().await?;
        let confirmed: Vec<String> = coins_of(cc, name)
            .await?
            .into_iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED)
            .filter_map(|c| c.statechain_id)
            .collect();
        if confirmed.len() >= want {
            // A leaf is a leaf only if it carries the child bundle the in-ladder route mints; a
            // plain V1 sub-coin would have none, and would silently pass a count-only check.
            let mut leaves = Vec::new();
            for sid in confirmed {
                if mercuryrustlib::tesr::load_child(cc, name, &sid).await?.is_some() {
                    leaves.push(sid);
                }
            }
            if leaves.len() >= want {
                return Ok(leaves);
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} did not adopt {want} leaf/leaves"))
}

/// Assert a refusal happened AND that it is the refusal we meant. A negative step that passes on an
/// unrelated parse/address/network error reports a safety it never observed — the D6 strictness
/// discipline sdk58 spells out.
fn refused_with<T: std::fmt::Debug>(r: Result<T>, attack: &str, expect: &str) -> Result<String> {
    match r {
        std::result::Result::Ok(v) => {
            Err(anyhow!("SAFETY: {attack} was ACCEPTED, got {v:?}"))
        }
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains(expect) {
                return Err(anyhow!(
                    "{attack} was refused for the WRONG reason — expected an error containing \
                     {expect:?}, got: {msg}"
                ));
            }
            Ok(msg)
        }
    }
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = v2_wallet("sdk83_alice").await?;
    let bob = v2_wallet("sdk83_bob").await?;
    let carol = v2_wallet("sdk83_carol").await?;

    // =============================================================================================
    // [1] alice deposits and `claim()` LADDERS the coin.
    // =============================================================================================
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let deposit_addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &deposit_addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != DEPOSIT {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("alice's deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_sid = coins_of(&cc, "sdk83_alice")
        .await?
        .into_iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id)
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    let parent_bundle = mercuryrustlib::tesr::load(&cc, "sdk83_alice", &alice_sid)
        .await?
        .ok_or(anyhow!("claim() must ladder a fresh confirmed root coin unconditionally"))?;
    let (f_txid, f_vout) = (parent_bundle.f_txid.clone(), parent_bundle.f_vout);
    println!(
        "SDK83 - [1] alice's {DEPOSIT}-sat coin is LADDERED over F = {f_txid}:{f_vout} \
         ({} exit tiers)",
        parent_bundle.exit_tiers().len()
    );

    // =============================================================================================
    // [2] alice splits into FIVE leaves through ONE spine batch: four to bob, one to carol.
    //
    // Four distinct receive slots for bob, minted with `new_transfer_address`, so each conveyance
    // lands on its own INITIALISED coin through the primary adoption branch. (Address reuse is
    // supported too — `get_utexo_address` documents it — but it routes the 2nd..Nth message through
    // `duplicate_coin_to_initialized_state`, and this test is about the combine, not about that.)
    // =============================================================================================
    let bob_addrs = {
        let mut v = Vec::new();
        for _ in 0..4 {
            v.push(mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk83_bob").await?);
        }
        v
    };
    let carol_addr = carol.get_utexo_address().await?;
    // One derived slot per new statechain coin: 5 pieces + alice's change leg.
    for _ in 0..6 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let results = alice
        .transfer_many(&[
            (bob_addrs[0].clone(), LEAF_1),
            (bob_addrs[1].clone(), LEAF_2),
            (bob_addrs[2].clone(), LEAF_3),
            (bob_addrs[3].clone(), LEAF_KEPT),
            (carol_addr.clone(), LEAF_CAROL),
        ])
        .await?;
    assert_eq!(results.len(), 5, "one result per recipient");
    assert!(
        results.iter().all(|r| r.used_split),
        "all five leaves must come from ONE split — separate transfers would not share a prefix, \
         which is the thing the combine amortises"
    );

    let bob_sids = claim_until_leaves(&cc, &bob, "sdk83_bob", 4).await?;
    let carol_sids = claim_until_leaves(&cc, &carol, "sdk83_carol", 1).await?;
    assert_eq!(bob_sids.len(), 4, "bob adopted four leaves");
    assert_eq!(carol_sids.len(), 1, "carol adopted one leaf");

    // ONE `SP`, one payload output per leaf. Read from bob's first leaf and cross-checked against
    // carol's: a shared prefix is the premise of the whole feature, so it is asserted, not assumed.
    let bob_cb0 = mercuryrustlib::tesr::load_child(&cc, "sdk83_bob", &bob_sids[0])
        .await?
        .ok_or(anyhow!("bob's leaf has no child bundle — transfer_many took the PLAIN split [B1]"))?;
    let carol_cb = mercuryrustlib::tesr::load_child(&cc, "sdk83_carol", &carol_sids[0])
        .await?
        .ok_or(anyhow!("carol's leaf has no child bundle"))?;
    let sp_hex = bob_cb0.parent.current().state.signed_tx.clone();
    assert_eq!(
        sp_hex,
        carol_cb.parent.current().state.signed_tx,
        "every leaf is carved by the SAME split state SP — one transaction, N leaves"
    );
    let sp = parse_tx(&sp_hex)?;
    let sp_txid = sp.txid().to_string();
    // 5 pieces + alice's change + the P2A anchor.
    assert_eq!(sp.output.len(), 7, "SP = 5 leaves + change + P2A anchor");
    assert_eq!(sp.output[6].value, mercurylib::tesr::P2A_VALUE, "the P2A anchor comes last");
    for (j, want) in [LEAF_1, LEAF_2, LEAF_3, LEAF_KEPT, LEAF_CAROL].iter().enumerate() {
        assert_eq!(sp.output[j].value, *want, "SP.out[{j}] pays its leaf its exact amount");
    }
    // SP descends from X_m rather than racing the trigger for F — the [B1] property sdk69 pins, and
    // the reason the prefix can be broadcast at all.
    let x_m = bob_cb0.parent.current().extension.clone();
    assert_eq!(
        sp.input[0].previous_output.txid.to_string(),
        x_m.txid,
        "SP must spend X_m.out[0], never F"
    );
    // Still entirely off-chain at this point: nothing has been broadcast.
    assert!(
        !tx_known(&cc, &sp_txid)?,
        "SP must still be un-broadcast — the split is off-chain until step 3"
    );
    println!(
        "SDK83 - [2] ONE spine batch SP={sp_txid} carved 5 leaves (bob {LEAF_1}/{LEAF_2}/{LEAF_3}/\
         {LEAF_KEPT}, carol {LEAF_CAROL}) + change + P2A, entirely off-chain"
    );

    // The four leaf coins bob will work with, in the order this test names them.
    let leaf_1 = coin_worth(&cc, "sdk83_bob", LEAF_1).await?;
    let leaf_2 = coin_worth(&cc, "sdk83_bob", LEAF_2).await?;
    let leaf_3 = coin_worth(&cc, "sdk83_bob", LEAF_3).await?;
    let leaf_kept = coin_worth(&cc, "sdk83_bob", LEAF_KEPT).await?;
    let carol_leaf = coin_worth(&cc, "sdk83_carol", LEAF_CAROL).await?;
    // Adoption is what populates the four fields the PSBT builder reads per input
    // (transfer_receiver.rs:1506-1508). Asserted here so a later refusal cannot be misread as a
    // combine bug when it is really an adoption bug.
    for (name, c) in [
        ("leaf_1", &leaf_1),
        ("leaf_2", &leaf_2),
        ("leaf_3", &leaf_3),
        ("leaf_kept", &leaf_kept),
    ] {
        assert_eq!(
            c.utxo_txid.as_deref(),
            Some(sp_txid.as_str()),
            "{name} must be funded by SP itself"
        );
        assert!(c.utxo_vout.is_some(), "{name} must name its SP output");
        assert!(c.aggregated_address.is_some(), "{name} must carry its own aggregate address");
        assert!(c.aggregated_pubkey.is_some(), "{name} must carry its own aggregate pubkey");
        assert_eq!(c.status, CoinStatus::CONFIRMED, "{name} is adopted and live");
    }
    let combine_sel_outpoints: Vec<String> = [&leaf_1, &leaf_2, &leaf_3]
        .iter()
        .map(|c| {
            mercuryrustlib::combine::coin_outpoint(c)
                .ok_or_else(|| anyhow!("a leaf has no funding outpoint"))
        })
        .collect::<Result<_>>()?;
    let kept_outpoint = mercuryrustlib::combine::coin_outpoint(&leaf_kept)
        .ok_or(anyhow!("kept leaf has no funding outpoint"))?;
    let carol_outpoint = mercuryrustlib::combine::coin_outpoint(&carol_leaf)
        .ok_or(anyhow!("carol's leaf has no funding outpoint"))?;

    let no_carriers: HashSet<String> = HashSet::new();
    let selection = |cs: &[&Coin]| -> Vec<Coin> { cs.iter().map(|c| (*c).clone()).collect() };

    // Colour evidence is a per-input VERDICT, not a set of positives: the planner demands one for
    // EVERY selected leaf, so a caller that gathered nothing is refused rather than told "nothing
    // is coloured". `carriers` is what `UtexoWallet::unspendable_as_btc_outpoints` returned — the
    // probe below is exactly the wiring a production caller writes, and it cannot turn a failed
    // gather into a `Plain` because that gather already `?`-propagated before we get here.
    let colour_of = |sel: &[Coin],
                     carriers: &HashSet<String>|
     -> Result<mercuryrustlib::combine::ColourEvidence> {
        let ops: Vec<String> = sel
            .iter()
            .map(|c| {
                mercuryrustlib::combine::coin_outpoint(c)
                    .ok_or_else(|| anyhow!("a selected leaf has no funding outpoint"))
            })
            .collect::<Result<_>>()?;
        mercuryrustlib::combine::ColourEvidence::probe::<anyhow::Error, _>(&ops, |op| {
            Ok(if carriers.contains(op) {
                mercuryrustlib::combine::ColourVerdict::Coloured
            } else {
                mercuryrustlib::combine::ColourVerdict::Plain
            })
        })
    };

    // =============================================================================================
    // [N1] BLIND IS A REFUSAL. `SP` is not on chain at all; the driver must refuse rather than
    // manufacture "0 confirmations, unspent" out of a backend that answered nothing.
    // =============================================================================================
    {
        let mut sel = selection(&[&leaf_1, &leaf_2, &leaf_3]);
        let ev = colour_of(&sel, &no_carriers)?;
        let dest = sweep_destination(&sp_txid)?;
        let r = mercuryrustlib::combine::combine_leaves(
            &cc,
            "sdk83_bob",
            &mut sel,
            &ev,
            &dest,
            SWEEP_FEE_RATE,
            CONFIRMATION_TARGET,
        )
        .await;
        let msg = match r {
            std::result::Result::Ok(v) => {
                return Err(anyhow!(
                    "SAFETY: a combine over an SP that is not on chain AT ALL was ACCEPTED: {v:?}"
                ))
            }
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains(&sp_txid),
            "the refusal must name the leaf funding it could not read, got: {msg}"
        );
        assert!(
            msg.contains("cannot tell whether SP is confirmed")
                || msg.contains("below the required"),
            "an unreadable/absent SP must be refused as BLIND or as unconfirmed — never accepted \
             and never reported as an ordinary empty result, got: {msg}"
        );
        // Side-effect free: nothing parked, nothing signed.
        for (name, sid) in [("leaf_1", &leaf_1), ("leaf_2", &leaf_2), ("leaf_3", &leaf_3)]
            .map(|(n, c)| (n, c.statechain_id.clone().unwrap()))
        {
            let now = coins_of(&cc, "sdk83_bob")
                .await?
                .into_iter()
                .find(|c| c.statechain_id.as_deref() == Some(sid.as_str()))
                .ok_or(anyhow!("{name} vanished"))?;
            assert_eq!(
                now.status,
                CoinStatus::CONFIRMED,
                "{name} must be left exactly as it was found — a refusal that parks coins would \
                 stand the watchtower down for a sweep that never happened"
            );
        }
        println!("SDK83 - [N1] a combine over an un-broadcast SP is REFUSED (blind, fail-closed): {msg}");
    }

    // =============================================================================================
    // [3] Broadcast the shared prefix `T -> X_0 -> SP` and mine until `SP` is CONFIRMED.
    //
    // `parent.exit_tiers()` is exactly that prefix: the trigger, every level's extension, and the
    // level's state — which the split replaced with `SP` (tesr.rs: `parent_seg.levels[last].state`).
    // Walking it stops AT `SP`; the child tails are deliberately not touched.
    // =============================================================================================
    let prefix: Vec<(String, Option<u16>)> = bob_cb0
        .parent
        .exit_tiers()
        .iter()
        .map(|t| (t.signed_tx.clone(), t.csv))
        .collect();
    assert_eq!(
        prefix.last().map(|(hex, _)| parse_tx(hex).map(|t| t.txid().to_string())).transpose()?,
        Some(sp_txid.clone()),
        "the prefix must END at SP — if it ends anywhere else this test is walking the wrong chain"
    );

    // [N2] THE MEMPOOL WINDOW. Broadcast every tier BUT the last, then put `SP` in the mempool and
    // attempt the combine before it is mined.
    let (_, before_sp) = prefix.split_last().expect("the prefix has at least the trigger");
    broadcast_chain(&cc, before_sp, &core, "prefix-before-SP").await?;
    let sp_raw = hex::decode(&sp_hex)?;
    {
        let mut attempts = 0u32;
        loop {
            match cc.electrum_client.transaction_broadcast_raw(&sp_raw) {
                std::result::Result::Ok(_) => break,
                Err(e) => {
                    attempts += 1;
                    if attempts > 40 {
                        return Err(anyhow!("SP never relayed after {attempts} attempts: {e}"));
                    }
                    bitcoin_core::generatetoaddress(1, &core)?;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            }
        }
    }
    // Wait for the indexer to SEE the mempool SP before making the claim about it. Without this the
    // step is a race: an un-indexed output makes `observe_leaf` report `unspent: false`, which is the
    // `LeafAlreadySpent` refusal — a pass for the wrong reason, and the sort of green that means
    // nothing. The state this step is about is specifically "present, zero confirmations".
    {
        let mut visible = false;
        for _ in 0..30 {
            let tip = cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
            if let std::result::Result::Ok(o) = mercuryrustlib::combine::observe_leaf(
                &cc.electrum_client,
                &combine_sel_outpoints[0],
                tip,
            ) {
                if o.unspent && o.confirmations == 0 {
                    visible = true;
                    break;
                }
                // Already confirmed: the mempool window closed before we could look at it. Nothing
                // was mined by this test between the broadcast and here, so this should not happen —
                // say so rather than silently skipping the step.
                assert_eq!(
                    o.confirmations, 0,
                    "SP confirmed before the mempool-window step could run; [N2] proved nothing"
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(
            visible,
            "the indexer never reported the mempool SP's outputs — [N2] cannot distinguish \
             'unconfirmed' from 'not indexed yet', so it is not run rather than passed by accident"
        );
    }
    {
        let mut sel = selection(&[&leaf_1, &leaf_2, &leaf_3]);
        let ev = colour_of(&sel, &no_carriers)?;
        let dest = sweep_destination(&sp_txid)?;
        let msg = refused_with(
            mercuryrustlib::combine::combine_leaves(
                &cc,
                "sdk83_bob",
                &mut sel,
                &ev,
                &dest,
                SWEEP_FEE_RATE,
                CONFIRMATION_TARGET,
            )
            .await,
            "a combine while SP is still in the MEMPOOL",
            "below the required",
        )?;
        assert!(
            msg.contains("mempool can be replaced"),
            "the refusal must say WHY a mempool SP is not good enough, got: {msg}"
        );
        println!("SDK83 - [N2] a combine over a MEMPOOL SP is REFUSED: {msg}");
    }

    // Now mine it in.
    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut confirmed = false;
    for _ in 0..30 {
        if tx_known(&cc, &sp_txid)? {
            let tip = cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
            let o = mercuryrustlib::combine::observe_leaf(
                &cc.electrum_client,
                &combine_sel_outpoints[0],
                tip,
            )?;
            if o.confirmations >= CONFIRMATION_TARGET {
                confirmed = true;
                break;
            }
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(confirmed, "SP did not reach {CONFIRMATION_TARGET} confirmation(s)");
    assert!(
        !outpoint_unspent(&cc, &f_txid, f_vout)?,
        "the prefix is on chain, so F is now spent by the trigger"
    );
    println!("SDK83 - [3] the shared prefix T -> X_m -> SP is ON CHAIN and SP is CONFIRMED");

    // =============================================================================================
    // [4] Every `SP.out[j]` is now a separate, ordinary, unspent on-chain UTXO.
    // =============================================================================================
    let tip = cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
    let all_leaf_outpoints: Vec<String> = combine_sel_outpoints
        .iter()
        .cloned()
        .chain([kept_outpoint.clone(), carol_outpoint.clone()])
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    for op in &all_leaf_outpoints {
        let o = mercuryrustlib::combine::observe_leaf(&cc.electrum_client, op, tip)?;
        assert!(o.unspent, "leaf {op} must be an unspent on-chain UTXO after SP confirms");
        assert!(
            o.confirmations >= CONFIRMATION_TARGET,
            "leaf {op} has {} confirmation(s)",
            o.confirmations
        );
        assert!(seen.insert(op.clone()), "the leaves must be SEPARATE outputs, {op} repeated");
    }
    assert_eq!(seen.len(), 5, "five leaves, five distinct on-chain UTXOs");
    println!("SDK83 - [4] all 5 SP.out[j] exist on chain as separate, unspent, confirmed UTXOs");

    // =============================================================================================
    // [N3] THE COLOURED REFUSAL — on the BUILD path, not in a helper a caller can walk around.
    //
    // The evidence, not the paint: `plan_leaf_combine` refuses on membership of the carrier set the
    // caller is REQUIRED to supply (the audit P0-5 shape, moved onto the build path). This step
    // supplies a set naming a real leaf — exactly what `unspendable_as_btc_outpoints` would return
    // if that leaf carried an allocation — and proves the refusal fires there and through the full
    // driver, side-effect free. Painting a leaf for real needs the CTES-R coloured split (sdk77);
    // the plain lane cannot mint one, and it would not change which branch is under test.
    // =============================================================================================
    {
        let mut coloured: HashSet<String> = HashSet::new();
        coloured.insert(combine_sel_outpoints[0].clone());

        // (a) an ALL-coloured selection -> ColouredLeaf, naming the leaf and the destruction.
        let one = selection(&[&leaf_1]);
        let msg = refused_with(
            mercuryrustlib::combine::plan_leaf_combine(&one, &colour_of(&one, &coloured)?, SWEEP_FEE_RATE)
                .map_err(|r| anyhow!("{r}")),
            "planning a combine over a COLOURED leaf",
            "carries an RGB allocation",
        )?;
        assert!(
            msg.contains("destroys it irrecoverably"),
            "the refusal must say what is lost, got: {msg}"
        );
        println!("SDK83 - [N3a] a coloured leaf is REFUSED on the build path: {msg}");

        // (b) a MIXED selection is its own, differently-named mistake — silently dropping the
        //     coloured leaf would sweep something other than what was asked for.
        let mixed = selection(&[&leaf_1, &leaf_2, &leaf_3]);
        let msg = refused_with(
            mercuryrustlib::combine::plan_leaf_combine(&mixed, &colour_of(&mixed, &coloured)?, SWEEP_FEE_RATE)
                .map_err(|r| anyhow!("{r}")),
            "planning a MIXED coloured/plain combine",
            "MIXES",
        )?;
        println!("SDK83 - [N3b] a mixed selection is REFUSED as mixed: {msg}");

        // (c) the same refusal through the FULL driver — the path a caller actually takes — and it
        //     must be side-effect free: no park, no signature, no broadcast.
        let mut sel = selection(&[&leaf_1, &leaf_2, &leaf_3]);
        let ev = colour_of(&sel, &coloured)?;
        let dest = sweep_destination(&sp_txid)?;
        let msg = refused_with(
            mercuryrustlib::combine::combine_leaves(
                &cc,
                "sdk83_bob",
                &mut sel,
                &ev,
                &dest,
                SWEEP_FEE_RATE,
                CONFIRMATION_TARGET,
            )
            .await,
            "combining a selection containing a coloured leaf",
            "MIXES",
        )?;
        for (i, op) in combine_sel_outpoints.iter().enumerate() {
            let (txid, vout) = op.split_once(':').ok_or(anyhow!("malformed outpoint {op}"))?;
            assert!(
                outpoint_unspent(&cc, txid, vout.parse()?)?,
                "leaf {i} ({op}) must be untouched by a REFUSED combine"
            );
        }
        for (name, c) in [("leaf_1", &leaf_1), ("leaf_2", &leaf_2), ("leaf_3", &leaf_3)] {
            let sid = c.statechain_id.clone().unwrap();
            let now = coins_of(&cc, "sdk83_bob")
                .await?
                .into_iter()
                .find(|x| x.statechain_id.as_deref() == Some(sid.as_str()))
                .ok_or(anyhow!("{name} vanished"))?;
            assert_eq!(
                now.status,
                CoinStatus::CONFIRMED,
                "{name} must be left CONFIRMED by a refused combine — a coin parked for a sweep \
                 that never happened is a coin the watchtower has stopped defending"
            );
        }
        println!("SDK83 - [N3c] the driver refuses it too, side-effect free: {msg}");
    }

    // =============================================================================================
    // [5] THE COMBINE. Three of bob's four leaves into ONE address.
    // =============================================================================================
    let destination = sweep_destination(&sp_txid)?;
    let dest_spk = electrum_client::bitcoin::Address::from_str(&destination)?
        .require_network(electrum_client::bitcoin::Network::Regtest)?
        .script_pubkey();
    assert!(
        cc.electrum_client.script_list_unspent(&dest_spk)?.is_empty(),
        "the sweep destination must start EMPTY, or step 6's \"exactly one new UTXO\" proves nothing"
    );

    let combined_in: u64 = LEAF_1 + LEAF_2 + LEAF_3;
    let expected_vsize = mercurylib::transaction::sweep_tx_vsize(N_COMBINED, 1)
        .map_err(|e| anyhow!("fee model refused to size the sweep: {e:?}"))?;
    let expected_fee = mercurylib::transaction::sweep_fee_sats(N_COMBINED, 1, SWEEP_FEE_RATE)
        .map_err(|e| anyhow!("fee model refused to price the sweep: {e:?}"))?;

    let mut sel = selection(&[&leaf_1, &leaf_2, &leaf_3]);
    let ev = colour_of(&sel, &no_carriers)?;
    let outcome = mercuryrustlib::combine::combine_leaves(
        &cc,
        "sdk83_bob",
        &mut sel,
        &ev,
        &destination,
        SWEEP_FEE_RATE,
        CONFIRMATION_TARGET,
    )
    .await
    .map_err(|e| anyhow!("the leaf combine FAILED: {e}"))?;
    assert_eq!(outcome.outpoints.len(), N_COMBINED, "one input per combined leaf");
    assert_eq!(outcome.outpoints, combine_sel_outpoints, "the sweep spends exactly the selection");
    assert_eq!(outcome.fee_sats, expected_fee, "the fee is the model's, not an ad-hoc number");
    assert_eq!(
        outcome.output_sats,
        combined_in - expected_fee,
        "the sweep pays sum(leaves) - fee"
    );
    println!(
        "SDK83 - [5] combine BROADCAST: {} leaves ({combined_in} sats) -> {} sats at {destination} \
         in tx {} (fee {} sats @ {SWEEP_FEE_RATE} sat/vB)",
        N_COMBINED, outcome.output_sats, outcome.txid, outcome.fee_sats
    );

    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // =============================================================================================
    // [6] EXACTLY ONE new UTXO, worth sum(combined) - fee; the leaves bob did not combine, and the
    //     leaf he never owned, are untouched.
    // =============================================================================================
    let mut swept = Vec::new();
    for _ in 0..30 {
        swept = cc.electrum_client.script_list_unspent(&dest_spk)?;
        if !swept.is_empty() {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        swept.len(),
        1,
        "a combine must end as exactly ONE consolidated UTXO — that is the whole point; got {}",
        swept.len()
    );
    assert_eq!(
        swept[0].tx_hash.to_string(),
        outcome.txid,
        "the consolidated UTXO belongs to the sweep this call reported"
    );
    assert_eq!(
        swept[0].value,
        combined_in - expected_fee,
        "the consolidated UTXO is worth sum(combined leaves) - the computed fee"
    );
    assert!(swept[0].height > 0, "the sweep is mined, not merely in the mempool");

    // The three combined leaves are spent...
    for op in &combine_sel_outpoints {
        let (txid, vout) = op.split_once(':').ok_or(anyhow!("malformed outpoint {op}"))?;
        assert!(
            !outpoint_unspent(&cc, txid, vout.parse()?)?,
            "combined leaf {op} must now be SPENT"
        );
    }
    // ...and nothing else is. bob's fourth leaf and carol's leaf are ordinary siblings on the same
    // `SP`; a sweep that touched them would be spending value it was never handed.
    for (whose, op) in [("bob's un-combined", &kept_outpoint), ("carol's", &carol_outpoint)] {
        let (txid, vout) = op.split_once(':').ok_or(anyhow!("malformed outpoint {op}"))?;
        assert!(
            outpoint_unspent(&cc, txid, vout.parse()?)?,
            "{whose} leaf {op} must be UNTOUCHED by the combine"
        );
    }
    assert_eq!(
        carol.get_balance().await?.available_sats,
        LEAF_CAROL,
        "carol's balance is unchanged by a sweep she was not part of"
    );

    // THE INTERLOCK, observed. The combined leaves must be out of the watchtower's CONFIRMED
    // allowlist: driving the extension tier of a leaf whose funding output is spent would be
    // broadcasting a transaction that can never confirm.
    for (name, c) in [("leaf_1", &leaf_1), ("leaf_2", &leaf_2), ("leaf_3", &leaf_3)] {
        let sid = c.statechain_id.clone().unwrap();
        let now = coins_of(&cc, "sdk83_bob")
            .await?
            .into_iter()
            .find(|x| x.statechain_id.as_deref() == Some(sid.as_str()))
            .ok_or(anyhow!("{name} vanished"))?;
        assert_ne!(
            now.status,
            CoinStatus::CONFIRMED,
            "{name} was swept — leaving it in the tower's CONFIRMED allowlist would let \
             defend_ladders broadcast a tier over an output that no longer exists"
        );
    }
    let kept_now = coins_of(&cc, "sdk83_bob")
        .await?
        .into_iter()
        .find(|x| x.statechain_id == leaf_kept.statechain_id)
        .ok_or(anyhow!("the kept leaf vanished"))?;
    assert_eq!(
        kept_now.status,
        CoinStatus::CONFIRMED,
        "the leaf that was NOT combined must still be live and defended"
    );
    println!(
        "SDK83 - [6] exactly ONE UTXO of {} sats at the destination; 3 leaves spent; bob's 4th and \
         carol's leaf untouched; the swept leaves are out of the tower's allowlist",
        swept[0].value
    );

    // ---- [6b] THE FEE MODEL AGAINST THE TRANSACTION IT PRICED. ----------------------------------
    //
    // The model must never UNDER-state the transaction it prices: an under-paying sweep of N
    // already-co-signed leaves is the "looks fine, never confirms" failure, and by then the
    // signatures are spent. Measured against the real serialised sweep, not against itself.
    let signed = parse_tx(&outcome.signed_tx)?;
    let actual_vsize = signed.vsize() as u64;
    assert_eq!(signed.input.len(), N_COMBINED, "one input per leaf, in the built transaction");
    assert_eq!(signed.output.len(), 1, "one output — the consolidated UTXO");
    assert_eq!(signed.lock_time.to_consensus_u32(), 0, "a combine carries NO absolute timelock");
    assert!(
        signed.input.iter().all(|i| i.sequence.to_consensus_u32() == 0),
        "a combine carries NO relative timelock — it spends ordinary confirmed P2TR outputs"
    );
    assert!(
        actual_vsize <= expected_vsize,
        "THE FEE MODEL UNDER-STATES THE SWEEP: sweep_tx_vsize({N_COMBINED}, 1) = {expected_vsize} \
         vB but the co-signed transaction serialises to {actual_vsize} vB. \
         `new_backup_transaction_multi` builds `taproot::Signature {{ hash_ty: TapSighashType::All }}`, \
         and rust-bitcoin's `Signature::to_vec` APPENDS the sighash byte for every type except \
         `Default` — so each key-path witness is 1 + 1 + 65 = 67 bytes, not the 66 \
         `INPUT_WITNESS_BYTES` assumes. Under-pricing a batch whose N signatures are already spent \
         is exactly the failure the ceil() rounding was added to avoid."
    );
    assert!(
        expected_vsize - actual_vsize <= 2,
        "the fee model must be TIGHT, not padded: modelled {expected_vsize} vB vs actual \
         {actual_vsize} vB"
    );
    let paid: u64 = combined_in - swept[0].value;
    assert_eq!(paid, outcome.fee_sats, "the fee actually paid is the fee that was quoted");
    assert!(
        (paid as f64) / (actual_vsize as f64) >= 1.0,
        "the sweep must clear the 1 sat/vB minimum relay rate: {paid} sats over {actual_vsize} vB"
    );
    println!(
        "SDK83 - [6b] fee model vs reality: modelled {expected_vsize} vB, actual {actual_vsize} vB, \
         paid {paid} sats ({:.3} sat/vB effective)",
        (paid as f64) / (actual_vsize as f64)
    );

    // =============================================================================================
    // [7] THE SAFETY PROPERTY: a leaf that was NOT combined is still independently exitable.
    //
    // Its siblings have been swept out from under the same `SP`, and its own tail (`ext_child` ->
    // `state_child`) was co-signed long before the combine existed. The exit needs NO fresh SE
    // signature — `exit_child_pass` is not even `async` and has no `ClientConfig` (tesr.rs:5356) —
    // so nothing the combine consumed can brick it.
    // =============================================================================================
    let kept_sid = leaf_kept.statechain_id.clone().ok_or(anyhow!("kept leaf has no sid"))?;
    let mut passes = 0;
    loop {
        let st = bob.unilateral_exit(Some(vec![kept_sid.clone()]), None).await?;
        let s = st.into_iter().next().ok_or(anyhow!("no exit status for the kept leaf"))?;
        if s.complete {
            println!("SDK83 - [7] bob's un-combined leaf EXITED unilaterally after {passes} pass(es)");
            break;
        }
        bitcoin_core::generatetoaddress(u32::from(s.wait_blocks).max(1) + 1, &core)?;
        passes += 1;
        if passes > 40 {
            return Err(anyhow!(
                "the un-combined leaf did not exit — a combine that strands its siblings is a \
                 combine that must not ship"
            ));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    // Its funding output is now spent by its OWN extension tier, not by anyone's sweep.
    {
        let (txid, vout) = kept_outpoint.split_once(':').ok_or(anyhow!("malformed outpoint"))?;
        assert!(
            !outpoint_unspent(&cc, txid, vout.parse()?)?,
            "the exited leaf's funding output is consumed by its own tail"
        );
    }
    // And carol's leaf — never bob's to spend — is STILL untouched at the end of all of it.
    {
        let (txid, vout) = carol_outpoint.split_once(':').ok_or(anyhow!("malformed outpoint"))?;
        assert!(
            outpoint_unspent(&cc, txid, vout.parse()?)?,
            "carol's leaf survives both the combine and a sibling's unilateral exit"
        );
    }

    println!(
        "SDK83 - SUCCESS: {N_COMBINED} confirmed leaves swept into ONE {}-sat UTXO in a single \
         transaction with no CSV; the un-combined sibling still exited on its own pre-signed tiers; \
         the leaf bob never owned was never touched.",
        swept[0].value
    );
    Ok(())
}
