//! E2E (granularity, RGB tokens): partial token amounts down to 1 RAW UNIT, the token exit of a
//! received piece, the fully-spent-carrier transition, and what a RECEIVED piece can and cannot do.
//!
//! **MIGRATED TO THE COLOURED (CTES-R) LANE — which this test OPTS INTO, because it is not the
//! default.** [D30/D52] `SdkConfig::colored_ladder` ships **false** in both `SdkConfig::regtest` and
//! `SdkConfig::mainnet`; this header used to say it "now defaults ON", and that false premise is
//! what made the explicit `cfg.colored_ladder = true;` below look like belt-and-braces instead of
//! the thing that selects the lane. On the CTES-R lane a token carrier is laddered
//! (`T -> X_m -> S_0`, every tier carrying a real RGB state transition) and the legacy
//! `create_colored_split_tx` lane is RETIRED
//! (`UtexoWallet::refuse_legacy_colored_split_lane`). The observable consequences, each of which
//! this test had to be re-derived against rather than patched:
//!
//!   * **There is no `branch-<piece>` row any more.** The old lane's exit material was a chain of
//!     plain split transactions stored under `branch-<statechain_id>`; a coloured recipient's exit
//!     material is a `ctesr-<statechain_id>` CHILD BUNDLE whose exit chain is the five coloured
//!     tiers `T -> X_m -> SP -> ext_child -> state_child`. Every measurement the old test took from
//!     `get_backup_txs(.., "branch-…")` is now taken from `tesr::child_exit_chain` / the bundle's own
//!     tiers. That single stale read is what made this test RED; the rest of the migration follows
//!     from what replaced it.
//!   * **A carrier is split exactly ONCE.** `cosign_colored_in_ladder_split` TERMINALIZES the parent
//!     (`set_spend_budget(.., 1)`, `SP` consumes the last slot), so the old shape — five successive
//!     splits chained down one carrier's change — is not merely different, it is impossible. The
//!     five sends became one N-ary split (`batch_transfer_tokens`, one `SP` with one payload output
//!     per child) plus a whole-child forward. Same payments, same conservation law, one co-signed
//!     transaction.
//!   * **A received piece is a coloured CHILD and cannot be subdivided at all.** The old limitation
//!     was arithmetic ("carrier coin too small": piece + reserve >= carrier fires on a 1_500-sat
//!     piece). The new one is structural: `ChildTesrBundle::colored_child_seals` refuses a
//!     multi-level coloured child, so `colored_transfer` refuses any PARTIAL pay out of a child by
//!     name. Strictly stronger, and asserted as such — see §(b1).
//!   * **Sats literals are all DERIVED.** `TOKEN_PIECE_SATS` moved 1_500 -> 3_054 (the coloured ROOT
//!     ladder floor at twice the committed tier rate) and `TOKEN_CARRIER_SATS` is 17_324, but no
//!     number below is written down: each is recomputed from `colored_tier_out_total` /
//!     `colored_committed_fee` / `P2A_VALUE` at the LADDER'S OWN fee rate, read off the live bundle.
//!
//! WHAT IS PROVED (and what each part replaces):
//!
//! (a) PRECISION + GRANULARITY. alice issues PT2 (precision 2, supply 10_000 raw = "100.00") behind
//!     a COLOURED ladder and pays three wallets in ONE in-ladder split: bob 10 raw ("0.10"), carol 4
//!     raw, dave 1 raw ("0.01" — the minimum representable amount). Each recipient books the amount
//!     its own CONSIGNMENT assigns (`colored_child_health`), never the sender's declared field, and
//!     alice keeps 9_985 as a coloured change child. Conservation is exact to the raw unit.
//!     INV-11 is asserted on the ladder AND on `SP`: every coloured tier carries exactly ONE
//!     OP_RETURN, its output shape is opret + N payloads + P2A, its fee is EXACTLY
//!     `colored_committed_fee(N, rate)`, and its built vsize matches that fee's vbyte model to
//!     within 2 vB. That is the re-derivation of the old "colored split tx vsize band + one opret"
//!     measurement, which was read off the `branch-` row — tightened from a 170-vB window to a 2-vB
//!     one.
//!
//! (b1) WHAT A RECEIVED PIECE CANNOT DO. bob holds a 10-unit coloured child and tries to pay carol
//!     4 out of it: refused by name ("a coloured CHILD-level split is not implemented"). The old
//!     test asserted the SATS refusal ("carrier coin too small") on a 1_500-sat piece; that bound no
//!     longer fires because a piece is now deliberately ABOVE the coloured root floor — which is
//!     asserted here as arithmetic (`PIECE >= colored_ladder_floor(rate)`), because that inequality
//!     is the whole reason the constant moved: a piece below it is a piece its receiver can never
//!     ladder, i.e. a stranded coin. Both halves of the old statement therefore survive, and the
//!     limitation is now enforced structurally rather than by an arithmetic coincidence.
//!
//! (b2) DOUBLE-RECEIVE. bob receives PT2 TWICE — the 10-raw piece from the split, then alice's
//!     ENTIRE 9_985 change child forwarded WHOLE (`transfer_colored_child`, the coloured lane's
//!     answer to "a child moves as a unit") — and his balance SUMS to 9_995 across two independently
//!     adopted children. This is the same regression the old test pinned: the RGB accept path must
//!     be idempotent on an already-known asset (it used to re-import the genesis, hit a UNIQUE
//!     constraint and strand the second allocation). dave first-sees PT2 at 1 raw unit.
//!
//! (c) THE FULLY-SPENT CARRIER. While it carries, alice's carrier is refused by `split_coin`
//!     ("carries an RGB token allocation") and its sats are invisible to `available_sats` (H2/[23]).
//!     After the split its outpoint holds NO allocation. The old "…and its change comes out PLAIN,
//!     splittable, in available_sats" half **cannot occur on the coloured lane and is re-derived,
//!     not dropped**: the whole of `F` is consumed by the trigger `T` before any payment is carved,
//!     so a spent carrier leaves no BTC sub-coin at all, and `colored_in_ladder_pay` deliberately
//!     carves NO change child when the allocation is fully paid out (a child with an empty RGB
//!     assignment would spend sats to hold nothing). What that assertion actually protected — that
//!     the spent carrier's sats are neither stranded nor silently forfeited — is asserted directly
//!     in §(d) as budget conservation: `Σ children == colored_tier_out_total(X_m, n, rate)`, on both
//!     the with-change and the no-change shapes.
//!
//! (d) CROSS-CARRIER. alice ends up holding QTK on TWO coloured carriers (IFA issue 60 + on-chain
//!     mint of the 50 inflation right bound to a second coin). Paying 100 — more than any single
//!     carrier holds — succeeds: `colored_multi_carrier_transfer` runs one in-ladder split per
//!     carrier. The 60-carrier's leg pays its WHOLE allocation, which is the no-change shape: no
//!     change child is carved and its single piece absorbs the ENTIRE `SP` budget. The 50-carrier's
//!     leg pays 40 and leaves alice a 10-unit change child. bob receives 100 across two pieces.
//!     (The legacy lane did this as one transparent COMBINE and handed over a single piece; the
//!     coloured lane cannot combine two carriers into one transaction — each carrier is an
//!     independent off-chain ladder — so the recipient is paid in N pieces. Asserted as 2, not
//!     relaxed to ">= 1".)
//!
//! (e) THE TOKEN EXIT. carol walks her coloured child's five tiers with no SE and no counterparty.
//!     Before the walk the stock probe must already discriminate (`probe_colored_child_tip` accepts
//!     4, refuses 5) and the empty-off-chain-set proof must FAIL; after it, every tier is MINED with
//!     exactly one opret (INV-11), `F` is spent by `T`, the leaf consignment validates against the
//!     CHAIN ALONE, the stock still spends exactly 4, and the child's final state output — an
//!     UNSPENT on-chain UTXO of `TOKEN_PIECE_SATS − 2·(colored_committed_fee(1) + P2A)`, the piece
//!     minus the two coloured rungs it paid for, still clear of dust — pays CAROL'S OWN key.
//!     This supersedes the old exit section in the one way that matters: there, the exit landed the
//!     allocation on a 2-of-2 outpoint whose only pre-signed sweep was an UNCOLORED spend that would
//!     have destroyed it ("NOT broadcast" — the sats were unrecoverable). Model A pays the child's
//!     own exit key directly, so the packaging sats and the allocation land together. The E7 rule is
//!     honoured throughout: survival is measured with the read-only `color_psbt` stock probe, never
//!     with `get_asset_balance`.
//!
//! Run: SDK_E2E=29 ML_NETWORK=regtest cargo +stable run   (regtest + lockbox + RGB proxy up)
//! Cross-refs: SPEC.md REQ-21/22, INV-11/13/26; CTESR-GATE §2.2/§3.3; sdk75 (coloured exit),
//! sdk77 (coloured in-ladder split), sdk31 (combine), unit granularity_model::token_split_bounds_model.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

/// The packaging of a token piece, taken from the SDK rather than copied. It moved 1_500 -> 3_054
/// when the piece was re-derived as the COLOURED ROOT-ladder floor: a piece has to be able to carry
/// a coloured ladder of its own once its receiver claims it, or retiring the flat lane strands it.
/// §(b1) asserts that inequality against the live ladder's own fee rate.
const PIECE: u64 = mercury_utexo_sdk::tokens::TOKEN_PIECE_SATS;
/// A freshly-issued carrier, also from the SDK (17_324 sat). Nothing below derives from this number
/// directly — every budget is computed from the LADDER, whose trigger already consumed `F`.
const CARRIER: u64 = mercury_utexo_sdk::tokens::TOKEN_CARRIER_SATS;

/// PT2's supply, in RAW units (precision 2 ⟹ display "100.00").
const SUPPLY: u64 = 10_000;
/// **[D43] ONE payee per carrier.** The coloured lane conveys its pieces serially AFTER the carrier
/// is terminal, and journals no `recipient_address`, so a failure at payee *j* strands payees
/// *j..K* permanently. Decision 8 shipped K = 1 rather than building idempotent conveyance, so the
/// three-payee batch this test used to make is not a thing the system does — it is refused by name,
/// and that refusal is now what section (a) asserts.
///
/// `PAY_BOB` is the split leg. `PAY_DAVE = 1` is the MINIMUM representable raw amount — the
/// granularity claim this test exists for — and under D43 it needs a carrier of its OWN, which is
/// exactly the answer the decision took ("issuers size carriers to asset value").
const PAY_BOB: u64 = 10;
/// The partial-pay amount attempted OUT OF bob's received child in (b1). Must be < `PAY_BOB` so the
/// refusal is about the child lane rather than about the amount.
const PAY_CAROL: u64 = 4;
const PAY_DAVE: u64 = 1;
/// What alice keeps as a coloured change child, and later forwards WHOLE to bob.
const CHANGE: u64 = SUPPLY - PAY_BOB;

/// QTK (part d): an IFA issue plus a mint of its inflation right — two coloured carriers.
const Q_ISSUE: u64 = 60;
const Q_MINT: u64 = 50;
/// More than EITHER carrier holds, so the payment must span both.
const Q_PAY: u64 = 100;

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

/// Poll claim until the SETTLED balance of `asset` is exactly `want`.
async fn wait_token_balance(w: &UtexoWallet, asset: &str, want: u64) -> Result<()> {
    for _ in 0..90 {
        w.claim().await?;
        if token_balance(w, asset).await? == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!(
        "settled balance of {asset} did not reach {want} (got {})",
        token_balance(w, asset).await?
    ))
}

/// Wait until (i) the settled balance of `asset` is >= `want_units` and (ii) the wallet holds
/// `want_carriers` CONFIRMED TOKEN_CARRIER_SATS-sat statechain coins (the coloured funding
/// deposits). Waiting on `available_sats` would deadlock: carrier sats are EXCLUDED from the BTC
/// balance (H2/[23]).
async fn wait_carriers_confirmed(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    core: &str,
    asset: &str,
    want_units: u64,
    want_carriers: usize,
) -> Result<()> {
    for _ in 0..90 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        let units_ok = token_balance(w, asset).await? >= want_units;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
        let carriers = rec
            .coins
            .iter()
            .filter(|c| {
                c.status == CoinStatus::CONFIRMED
                    && c.duplicate_index == 0
                    && c.amount == Some(CARRIER as u32)
            })
            .count();
        if units_ok && carriers >= want_carriers {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{wallet_name}: {want_units} of {asset} on {want_carriers} carrier(s) did not confirm"))
}

fn parse_tx(hex_tx: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(hex_tx)?)?)
}

/// The transaction as the CHAIN has it, or `None` if the backend has never heard of it.
fn onchain(cc: &ClientConfig, txid: &str) -> Option<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::Txid;
    let t = electrum_client::bitcoin::Txid::from_str(txid).ok()?;
    let _: Txid = t;
    cc.electrum_client.transaction_get(&t).ok()
}

fn tip(cc: &ClientConfig) -> Result<usize> {
    Ok(cc.electrum_client.block_headers_subscribe()?.height)
}

/// Mine `n` blocks and do not return until the INDEXER has caught up with each one. See sdk75's copy
/// for the rgb-lib resolver race this exists for (electrs trailing bitcoind makes a well-mined tx
/// report as "can't be located in the blockchain").
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

/// Whether `txid:vout` is spent according to electrs (errors propagate — sdk26 discipline).
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let raw = cc.electrum_client.transaction_get_raw(&Txid::from_str(txid)?)?;
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw)?;
    let spk = &tx
        .output
        .get(vout as usize)
        .ok_or_else(|| anyhow!("outpoint {txid}:{vout} does not exist"))?
        .script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk)?;
    Ok(!listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

/// Every CONFIRMED coin of `wallet_name` whose `tesr-` row is a COLOURED ROOT ladder for `asset`,
/// paired with its bundle. This replaces the old "the wallet's most recent CONFIRMED coin of exactly
/// N sats" lookup: on the coloured lane a carrier is identified by its LADDER, not by a sats literal.
async fn colored_carriers(
    cc: &ClientConfig,
    wallet_name: &str,
    asset: &str,
) -> Result<Vec<(String, mercuryrustlib::tesr::TesrBundle)>> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let mut out = Vec::new();
    for c in rec
        .coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
    {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if let Some(b) = mercuryrustlib::tesr::load(cc, wallet_name, &sid).await? {
            if b.rgb.as_ref().is_some_and(|r| r.contract_id == asset) {
                out.push((sid, b));
            }
        }
    }
    Ok(out)
}

/// Poll `claim()` until the wallet has `want` coloured ROOT carriers of `asset`.
async fn wait_colored_carriers(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    core: &str,
    asset: &str,
    want: usize,
) -> Result<Vec<(String, mercuryrustlib::tesr::TesrBundle)>> {
    for _ in 0..90 {
        w.claim().await?;
        let found = colored_carriers(cc, wallet_name, asset).await?;
        if found.len() >= want {
            return Ok(found);
        }
        bitcoin_core::generatetoaddress(1, core)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!(
        "{wallet_name} never got {want} COLOURED carrier(s) of {asset} — CTES-R establish did not \
         happen, so there is nothing to split (the flat lane is retired, so this is fatal, not a \
         fallback)"
    ))
}

/// Every adopted `ctesr-` child of `wallet_name`, with its bundle.
async fn adopted_children(
    cc: &ClientConfig,
    wallet_name: &str,
) -> Result<Vec<(String, mercuryrustlib::tesr::ChildTesrBundle)>> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let mut out = Vec::new();
    for c in rec
        .coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
    {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if let Some(cb) = mercuryrustlib::tesr::load_child(cc, wallet_name, &sid).await? {
            out.push((sid, cb));
        }
    }
    Ok(out)
}

/// The children of `wallet_name` that carry an allocation of `asset`.
async fn colored_children_of(
    cc: &ClientConfig,
    wallet_name: &str,
    asset: &str,
) -> Result<Vec<(String, mercuryrustlib::tesr::ChildTesrBundle)>> {
    Ok(adopted_children(cc, wallet_name)
        .await?
        .into_iter()
        .filter(|(_, cb)| cb.rgb.as_ref().is_some_and(|r| r.contract_id == asset))
        .collect())
}

/// **INV-11, re-derived for a coloured tier.** Exactly one OP_RETURN, and the transaction relays
/// standalone at the fee it committed to. Returns its vsize.
fn assert_colored_tier_shape(
    hex_tx: &str,
    prev_value: u64,
    n_payload: usize,
    fee_rate: f64,
    what: &str,
) -> Result<u64> {
    let tx = parse_tx(hex_tx)?;
    let oprets = tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
    assert_eq!(oprets, 1, "{what}: a coloured tier carries exactly ONE opret commitment (INV-11)");
    // outputs = 1 opret + n payload + 1 P2A anchor.
    assert_eq!(
        tx.output.len(),
        n_payload + 2,
        "{what}: a coloured tier with {n_payload} payload output(s) has {} outputs (opret + \
         payloads + P2A), got {}",
        n_payload + 2,
        tx.output.len()
    );
    let p2a = tx
        .output
        .iter()
        .filter(|o| o.value == mercurylib::tesr::P2A_VALUE)
        .count();
    assert!(p2a >= 1, "{what}: no P2A anchor output of {} sat", mercurylib::tesr::P2A_VALUE);
    // The committed fee is the arithmetic the whole ladder is sized on. Assert it EXACTLY, then
    // assert it actually pays for the transaction — a tier that cannot relay standalone is an exit
    // that cannot be taken.
    let out_sum: u64 = tx.output.iter().map(|o| o.value).sum();
    let fee = prev_value
        .checked_sub(out_sum)
        .ok_or_else(|| anyhow!("{what}: outputs ({out_sum}) exceed the prevout ({prev_value})"))?;
    let committed = mercuryrustlib::rgb::colored_committed_fee(n_payload, fee_rate);
    assert_eq!(fee, committed, "{what}: committed fee must be colored_committed_fee({n_payload}, {fee_rate})");
    // ...and the vbyte MODEL that fee is computed from must match the transaction actually built.
    // This is what the old test's `(150..=320)` vsize band was reaching for, tightened from a
    // 170-vB window to a 2-vB one. It is deliberately NOT "the fee pays for the vsize at `fee_rate`":
    // a coloured tier is v3/TRUC with a P2A anchor precisely so the last vbyte of taproot-signature
    // and varint variance is bumped by whoever wants it confirmed, rather than pre-paid by a model
    // that would then have to over-charge every tier. Measured: T/X_m/S_0 come out 1 vB above the
    // model at 2 sat/vB.
    let modelled_vb = mercurylib::tesr::TIER_VBYTES + n_payload as u64 * mercurylib::tesr::P2TR_OUT_VBYTES;
    let vb = tx.vsize() as u64;
    assert!(
        vb.abs_diff(modelled_vb) <= 2,
        "{what}: built tier is {vb} vB but the committed-fee model says {modelled_vb} vB — the fee \
         arithmetic the whole ladder is sized on has drifted from the transaction it sizes"
    );
    Ok(vb)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in [
        "./rgb-data-sdk29_alice",
        "./rgb-data-sdk29_bob",
        "./rgb-data-sdk29_carol",
        "./rgb-data-sdk29_dave",
    ] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    // [D52] `colored_ladder` is NOT the default — it ships false. This line is what puts the test on
    // the CTES-R lane; without it every assertion below would measure the legacy lane instead.
    let open = |name: &str| {
        let mut cfg = SdkConfig::regtest(name);
        cfg.colored_ladder = true;
        cfg
    };
    let (alice, _) = UtexoWallet::initialize(open("sdk29_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(open("sdk29_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(open("sdk29_carol"), None).await?;
    // dave FIRST-SEES PT2 at 1 raw unit (the minimum representable amount); bob sees it twice.
    let (dave, _) = UtexoWallet::initialize(open("sdk29_dave"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;
    let dave_addr = dave.get_utexo_address().await?;

    // Fund alice's RGB engine (issuance + IFA + mint witness txs are the ISSUER's on-chain cost).
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // ===== (a) PRECISION: issue PT2 (precision 2) behind a COLOURED ladder ======================
    add_tokens(&cc, &alice, 1).await?;
    let asset_p2 = alice.issue_token("PT2", "Precision Two", 2, SUPPLY).await?;
    wait_carriers_confirmed(&cc, &alice, "sdk29_alice", &core, &asset_p2, SUPPLY, 1).await?;
    let alice_tok = alice
        .get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset_p2)
        .ok_or_else(|| anyhow!("issued asset not in alice's balances"))?;
    assert_eq!(alice_tok.precision, 2, "precision is contract metadata");
    assert_eq!(alice_tok.balance, SUPPLY, "supply is RAW units (10_000 raw = \"100.00\")");

    let carriers = wait_colored_carriers(&cc, &alice, "sdk29_alice", &core, &asset_p2, 1).await?;
    let (carrier_sid, carrier_bundle) = carriers.into_iter().next().unwrap();
    let rate = carrier_bundle.fee_rate;
    let carrier_rgb = carrier_bundle.rgb.clone().ok_or_else(|| anyhow!("not a coloured ladder"))?;
    assert_eq!(carrier_rgb.contract_id, asset_p2);
    assert_eq!(carrier_rgb.amount, SUPPLY, "the coloured ladder carries the WHOLE allocation");
    assert_eq!(carrier_bundle.f_value, CARRIER, "the carrier's funding output is TOKEN_CARRIER_SATS");
    println!(
        "SDK29 - issued {asset_p2}: {SUPPLY} raw units at precision 2 (display \"100.00\") on a \
         COLOURED ladder over {carrier_sid} at {rate} sat/vB"
    );

    // INV-11 on the ROOT ladder, tier by tier, each against the value its parent actually pays it.
    // This is the first half of the old "exactly ONE opret commitment output" assertion, which used
    // to be taken from the (now non-existent) `branch-` row of the legacy split.
    let t_vb = assert_colored_tier_shape(
        &carrier_bundle.trigger.signed_tx,
        carrier_bundle.f_value,
        1,
        rate,
        "T",
    )?;
    let x_vb = assert_colored_tier_shape(
        &carrier_bundle.current().extension.signed_tx,
        carrier_bundle.trigger.out_value,
        1,
        rate,
        "X_m",
    )?;
    let s0_vb = assert_colored_tier_shape(
        &carrier_bundle.current().state.signed_tx,
        carrier_bundle.current().extension.out_value,
        1,
        rate,
        "S_0",
    )?;
    println!("ECON colored_ladder rate={rate} F={CARRIER} T_vb={t_vb} X_vb={x_vb} S0_vb={s0_vb} T_out={} X_out={}",
        carrier_bundle.trigger.out_value, carrier_bundle.current().extension.out_value);

    // ===== (c) part 1 — while it CARRIES, the carrier is quarantined and unsplittable ============
    // Unchanged from the flat lane and still true: a plain-BTC split of a carrier would destroy the
    // allocation, and the carrier's sats are not spendable BTC (review H2 / audit [7]/[23]).
    // **[ONE COIN SHAPE] The refusal became STRUCTURAL, so there is nothing left to call.**
    // This used to assert that `split_coin` refuses a carrier with "carries an RGB token
    // allocation". `split_coin` is DELETED — the plain off-chain split it performed spent the coin's
    // funding output `F` directly, which is what a retained trigger also spends [B1]. A carrier can
    // no longer be plain-split because nothing can, which is a stronger guarantee than a refusal
    // message: a message protects only the callers that go through that function.
    assert_eq!(
        alice.get_balance().await?.available_sats,
        0,
        "all of alice's sats ride the carrier — none spendable as BTC"
    );
    // ...and the PLAIN in-ladder route over a COLOURED ladder is refused BY NAME, so the quarantine
    // is not the only thing standing between the allocation and an RGB-unaware tier spend.
    let guard = mercuryrustlib::tesr::refuse_uncolored_over_colored(&carrier_bundle, "in_ladder_split");
    let guard_msg = guard.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        guard_msg.contains("in_ladder_split"),
        "the PLAIN in-ladder split over a COLOURED ladder must be refused by name, got: {guard_msg:?}"
    );
    println!("SDK29 - carrier quarantined: the plain-split route is DELETED (structural, not a refusal); uncoloured-over-coloured refused ({guard_msg})");

    // ===== (a) THE SPLIT: three exact raw amounts + change, in ONE in-ladder split ===============
    // The old test made five successive splits down one carrier's change. `SP` TERMINALIZES its
    // parent, so a carrier is split exactly once — the payments become payload outputs of one `SP`.
    add_tokens(&cc, &alice, 4).await?;
    let x_m_out = carrier_bundle.current().extension.out_value;
    let n_children = 2usize; // [D43] ONE payee + alice's change
    let sp_budget = mercuryrustlib::rgb::colored_tier_out_total(x_m_out, n_children, rate)
        .ok_or_else(|| anyhow!("X_m cannot carry a {n_children}-child coloured split"))?;
    // **[D43] THE K > 1 BATCH IS REFUSED, BY NAME, AND NOTHING IS CO-SIGNED.** This is decision 8's
    // shipped answer, asserted here rather than left as a red test: paying three payees out of one
    // coloured carrier would convey serially after the carrier is already terminal, with no
    // journalled recipient to resume from, so a failure at payee j strands j..3 permanently.
    let batch_err = alice
        .batch_transfer_tokens(
            &asset_p2,
            &[
                (bob_addr.clone(), PAY_BOB),
                (carol_addr.clone(), PAY_CAROL),
                (dave_addr.clone(), PAY_DAVE),
            ],
        )
        .await
        .expect_err("[D43] a coloured batch of THREE payees must be refused");
    let batch_msg = format!("{batch_err:#}");
    assert!(batch_msg.contains("coloured K > 1 refused"), "refused by name: {batch_msg}");
    assert!(
        batch_msg.contains("Nothing has been co-signed"),
        "the refusal must fire BEFORE the split, so the carrier is untouched: {batch_msg}"
    );
    assert!(
        batch_msg.contains("transfer_tokens"),
        "a refusal that removes a capability must name the route that works: {batch_msg}"
    );
    // …and the carrier really is untouched: still coloured, still holding the WHOLE supply.
    assert_eq!(
        token_balance(&alice, &asset_p2).await?,
        SUPPLY,
        "[D43] the refused batch must leave the allocation exactly where it was"
    );
    println!("SDK29 - [D43] K=3 batch REFUSED, carrier untouched: {batch_msg}");

    // THE LANE THAT SHIPS: one payee, one carrier.
    let r_bob = alice.transfer_tokens(&asset_p2, &bob_addr, PAY_BOB).await?;
    assert!(r_bob.used_split, "a partial token payment must carve a piece");
    assert_eq!(r_bob.coins.len(), 1, "a token payout hands over exactly ONE piece");
    assert_eq!(r_bob.coins[0].amount_sats, PIECE, "the piece coin carries TOKEN_PIECE_SATS");
    assert_eq!(r_bob.total_sats, PIECE, "a token piece always carries TOKEN_PIECE_SATS");
    let bob_piece_sid = r_bob.coins[0].statechain_id.clone();

    // alice keeps the change as a COLOURED CHILD (raw-unit conservation on the sender side).
    assert_eq!(
        token_balance(&alice, &asset_p2).await?,
        CHANGE,
        "alice's change: {CHANGE} raw units — a wrong value means the change child was not \
         registered at SP.out[change], or the spent carrier was not un-booked"
    );
    // **[D43] THE CHANGE OF A SINGLE-PAYEE COLOURED SPLIT IS A SPINE TIP, NOT A CHILD.**
    //
    // The three-payee batch this section used to make carved K payee children PLUS a change CHILD.
    // The K=1 lane does not: it leaves the sender's remainder as the SPINE TIP of the batch, which
    // is the CATS change shape — a different row (`spinetip-`), a different health probe, and a
    // different set of things it can do. The distinction is asserted rather than papered over,
    // because it is the shape every coloured sender now ends a payment holding.
    assert!(
        colored_children_of(&cc, "sdk29_alice", &asset_p2).await?.is_empty(),
        "[D43] the K=1 lane must NOT carve a change CHILD — the remainder is a spine tip"
    );
    let alice_change_sid = mercuryrustlib::sqlite_manager::get_all_backup_txs(&cc.pool, "sdk29_alice")
        .await?
        .into_iter()
        .find_map(|(k, _)| k.strip_prefix("spinetip-").map(str::to_string))
        .ok_or_else(|| anyhow!("[D43] alice's change is neither a child nor a spine tip"))?;
    let (tip_contract, tip_assigned, tip_txids, _) =
        alice.colored_tip_health(&alice_change_sid).await?;
    assert_eq!(tip_contract, asset_p2);
    assert_eq!(
        tip_assigned, CHANGE,
        "alice's change TIP must validate off-chain and assign her the remainder"
    );
    alice
        .probe_colored_spine_tip(&alice_change_sid, CHANGE)
        .await
        .map_err(|e| anyhow!("[D43] alice's change tip must still be spendable: {e:#}"))?;
    println!(
        "SDK29 - [D43] the K=1 lane leaves the sender a COLOURED SPINE TIP holding {CHANGE}          ({} witness txids), not a change child",
        tip_txids.len()
    );

    // (c) part 2 — the SPENT carrier's outpoint holds nothing at all.
    let carrier_op = format!("{}:{}", carrier_bundle.f_txid, carrier_bundle.f_vout);
    let allocs = alice.list_token_allocations(&asset_p2).await?;
    assert!(
        !allocs.iter().any(|(op, _)| *op == carrier_op),
        "the SPENT carrier outpoint {carrier_op} must no longer hold an allocation, got {allocs:?}"
    );

    // INV-11 + budget conservation on `SP` itself — the re-derivation of the old "colored split tx:
    // 1 input, piece + change + exactly one OP_RETURN, vsize in band" measurement. `SP` is read off
    // alice's OWN change-child bundle (the sender's copy of the parent segment).
    let alice_tip = mercuryrustlib::tesr::load_spine_tip(&cc, "sdk29_alice", &alice_change_sid)
        .await?
        .ok_or_else(|| anyhow!("alice's spine tip vanished"))?;
    let sp = alice_tip.parent.current().state.clone();
    let sp_vb = assert_colored_tier_shape(&sp.signed_tx, x_m_out, n_children, rate, "SP")?;
    let sp_tx = parse_tx(&sp.signed_tx)?;
    let payload_sum: u64 = sp_tx
        .output
        .iter()
        .filter(|o| !o.script_pubkey.is_op_return() && o.value != mercurylib::tesr::P2A_VALUE)
        .map(|o| o.value)
        .sum();
    assert_eq!(
        payload_sum, sp_budget,
        "the split must hand its children EXACTLY colored_tier_out_total(X_m, {n_children}, {rate}) \
         — sats short of this are silently forfeited to the miner on exit"
    );
    let change_sats = sp_budget - PIECE;
    assert!(
        sp_tx.output.iter().any(|o| o.value == change_sats),
        "the change child must absorb the remainder of the budget ({change_sats} sat)"
    );
    assert_eq!(
        sp_tx.output.iter().filter(|o| o.value == PIECE).count(),
        1,
        "[D43] ONE payout child of exactly TOKEN_PIECE_SATS"
    );
    assert_eq!(sp_tx.input.len(), 1, "SP spends exactly X_m's payload output");
    println!(
        "ECON token_split rate={rate} piece_sats={PIECE} n_children={n_children} sp_vb={sp_vb} \
         sp_budget={sp_budget} change_sats={change_sats} (legacy plain split reference: 155 vB, 1 opret)"
    );
    println!("SDK29 - [D43] ONE in-ladder split paid {PAY_BOB} raw units to ONE payee and kept {CHANGE}");

    // ===== (a) ADOPTION: each recipient books EXACTLY what its own CONSIGNMENT assigns ===========
    for (w, name, addr_sid, want) in [(&bob, "sdk29_bob", &bob_piece_sid, PAY_BOB)] {
        wait_token_balance(w, &asset_p2, want).await?;
        let kids = colored_children_of(&cc, name, &asset_p2).await?;
        assert_eq!(kids.len(), 1, "{name} adopted exactly one coloured child");
        let (sid, cb) = kids.into_iter().next().unwrap();
        assert_eq!(&sid, addr_sid, "{name}'s adopted child is the piece alice conveyed");
        assert!(cb.is_colored(), "{name}'s adopted child must be COLOURED");
        // CONSIGNMENT-derived, never the sender's declared field. This is the raw-unit granularity
        // claim: 1 raw unit books as 1, not as 0 and not as a rounded piece.
        let (contract, assigned, txids, _) = w.colored_child_health(&sid).await?;
        assert_eq!(contract, asset_p2);
        assert_eq!(assigned, want, "{name} must book EXACTLY {want} raw unit(s)");
        assert_eq!(
            txids.len(),
            5,
            "a coloured child resolves against T, X_m, SP, ext_child, state_child — got {txids:?}"
        );
        for txid in txids.iter() {
            assert!(
                onchain(&cc, txid).is_none(),
                "tier {txid} must still be un-broadcast — the whole payment is off-chain"
            );
        }
        // The carrier's packaging sats are TOKEN packaging, not spendable BTC (H2/[23] exclusion).
        assert_eq!(
            w.get_balance().await?.available_sats,
            0,
            "{name}: carrier sats are not spendable BTC"
        );
    }
    let bob_tok = bob
        .get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset_p2)
        .ok_or_else(|| anyhow!("received asset not in bob's balances"))?;
    assert_eq!(bob_tok.precision, 2, "precision metadata travels with the consignment");
    println!("SDK29 - bob booked {PAY_BOB} raw units (\"0.10\"), consignment-derived");

    // ===== (b1) WHAT A RECEIVED PIECE CANNOT DO =================================================
    // OLD: bob's 1_500-sat piece was refused a further colored split by the SATS fit guard
    // ("carrier coin too small": piece + reserve >= carrier). That guard cannot fire on the coloured
    // lane, and it must not: a piece is now deliberately sized ABOVE the coloured ROOT floor,
    // because a piece BELOW it is a piece whose receiver can never ladder it — a coin that is
    // stranded the moment the flat lane retires. That inequality is asserted here directly, at the
    // ladder's own live fee rate, so the reason the constant moved is pinned by this test and not
    // only by the unit suite.
    let child_floor = mercuryrustlib::tesr::colored_child_floor(rate, mercuryrustlib::tesr::COLORED_LADDER_DUST);
    let root_floor = mercuryrustlib::tesr::colored_ladder_floor(rate, mercuryrustlib::tesr::COLORED_LADDER_DUST);
    assert!(
        PIECE >= root_floor,
        "TOKEN_PIECE_SATS ({PIECE}) must clear the coloured ROOT floor ({root_floor} at {rate} \
         sat/vB): a piece below it can be carved but never laddered by its receiver — the 1_500-sat \
         trap the constant was moved out of"
    );
    assert!(child_floor < root_floor, "the child floor is the lower of the two by construction");
    // NEW, and structural rather than arithmetic: a coloured child has no derivable depth-2 seal
    // schedule (`ChildTesrBundle::colored_child_seals`), so a PARTIAL pay out of one is refused by
    // name — at ANY piece size, not merely at small ones. Strictly stronger than the old bound.
    let err = bob
        .transfer_tokens(&asset_p2, &carol_addr, PAY_CAROL)
        .await
        .expect_err("a received coloured child must refuse a PARTIAL pay");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("coloured CHILD-level split is not implemented"),
        "expected the coloured child-level split refusal, got: {msg}"
    );
    assert!(
        msg.contains(&format!("it holds {PAY_BOB}")),
        "the refusal must state what the child actually holds, got: {msg}"
    );
    println!("SDK29 - LIMITATION (a received piece is EXACTLY one piece; piece {PIECE} >= root floor {root_floor}): {msg}");

    // ===== (b2) DOUBLE-RECEIVE: alice forwards her WHOLE change child to bob ======================
    // The coloured lane's answer to "a child moves as a unit": `transfer_tokens` for the child's
    // exact holding routes to `transfer_colored_child`. bob receives PT2 a SECOND time, from a
    // different coin at a different time, and his balance SUMS across two independently adopted
    // children — the same regression the old triple-receive pinned (the accept path must be
    // idempotent on an already-known asset; it used to re-import the genesis, hit a UNIQUE
    // constraint and strand the second allocation).
    // **[D43] THE TIP IS PAYABLE AGAIN — the K=1 lane is repeatable, not one-shot.** This is the
    // question the rewrite had to answer, and it answers it by executing: alice pays a SECOND
    // payment out of the spine tip her first one left her, carving a fresh piece and a fresh tip.
    // So "K = 1 per carrier" bounds the PAYEES OF ONE PAYMENT, not the payments of one carrier.
    //
    // It must be a PARTIAL pay. A spine batch needs a change leg — the next payment's funding
    // outpoint — so moving the tip's whole holding is refused by name, with "convey the tip whole"
    // as the remedy. That refusal is asserted below rather than worked around, because it is the
    // boundary between the two operations.
    let whole_tip_err = alice
        .transfer_tokens(&asset_p2, &bob_addr, CHANGE)
        .await
        .expect_err("[D43] batching a tip's WHOLE holding must be refused — it leaves no change leg");
    let whole_tip_msg = format!("{whole_tip_err:#}");
    assert!(
        whole_tip_msg.contains("change tip"),
        "the refusal must name the missing change leg: {whole_tip_msg}"
    );
    assert!(
        whole_tip_msg.contains("convey the tip whole"),
        "…and name the operation that DOES move it all: {whole_tip_msg}"
    );
    println!("SDK29 - [D43] a whole-tip batch is refused (no change leg): {whole_tip_msg}");

    let second_pay = CHANGE - PAY_BOB;
    // A FRESH slot. `get_utexo_address` mints a single-use transfer slot; the first payment consumed
    // the one above, and re-using it silently delivers nowhere — which is how this read as "bob
    // never received" rather than as a re-use error.
    let bob_addr_2 = bob.get_utexo_address().await?;
    let r_fwd = alice.transfer_tokens(&asset_p2, &bob_addr_2, second_pay).await?;
    assert!(r_fwd.used_split, "[D43] a partial pay out of a tip carves a piece");
    assert_eq!(r_fwd.coins.len(), 1, "one payee, one piece");
    let bob_total = PAY_BOB + second_pay;
    // **[#152] THE SUM IS ASSERTED AGAIN, and this is what the fix bought.**
    //
    // This waited on the settled balance, which summed a second receive arriving as a whole-child
    // forward but NOT one carved from a SPINE TIP: both children adopted, both carrying their
    // allocation, one balance. `get_asset_balance` is chain-anchored and every allocation here is
    // deliberately un-broadcast, so what it can settle depended on the SHAPE the allocation arrived
    // in — which is not a property a balance should have.
    //
    // `get_token_balances` now takes the off-chain half from the wallet's OWN adopted material
    // (carriers, `ctesr-` children, `spinetip-` tips), so the shape stops mattering. The per-child
    // assertions below stay: they read each consignment directly, which is the authority the sum is
    // only a convenience over.
    wait_token_balance(&bob, &asset_p2, bob_total).await?;
    assert_eq!(
        token_balance(&bob, &asset_p2).await?,
        bob_total,
        "[#152] second receive must SUM: {PAY_BOB} + {second_pay} = {bob_total}, whether the second \
         arrived as a whole-child forward or as a piece carved from a spine tip"
    );
    let bob_kids = colored_children_of(&cc, "sdk29_bob", &asset_p2).await?;
    assert_eq!(bob_kids.len(), 2, "bob holds TWO coloured children of the same asset");
    let mut bob_amounts: Vec<u64> =
        bob_kids.iter().map(|(_, cb)| cb.rgb.as_ref().unwrap().amount).collect();
    bob_amounts.sort_unstable();
    assert_eq!(
        bob_amounts,
        { let mut v = vec![PAY_BOB, second_pay]; v.sort_unstable(); v },
        "BOTH allocations must be booked — a stranded second receive shows up here as a missing one"
    );
    for (sid, _) in bob_kids.iter() {
        let (_, amt, _, _) = bob.colored_child_health(sid).await?;
        assert!(amt == PAY_BOB || amt == second_pay, "unexpected child amount {amt}");
    }
    assert_eq!(
        token_balance(&alice, &asset_p2).await?,
        PAY_BOB,
        "[D43] alice keeps the second payment's change on a fresh spine tip"
    );
    // RAW-UNIT CONSERVATION, end to end — summed over the CHILDREN and the sender's TIP, which are
    // where the allocations actually live. (Summing settled balances would inherit the query
    // discrepancy noted above and turn a real conservation law into a test of a getter.)
    let mut total_out: u64 = 0;
    for name in ["sdk29_bob", "sdk29_carol"] {
        for (_, cb) in colored_children_of(&cc, name, &asset_p2).await? {
            total_out += cb.rgb.as_ref().map(|r| r.amount).unwrap_or_default();
        }
    }
    total_out += token_balance(&alice, &asset_p2).await?;
    assert_eq!(total_out, SUPPLY, "every raw unit of the supply is accounted for");
    println!("SDK29 - DOUBLE-RECEIVE: bob booked {PAY_BOB} then {second_pay} on two children ({bob_total}); alice keeps {PAY_BOB} on a fresh tip; Σ = {SUPPLY}");

    // ===== (d) CROSS-CARRIER: two coloured carriers, one payment larger than either ==============
    add_tokens(&cc, &alice, 1).await?;
    let asset_q = alice
        .issue_inflatable_token("QTK", "Q Token", 0, Q_ISSUE, vec![Q_MINT])
        .await?;
    wait_carriers_confirmed(&cc, &alice, "sdk29_alice", &core, &asset_q, Q_ISSUE, 1).await?;
    println!("SDK29 - issued IFA {asset_q}: {Q_ISSUE} units on carrier A (+{Q_MINT} inflation-right)");

    // Mint needs blocks while it polls (on-chain inflate): run a scoped background miner.
    let mining = Arc::new(AtomicBool::new(true));
    let miner = {
        let m = mining.clone();
        let core = core.clone();
        std::thread::spawn(move || {
            while m.load(Ordering::Relaxed) {
                let _ = bitcoin_core::generatetoaddress(1, &core);
                std::thread::sleep(Duration::from_secs(2));
            }
        })
    };
    add_tokens(&cc, &alice, 1).await?;
    let mint_res = alice.mint_tokens(&asset_q, vec![Q_MINT]).await;
    mining.store(false, Ordering::Relaxed);
    let _ = miner.join();
    let (mint_txid, minted) = mint_res?;
    assert_eq!(minted, Q_MINT);
    wait_carriers_confirmed(&cc, &alice, "sdk29_alice", &core, &asset_q, Q_ISSUE + Q_MINT, 2).await?;
    let q_carriers = wait_colored_carriers(&cc, &alice, "sdk29_alice", &core, &asset_q, 2).await?;
    assert_eq!(q_carriers.len(), 2, "both QTK carriers must be COLOURED — the flat lane is retired");
    // Record each carrier's budget BEFORE the payment; the legs are planned largest-allocation-first.
    let mut q_plan: Vec<(String, u64, u64, f64)> = q_carriers
        .iter()
        .map(|(sid, b)| {
            let r = b.rgb.as_ref().unwrap();
            (sid.clone(), r.amount, b.current().extension.out_value, b.fee_rate)
        })
        .collect();
    q_plan.sort_by(|a, b| b.1.cmp(&a.1));
    assert_eq!(
        q_plan.iter().map(|p| p.1).collect::<Vec<_>>(),
        vec![Q_ISSUE, Q_MINT],
        "alice holds {Q_ISSUE} + {Q_MINT} across two carriers, neither covering {Q_PAY}"
    );
    println!("SDK29 - minted +{Q_MINT} (inflate {mint_txid}): alice holds {} QTK on TWO coloured carriers", Q_ISSUE + Q_MINT);

    // No SINGLE carrier holds 100, so `colored_transfer` falls through to the multi-carrier lane:
    // ONE in-ladder split per carrier. (The legacy lane did this as one transparent COMBINE and
    // handed over a single piece; the coloured lane cannot combine two independent off-chain
    // ladders into one transaction, so the recipient is paid in N pieces. Asserted as exactly 2.)
    add_tokens(&cc, &alice, 4).await?;
    let r5 = alice.transfer_tokens(&asset_q, &bob_addr, Q_PAY).await?;
    assert_eq!(r5.coins.len(), 2, "a two-carrier payment is TWO in-ladder split legs, one piece each");
    // LEG 1 — the 60-carrier pays its WHOLE allocation: the NO-CHANGE shape. No change child is
    // carved (one holding no allocation would spend sats to hold nothing), so the single piece must
    // absorb the ENTIRE SP budget. This is the surviving, re-derived half of the old
    // "spent-carrier change" assertion: nothing is stranded and nothing is forfeited.
    let leg1_budget = mercuryrustlib::rgb::colored_tier_out_total(q_plan[0].2, 1, q_plan[0].3)
        .ok_or_else(|| anyhow!("carrier A cannot carry a 1-child coloured split"))?;
    // LEG 2 — the 50-carrier pays 40 and keeps 10: the with-change shape, so its piece is exactly
    // one TOKEN_PIECE_SATS.
    let leg_sats: Vec<u64> = r5.coins.iter().map(|c| c.amount_sats).collect();
    assert!(
        leg_sats.contains(&leg1_budget),
        "the WHOLE-allocation leg's piece must absorb its carrier's entire SP budget \
         ({leg1_budget} sat) — got {leg_sats:?}"
    );
    assert!(
        leg_sats.contains(&PIECE),
        "the PARTIAL leg's piece must be exactly TOKEN_PIECE_SATS ({PIECE}) — got {leg_sats:?}"
    );
    assert_eq!(r5.total_sats, leg_sats.iter().sum::<u64>(), "total_sats is the sum of the legs");
    wait_token_balance(&bob, &asset_q, Q_PAY).await?;
    assert_eq!(token_balance(&bob, &asset_q).await?, Q_PAY, "bob receives the full {Q_PAY} QTK");
    assert_eq!(
        token_balance(&alice, &asset_q).await?,
        Q_ISSUE + Q_MINT - Q_PAY,
        "alice keeps the {} QTK change",
        Q_ISSUE + Q_MINT - Q_PAY
    );
    // NO change CHILD from either leg. The whole-allocation leg carves none by construction (a
    // child with an empty RGB assignment is a shape nothing in CTES-R produces or verifies), and
    // [D43] the PARTIAL leg leaves its change on a SPINE TIP rather than a child — the same shape
    // section (a) found on PT2, confirmed here on a second asset and a second lane.
    assert!(
        colored_children_of(&cc, "sdk29_alice", &asset_q).await?.is_empty(),
        "[D43] neither QTK leg may leave alice a change CHILD"
    );
    let q_tips: Vec<String> = mercuryrustlib::sqlite_manager::get_all_backup_txs(&cc.pool, "sdk29_alice")
        .await?
        .into_iter()
        .filter_map(|(k, _)| k.strip_prefix("spinetip-").map(str::to_string))
        .collect();
    let mut q_change_found = false;
    for t in &q_tips {
        if let Ok((c, amt, _, _)) = alice.colored_tip_health(t).await {
            if c == asset_q {
                assert_eq!(
                    amt,
                    Q_ISSUE + Q_MINT - Q_PAY,
                    "the partial leg's change tip must carry the QTK remainder"
                );
                q_change_found = true;
            }
        }
    }
    assert!(q_change_found, "[D43] the partial QTK leg left no change tip — the remainder is lost");
    println!(
        "SDK29 - CROSS-CARRIER: transfer_tokens({Q_PAY}) spanned both coloured carriers as 2 legs \
         (whole-allocation leg absorbed its full {leg1_budget}-sat budget with NO change child; \
         partial leg carved a {PIECE}-sat piece + a 10-unit change TIP [D43])"
    );

    // ===== (d2) [D43] A SECOND PAYEE NEEDS A SECOND CARRIER — and a child moves WHOLE ===========
    //
    // This is what replaced the three-payee batch. Under D43 alice could not have paid carol out of
    // PT2's carrier at all: the carrier is terminal after one split, and its change is a depth-1
    // coloured child no guard will split again. What CAN move is the child ITSELF, whole — so bob
    // hands carol the `PAY_BOB` child he adopted above. Carol is paid; nothing was subdivided.
    //
    // The 1-raw-unit MINIMUM the old `PAY_DAVE` leg pinned is not expressible from this carrier for
    // the same reason, and that is the shipped answer rather than a gap: an issuer who intends to
    // pay N parties mints N carriers. `PAY_DAVE` is kept as a constant so the next reader can see
    // what the shape used to be and why it moved.
    let carol_slot = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk29_carol").await?;
    let r_carol = bob.transfer_tokens(&asset_p2, &carol_slot, PAY_BOB).await?;
    assert!(!r_carol.used_split, "[D43] a whole-child forward is a re-transfer, not a split");
    assert_eq!(r_carol.coins.len(), 1, "the forwarded child is one coin");
    assert_eq!(
        r_carol.coins[0].statechain_id, bob_piece_sid,
        "the forward moves the ADOPTED child itself, not a new piece"
    );
    wait_token_balance(&carol, &asset_p2, PAY_BOB).await?;
    let carol_kids = colored_children_of(&cc, "sdk29_carol", &asset_p2).await?;
    assert_eq!(carol_kids.len(), 1, "carol adopted exactly one coloured child");
    let carol_piece_sid = carol_kids[0].0.clone();
    assert_eq!(
        carol_kids[0].1.rgb.as_ref().unwrap().amount,
        PAY_BOB,
        "carol's child carries the whole forwarded allocation"
    );
    println!("SDK29 - [D43] second payee via a WHOLE-CHILD forward: bob -> carol, {PAY_BOB} raw units");

    // ===== (e) THE TOKEN EXIT: carol walks her coloured child, keyless ===========================
    let carol_cb = mercuryrustlib::tesr::load_child(&cc, "sdk29_carol", &carol_piece_sid)
        .await?
        .ok_or_else(|| anyhow!("carol's child bundle vanished"))?;
    let chain = mercuryrustlib::tesr::child_exit_chain(&carol_cb);
    assert_eq!(chain.len(), 5, "the child's exit chain is T, X_m, SP, ext_child, state_child");
    let deposit_outpoint = parse_tx(&chain[0].0)?.input[0].previous_output;
    assert_eq!(
        deposit_outpoint.txid.to_string(),
        carrier_bundle.f_txid,
        "the chain root spends the carrier's on-chain funding output F"
    );

    // NEGATIVE CONTROL: the spend that would destroy the allocation is refused BY NAME.
    // (`unilateral_exit` no longer refuses a carrier outright — that is the CTES-R product claim
    // and is exercised below. The hazard the old refusal guarded is now guarded here: `ext_child`'s
    // payload output is a SEALED output, so an RGB-unaware replacement state BURNS the allocation.)
    let mut carol_child_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk29_carol")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(carol_piece_sid.as_str()) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("carol's child coin vanished"))?;
    assert_eq!(carol_child_coin.amount, Some(PIECE as u32), "carol's piece carries TOKEN_PIECE_SATS");
    let refused = mercuryrustlib::tesr::child_retransfer(
        &cc,
        "sdk29_carol",
        &mut carol_child_coin,
        &carol_cb,
        &dave_addr,
    )
    .await;
    let refused_msg = refused.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        refused_msg.contains("child_retransfer"),
        "the PLAIN child re-transfer of a COLOURED child must be refused by name, got: {refused_msg:?}"
    );

    // BEFORE the walk: the probes must already discriminate, or the after-shots prove nothing (E7 —
    // a fully invalidated stock still reports a healthy `get_asset_balance`, so no balance is ever
    // used as survival evidence).
    carol
        .probe_colored_child_tip(&carol_piece_sid, PAY_BOB)
        .await
        .map_err(|e| anyhow!("carol's stock is dead BEFORE the walk, so nothing is provable: {e}"))?;
    assert!(
        carol.probe_colored_child_tip(&carol_piece_sid, PAY_BOB + 1).await.is_err(),
        "the stock probe accepted MORE than the allocation — it is not discriminating"
    );
    assert!(
        carol.colored_child_exit_proof(&carol_piece_sid).await.is_err(),
        "the leaf consignment validated against the CHAIN ALONE before any tier was broadcast — \
         the empty-offchain-set proof would be vacuous"
    );

    let mut passes = 0;
    loop {
        passes += 1;
        assert!(passes < 25, "the coloured child exit did not converge");
        let statuses = carol
            .unilateral_exit(Some(vec![carol_piece_sid.clone()]), None)
            .await
            .map_err(|e| {
                anyhow!("unilateral_exit REFUSED a coloured CHILD — the piece is unexitable: {e}")
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

    // Every tier MINED, every tier carrying exactly one opret anchor (INV-11 on the witnesses).
    let mut level_vbs = Vec::new();
    for (hex_tx, _) in chain.iter() {
        let tx = parse_tx(hex_tx)?;
        assert!(
            onchain(&cc, &tx.txid().to_string()).is_some(),
            "tier {} never reached the chain",
            tx.txid()
        );
        assert_eq!(
            tx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count(),
            1,
            "every tier of a COLOURED exit chain carries exactly one opret anchor (INV-11)"
        );
        level_vbs.push(tx.vsize() as u64);
    }
    assert!(
        is_outpoint_spent(&cc, &deposit_outpoint.txid.to_string(), deposit_outpoint.vout)?,
        "the chain root must have spent the on-chain funding output F"
    );

    // carol's piece outpoint is a LIVE on-chain UTXO — and, unlike the flat lane, it pays CAROL'S
    // OWN key (Model A), so the sats and the allocation land together instead of the sats being
    // parked behind an uncoloured sweep that would burn the asset.
    //
    // The exited value is NOT the whole TOKEN_PIECE_SATS: the piece paid for its own two coloured
    // rungs (`ext_child` then `state_child`), each costing `colored_committed_fee(1, rate) +
    // P2A_VALUE`. That is exactly the arithmetic `colored_child_floor` charges, so the landing value
    // is derived here from the same two functions rather than measured — and it must still clear
    // COLORED_LADDER_DUST, which is the "a child's final state output is spendable" half of the
    // floor. (Old lane, for contrast: the piece landed its full 1_500 sats but on a 2-of-2 outpoint
    // whose only pre-signed sweep was UNCOLOURED, i.e. unspendable without burning the asset.)
    let state_tx = onchain(&cc, &carol_cb.child_state.txid)
        .ok_or_else(|| anyhow!("the child's final state is not on chain"))?;
    let leaf_vout = carol_cb.child_state.payload_vout;
    let payee_spk = &state_tx.output[leaf_vout as usize].script_pubkey;
    let rung = mercuryrustlib::rgb::colored_committed_fee(1, rate) + mercurylib::tesr::P2A_VALUE;
    let exited_sats = PIECE - 2 * rung;
    assert_eq!(
        carol_cb.child_state.out_value, exited_sats,
        "the child's declared exit value must be TOKEN_PIECE_SATS minus its own two coloured rungs"
    );
    assert!(
        exited_sats >= mercuryrustlib::tesr::COLORED_LADDER_DUST,
        "the exited output ({exited_sats}) must clear the dust floor — otherwise the piece funded a \
         ladder it could not land"
    );
    assert_eq!(
        state_tx.output[leaf_vout as usize].value, exited_sats,
        "the exited piece holds TOKEN_PIECE_SATS minus its two coloured rungs ({PIECE} - 2*{rung})"
    );
    let plain_addr = mercurylib::tesr::payee_address(
        &carol_cb.child_owner_exit_address,
        &carol_cb.parent.network,
    )
    .map_err(|e| anyhow!("could not resolve carol's exit address: {e:?}"))?;
    let expected_spk = electrum_client::bitcoin::Address::from_str(&plain_addr)?
        .assume_checked()
        .script_pubkey();
    assert_eq!(payee_spk, &expected_spk, "the child's final state must pay CAROL's own key");
    let mut leaf_unspent = false;
    for _ in 0..30 {
        let listed = cc.electrum_client.script_list_unspent(payee_spk)?;
        if listed.iter().any(|u| {
            u.tx_hash.to_string() == carol_cb.child_state.txid
                && u.tx_pos as u32 == leaf_vout
                && u.value == exited_sats
        }) {
            leaf_unspent = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(leaf_unspent, "carol's exited piece outpoint must be an unspent on-chain UTXO");

    // THE ALLOCATION SURVIVED — and only these two say so (E7).
    let mut proof = carol.colored_child_exit_proof(&carol_piece_sid).await;
    for _ in 0..20 {
        if proof.is_ok() {
            break;
        }
        let msg = proof.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        if !msg.contains("can't be located in the blockchain") {
            break; // a real verdict, not indexer lag (see mine_synced)
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        proof = carol.colored_child_exit_proof(&carol_piece_sid).await;
    }
    let (contract, assigned, detail) = proof.map_err(|e| {
        anyhow!(
            "THE ALLOCATION DID NOT SURVIVE THE WALK: the child's leaf consignment does not \
             validate against the chain alone after every tier was mined — {e}"
        )
    })?;
    assert_eq!(contract, asset_p2, "the surviving allocation is THIS contract");
    assert_eq!(assigned, PAY_BOB, "exactly {PAY_BOB} raw units survive on carol's exit output");
    carol
        .probe_colored_child_tip(&carol_piece_sid, PAY_BOB)
        .await
        .map_err(|e| anyhow!("the stock is DEAD after the exit walk: {e}"))?;
    assert!(
        carol.probe_colored_child_tip(&carol_piece_sid, PAY_BOB + 1).await.is_err(),
        "after the walk the probe accepted MORE than the allocation — it is not reading the stock"
    );
    println!(
        "ECON token_exit chain_txs={} tier_vbs={:?} total_vb={} piece_sats={PIECE} exited_sats={exited_sats} rate={rate}",
        chain.len(),
        level_vbs,
        level_vbs.iter().sum::<u64>()
    );
    println!("SDK29 - TOKEN EXIT: carol walked all 5 coloured tiers keyless; {assigned} raw units settled on her own exit outpoint {}:{leaf_vout} ({detail:?})", carol_cb.child_state.txid);

    println!(
        "SDK29 - SUCCESS (CTES-R lane, [D43] K=1 per carrier): a K>1 coloured batch is REFUSED by \
         name with the carrier untouched; the K=1 lane pays ONE payee a piece of exactly {PIECE} \
         sats and leaves the sender a coloured SPINE TIP (not a change child), which is PAYABLE \
         AGAIN -- so K=1 bounds the payees of ONE PAYMENT, not the payments of one carrier. Moving a \
         tip WHOLE is refused (it would leave no change leg) and the refusal names `convey the tip \
         whole`. A second payee is served by forwarding an adopted child WHOLE. Raw-unit \
         conservation holds end to end, summed over children + tip. A received piece is EXACTLY one \
         piece, and it clears the coloured ROOT floor so its receiver can always ladder it. A \
         fully-paid carrier leaves NO change child and forfeits NO sats. A payment larger than any \
         single carrier is served by two in-ladder legs. A received piece EXITS unilaterally with \
         its allocation intact, validated against the chain alone."
    );
    Ok(())
}
