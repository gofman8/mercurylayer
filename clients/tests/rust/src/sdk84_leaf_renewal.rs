//! E2E (SDK_E2E=84) — **LEAF RENEWAL: a leaf that has spent its transfer budget gets it back, for
//! zero on-chain bytes and no depth.**
//!
//! A LEAF (an adopted in-ladder split child — a `ctesr-` row, a [`ChildTesrBundle`]) had a HARD,
//! unreplenishable transfer budget. `child_retransfer` takes one `δ` off the state rung per
//! whole-coin hop and refuses at the floor, and leaves are minted at `state_csv(0)`
//! (`in_ladder_split` / `spine_batch_split` / `child_in_ladder_split` all write
//! `child_state_csv: p.state_csv(0)`). On the mainnet schedule that is `(1440 − 144) / 36 = 36`
//! hops and then the leaf could **never move again**; the message it refused with named remedies a
//! leaf does not have — it cannot re-anchor (`refresh()` routes a `ctesr-` coin to
//! `unilateral_exit`, because `SP.out[j]` is un-broadcast and there is no confirmed outpoint to
//! co-operatively spend) and it has no `rollover` (`renew`/`rollover` both take `&mut TesrBundle`).
//! Leaves are what almost every user holds.
//!
//! `renew_child` rebuilds `child_extension` + `child_state` **in place over the SAME `SP.out[j]`**,
//! resetting the state rung to `state_csv(0)` without consuming a depth level: zero on-chain bytes,
//! two SE co-signatures, client-only (the SE is blind — it is handed a sighash and a caller-supplied
//! prevout amount and never deserialises an output, so it neither knows nor cares that this is a
//! renewal rather than a first mint). The budget becomes
//! `hops-per-epoch × (m_max + 1) epochs` per depth level — mainnet `36 × 16 = 576`, and `m = 0` is
//! itself an epoch, which is why it is `m_max + 1` and not `m_max`.
//!
//! **THE SAFETY PROPERTY EVERYTHING RESTS ON: REPLACE-BY-LOWER-TIMELOCK.** Every extension this leaf
//! has ever had — the original and every renewal — spends the SAME outpoint `SP.out[j]`, so exactly
//! ONE can ever confirm. `child_retransfer` replaces only the STATE, so an extension is SHARED with
//! every previous owner of the leaf: after `k` renewals and `h` hops the extensions in existence are
//! `X_0 … X_k` with strictly decreasing CSV, the CURRENT owner holds `X_k` (the lowest) plus a state
//! over `X_k.out[0]`, and a PREVIOUS owner holds whichever `X_i` was live during their tenure plus
//! their retained state over `X_i.out[0]`. `X_k` matures strictly first, so it wins `SP.out[j]`
//! against every `X_i`; a stale owner's retained state can only confirm if their `X_i` confirms, and
//! it cannot. A renewal therefore makes every stale threat STRICTLY WEAKER — they must win two races
//! instead of one. Step [8] settles that on chain rather than on paper.
//!
//! ## What is proved, in the order the numbered steps run
//!
//!  1. alice deposits, `claim()` LADDERS the coin, and a non-exact payment splits it in-ladder so
//!     `h0` holds a LEAF minted at `state_csv(0)`.
//!  2. the leaf is transferred onward, whole-coin, until its state CSV sits ON the floor — on regtest
//!     `(d0 − d_floor) / δ = (24 − 6) / 6 = 3` hops, taken from `TesrParams`, never written as a
//!     literal.
//!  3. the NEXT whole-coin transfer is REFUSED, and the refusal names RENEWAL (`renew_child`) — not
//!     the impossible re-anchor the old text named.
//!  4. the leaf is RENEWED: state CSV back to `state_csv(0)`, extension CSV strictly LOWER, the same
//!     `SP.out[j]`, depth unchanged, the census balanced, and the bundle still accepted by the REAL
//!     receiver path (`verify_conveyed_child`).
//!  5. transfers work again, and the leaf survives exactly the same number of further hops.
//!  6. renewal is repeated until the extension schedule is exhausted (regtest `m_max = 2`, so three
//!     epochs and two renewals), and the exhaustion refusal names `child_in_ladder_split` — the
//!     leaf's analogue of `rollover` — with `re-anchor` appearing only as an explicit denial.
//!  7. **[N1]** a renewal whose extension CSV is not strictly LOWER is refused with NO co-signature
//!     burned, and three neighbouring rungs (off-grid, and the clamped floor) with it.
//!  8. the renewed leaf still EXITS unilaterally on its pre-signed tiers, and the transaction that
//!     actually spends `SP.out[j]` on chain is the RENEWED extension — neither superseded extension
//!     ever reaches the chain.
//!
//! **[N1] is placed after the FIRST renewal on purpose.** `plan_child_renewal` checks exhaustion
//! (`needs_rollover`) BEFORE the strict-decrease guard, so run at the end of the schedule the
//! exhaustion refusal would fire first and the step would prove nothing about the guard it is named
//! for. It is run while the leaf is still renewable, which is the only place it is meaningful.
//!
//! ## A note on the wallet-per-hop shape
//!
//! Every hop lands in a FRESH wallet. Ping-ponging the leaf between two wallets would hand a wallet
//! a statechain id it already holds a WITHDRAWN row for, and `renew_child`'s `[A2]` liveness read
//! keys on the `duplicate_index == 0` row — so a re-receiving wallet would make every liveness and
//! census assertion below ambiguous. Fresh holders keep each assertion about exactly one row.
//!
//! Run: SDK_E2E=84 ML_NETWORK=regtest cargo +stable run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::tesr::TesrParams;
use mercurylib::wallet::{Coin, CoinStatus};
use mercuryrustlib::client_config::ClientConfig;
use mercuryrustlib::tesr::ChildTesrBundle;

use crate::bitcoin_core;

/// The only on-chain funding in the whole flow.
const DEPOSIT: u64 = 100_000;
/// The non-exact payment that forces the in-ladder split, and therefore the leaf's value. It never
/// changes again: a whole-coin `child_retransfer` rebuilds the state over the SAME extension payload
/// output at the SAME value, and a renewal rebuilds both tiers at the same committed fee — so the
/// leaf is worth exactly this from step 1 to step 8.
const PAY: u64 = 30_000;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn v2_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn coins_of(cc: &ClientConfig, wallet_name: &str) -> Result<Vec<Coin>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins)
}

/// The `duplicate_index == 0` coin for `sid` in `wallet_name`. Deliberately the same row
/// `renew_child`'s `[A2]` `durable_coin_status` reads, so an assertion here is about the row the
/// driver's own liveness gate consults.
async fn coin_of(cc: &ClientConfig, wallet_name: &str, sid: &str) -> Result<Coin> {
    coins_of(cc, wallet_name)
        .await?
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("{wallet_name} holds no duplicate_index 0 coin for {sid}"))
}

async fn num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or_else(|| anyhow!("no statechain info for {sid}"))?
        .num_sigs)
}

fn parse_tx(signed_hex: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(signed_hex)?)?)
}

/// The SIGNED nSequence of a tier, as a BIP-68 block count.
///
/// Not `tier.csv`. That field travels beside the transaction and is a sender-supplied number; the
/// nSequence is committed by the taproot SIGHASH_ALL sighash `verify_tier_cosigned` checks, so it
/// cannot be moved without invalidating a signature nobody outside the SE can forge. Every CSV claim
/// in this test is made against this, and the declared field is asserted to AGREE rather than
/// trusted — which is exactly the doctrine `child_extension_csv` is written to.
fn signed_csv(tier: &mercuryrustlib::tesr::TesrTier) -> Result<u16> {
    let tx = parse_tx(&tier.signed_tx)?;
    let seq = tx
        .input
        .first()
        .ok_or_else(|| anyhow!("tier {} has no input", tier.txid))?
        .sequence
        .0;
    if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
        return Err(anyhow!("tier {} nSequence {seq:#x} is not a BIP-68 block timelock", tier.txid));
    }
    let signed = seq as u16;
    let declared = tier
        .csv
        .ok_or_else(|| anyhow!("tier {} declares no CSV", tier.txid))?;
    if declared != signed {
        return Err(anyhow!(
            "tier {}: declared CSV {declared} disagrees with the SIGNED nSequence {signed} — a \
             stored row whose two copies disagree is corruption to report, not to average over",
            tier.txid
        ));
    }
    Ok(signed)
}

/// The outpoint a tier spends, as `txid:vout`.
fn tier_prevout(tier: &mercuryrustlib::tesr::TesrTier) -> Result<String> {
    let tx = parse_tx(&tier.signed_tx)?;
    let op = tx
        .input
        .first()
        .ok_or_else(|| anyhow!("tier {} has no input", tier.txid))?
        .previous_output;
    Ok(format!("{}:{}", op.txid, op.vout))
}

/// Is this txid known to the chain backend at all (mempool or mined)?
///
/// `Err` is NOT folded into `false`. A backend that could not answer has told us nothing, and the
/// off-chain claims below ("nothing was broadcast", "the superseded extension never reached the
/// chain") would be fabrications if an unreadable backend were reported as an absent transaction.
fn tx_known(cc: &ClientConfig, txid: &str) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let id = Txid::from_str(txid).map_err(|e| anyhow!("bad txid {txid}: {e}"))?;
    match cc.electrum_client.transaction_get_raw(&id) {
        std::result::Result::Ok(_) => Ok(true),
        // electrum answers "missing" for an unknown tx with an error and there is no way to tell
        // that apart from a transport failure at this layer. Callers use `false` only as "still
        // off-chain / keep waiting", never as proof of anything else.
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

/// Assert a refusal happened AND that it is the refusal we meant.
///
/// `{:#}` rather than `to_string()`: the SDK's `transfer` propagates `child_retransfer`'s error with
/// a bare `?` today, but a `.context(..)` added tomorrow would push the message we are matching into
/// the source chain and turn every negative step below into a silent pass-for-the-wrong-reason. The
/// alternate form prints the whole chain, so the assertion survives that change.
fn refused_with<T: std::fmt::Debug>(r: Result<T>, attack: &str, expect: &str) -> Result<String> {
    match r {
        std::result::Result::Ok(v) => Err(anyhow!("SAFETY: {attack} was ACCEPTED, got {v:?}")),
        Err(e) => {
            let msg = format!("{e:#}");
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

/// Poll `claim()` until `w` books `sid` as a CONFIRMED, duplicate_index-0 coin carrying a child
/// bundle, and return that bundle.
///
/// A leaf is a leaf only if it carries the `ctesr-` bundle the in-ladder route mints; a count-only
/// or balance-only check would pass on a plain V1 sub-coin, which has none.
async fn claim_leaf(
    cc: &ClientConfig,
    w: &UtexoWallet,
    name: &str,
    sid: &str,
) -> Result<ChildTesrBundle> {
    for _ in 0..60 {
        w.claim().await?;
        let live = coins_of(cc, name).await?.into_iter().any(|c| {
            c.statechain_id.as_deref() == Some(sid)
                && c.duplicate_index == 0
                && c.status == CoinStatus::CONFIRMED
        });
        if live {
            if let Some(cb) = mercuryrustlib::tesr::load_child(cc, name, sid).await? {
                return Ok(cb);
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} did not adopt leaf {sid}"))
}

/// The census term the receiver's verifier computes: one slot per disclosed superseded tier, states
/// and extensions alike (`verify_superseded_segment` returns `sup_states.len() + sup_exts.len()`).
fn disclosed_superseded(cb: &ChildTesrBundle) -> u32 {
    (cb.child_superseded_states.len() + cb.child_superseded_extensions.len()) as u32
}

/// `child_num_sigs == child_flat_backups + 2 live tiers + superseded` — the leaf's EXACT-EQUALITY
/// census, evaluated exactly as `verify_child_bundle` evaluates it. A derived child slot carries no
/// flat backup (`CHILD_V2_BASELINE = 0`), so the whole term is `2 + superseded`.
async fn assert_census_balances(cc: &ClientConfig, cb: &ChildTesrBundle, where_: &str) -> Result<u32> {
    let sid = &cb.child_statechain_id;
    let issued = num_sigs(cc, sid).await?;
    let disclosed = mercuryrustlib::tesr::CHILD_V2_BASELINE + 2 + disclosed_superseded(cb);
    if issued != disclosed {
        return Err(anyhow!(
            "{where_}: leaf census does NOT balance — the SE issued {issued} co-signature(s) but the \
             bundle discloses {disclosed} ({} live tiers + {} superseded state(s) + {} superseded \
             extension(s) on a CHILD_V2_BASELINE of {}). The leaf's census is exact equality, so a \
             gap in either direction is unfixable: too high means a hidden co-signed state, too low \
             means signatures this wallet can never account for.",
            2,
            cb.child_superseded_states.len(),
            cb.child_superseded_extensions.len(),
            mercuryrustlib::tesr::CHILD_V2_BASELINE
        ));
    }
    Ok(issued)
}

/// The receiver's REAL admission path, run against the bundle as it stands.
///
/// The exit address is asserted to be the current holder's OWN backup address before it is passed
/// in — otherwise the Model-A check (`the final child state must pay THIS`) would be handed the
/// bundle's own field and would prove nothing.
async fn verifies_at_the_receiver(
    cc: &ClientConfig,
    wallet_name: &str,
    cb: &ChildTesrBundle,
    where_: &str,
) -> Result<u64> {
    let coin = coin_of(cc, wallet_name, &cb.child_statechain_id).await?;
    let mine = mercurylib::transaction::get_user_backup_address(&coin, "regtest".to_string())?;
    if mine != cb.child_owner_exit_address {
        return Err(anyhow!(
            "{where_}: the leaf's exit address {} is not {wallet_name}'s own backup address {mine} \
             — running the Model-A check against the bundle's own field would be vacuous",
            cb.child_owner_exit_address
        ));
    }
    mercuryrustlib::tesr::verify_conveyed_child(cc, &mine, cb)
        .await
        .map_err(|e| anyhow!("{where_}: the REAL receiver path refused this leaf: {e}"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    // ── THE SCHEDULE, TAKEN FROM `TesrParams` AND NOWHERE ELSE ───────────────────────────────────
    // Every count below is derived. The one literal is the assertion that this IS the regtest
    // preset: if the schedule ever changes, this test must say so loudly rather than quietly walk a
    // different number of hops and still print SUCCESS.
    let p = TesrParams::for_network("regtest");
    assert_eq!(
        p,
        TesrParams::regtest(),
        "SDK_E2E=84 runs on the regtest schedule; every hop and epoch count below is derived from it"
    );
    let hops_per_epoch = ((p.d0 - p.d_floor) / p.delta) as usize; // (24 − 6) / 6 = 3
    let epochs = p.m_max as usize + 1; // m = 0 is ITSELF an epoch, so m_max + 1 = 3
    let total_hops = hops_per_epoch * epochs; // 9
    let renewals = epochs - 1; // 2 — the moves BETWEEN epochs
    assert!(hops_per_epoch > 0 && epochs > 1, "the schedule must allow at least one renewal");
    println!(
        "SDK84 - schedule: d0={} δ={} d_floor={} | e0={} δE={} e_floor={} m_max={} \
         ⟹ {hops_per_epoch} hop(s)/epoch × {epochs} epoch(s) = {total_hops} hops per depth level, \
         via {renewals} renewal(s)",
        p.d0, p.delta, p.d_floor, p.e0, p.delta_e, p.e_floor, p.m_max
    );

    // One fresh holder per hop, plus the one the split conveys to. See the module note: a wallet that
    // re-receives a statechain id it already holds a WITHDRAWN row for would make every
    // `duplicate_index == 0` assertion below ambiguous.
    let alice = v2_wallet("sdk84_alice").await?;
    // A destination for the transfers that must be REFUSED. It is its own wallet so the refusal is
    // never confounded with a self-send, and it must end the run holding nothing.
    let sink = v2_wallet("sdk84_sink").await?;
    let mut holders: Vec<(String, UtexoWallet)> = Vec::new();
    for i in 0..=total_hops {
        let name = format!("sdk84_h{i}");
        let w = v2_wallet(&name).await?;
        holders.push((name, w));
    }

    // =============================================================================================
    // [1] alice deposits, `claim()` LADDERS the coin, and a NON-EXACT payment splits it in-ladder so
    //     h0 holds a LEAF minted at `state_csv(0)`.
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
    let alice_sid = coins_of(&cc, "sdk84_alice")
        .await?
        .into_iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id)
        .ok_or_else(|| anyhow!("alice has no confirmed coin"))?;
    let parent_bundle = mercuryrustlib::tesr::load(&cc, "sdk84_alice", &alice_sid)
        .await?
        .ok_or_else(|| anyhow!("claim() must ladder a fresh confirmed root coin unconditionally"))?;
    let (f_txid, f_vout) = (parent_bundle.f_txid.clone(), parent_bundle.f_vout);
    println!(
        "SDK84 - [1] alice's {DEPOSIT}-sat deposit confirmed after {waited} claim pass(es) and was \
         LADDERED: sid {} over F = {f_txid}:{f_vout}",
        alice_sid.chars().take(8).collect::<String>()
    );

    let h0_addr =
        mercuryrustlib::transfer_receiver::new_transfer_address(&cc, &holders[0].0).await?;
    let r = alice.transfer(&h0_addr, PAY).await?;
    assert!(
        r.used_split,
        "a {PAY}-sat payment out of one {DEPOSIT}-sat LADDERED coin must split in-ladder — without a \
         split there is no leaf and this test has nothing to renew"
    );
    println!(
        "SDK84 - [1] the non-exact {PAY}-sat payment out of {DEPOSIT} sat took the IN-LADDER SPLIT \
         lane (used_split={}, total_sats={}) and conveyed a leaf to sdk84_h0",
        r.used_split, r.total_sats
    );
    let leaf_sid = {
        // The child's statechain id is whichever CONFIRMED `ctesr-` row h0 just adopted.
        let cb = claim_leaf_any(&cc, &holders[0].1, &holders[0].0).await?;
        cb.child_statechain_id.clone()
    };
    let minted = mercuryrustlib::tesr::load_child(&cc, &holders[0].0, &leaf_sid)
        .await?
        .ok_or_else(|| anyhow!("h0 did not adopt the leaf bundle"))?;
    assert!(
        minted.ancestors.is_empty(),
        "this test is written for a DEPTH-1 leaf (`sp_vout` relative to `parent.current().state`); a \
         deeper one would need `segment_funding_tier`'s convention and is a different test"
    );
    assert!(minted.rgb.is_none(), "a PLAIN leaf — the coloured lane has its own renewal builder");

    // The leaf is minted at the top of the state schedule and the top of the extension schedule.
    let sp_tier = minted.parent.current().state.clone();
    let sp_txid = parse_tx(&sp_tier.signed_tx)?.txid().to_string();
    let leaf_outpoint = format!("{sp_txid}:{}", minted.sp_vout);
    assert_eq!(
        signed_csv(&minted.child_state)?,
        p.state_csv(0),
        "a leaf is minted at `state_csv(0)` — that is the premise of the whole budget argument"
    );
    assert_eq!(
        signed_csv(&minted.child_extension)?,
        p.ext_csv(0),
        "a leaf is minted at `ext_csv(0)`, which is what makes its renewal epoch DERIVABLE"
    );
    assert_eq!(
        mercuryrustlib::tesr::child_renewal_epoch(&minted)?,
        0,
        "a freshly minted leaf is at renewal epoch 0 — DERIVED from the signed nSequence, because a \
         leaf carries no `m` field and must never grow one"
    );
    assert_eq!(
        tier_prevout(&minted.child_extension)?,
        leaf_outpoint,
        "the leaf's extension spends SP.out[j] — the outpoint every renewal below rebuilds over"
    );
    assert!(!tx_known(&cc, &sp_txid)?, "SP is un-broadcast: the split is entirely off-chain");
    let mut expected_sigs = assert_census_balances(&cc, &minted, "[1] freshly minted leaf").await?;
    assert_eq!(expected_sigs, 2, "a fresh leaf has exactly its two tiers co-signed");
    println!(
        "SDK84 - [1] h0 holds a LEAF ({leaf_sid}) worth {PAY} sat over SP.out[{}] = {leaf_outpoint}, \
         minted at state CSV {} / extension CSV {} (epoch 0), num_sigs {expected_sigs}; F = \
         {f_txid}:{f_vout} is the only on-chain funding and SP is still off-chain",
        minted.sp_vout,
        p.state_csv(0),
        p.ext_csv(0)
    );

    // =============================================================================================
    // [2]/[5] THE HOPS, and [3]/[6] the refusals at each floor, epoch by epoch.
    //
    // Each epoch runs the leaf down to its state floor, proves the NEXT hop is refused and that the
    // refusal names RENEWAL, then renews and proves the budget came back. The last epoch's refusal
    // is the EXHAUSTION one instead — there is no lower extension rung to renew onto.
    // =============================================================================================
    let mut holder = 0usize; // index into `holders`; h0 already holds the leaf
    let mut hops_done = 0usize;
    let mut renewals_done = 0usize;

    for epoch in 0..epochs {
        // ── the hops of this epoch ────────────────────────────────────────────────────────────────
        for k in 0..hops_per_epoch {
            let from_name = holders[holder].0.clone();
            let from_w = &holders[holder].1;
            let before = mercuryrustlib::tesr::load_child(&cc, &from_name, &leaf_sid)
                .await?
                .ok_or_else(|| anyhow!("{from_name} lost the leaf bundle"))?;
            let csv_before = signed_csv(&before.child_state)?;
            let ext_before = signed_csv(&before.child_extension)?;

            let to = holder + 1;
            let to_name = holders[to].0.clone();
            let to_w = &holders[to].1;
            let to_addr =
                mercuryrustlib::transfer_receiver::new_transfer_address(&cc, &to_name).await?;
            let res = from_w
                .transfer(&to_addr, PAY)
                .await
                .map_err(|e| anyhow!("hop {} ({from_name} -> {to_name}) failed: {e:#}", hops_done + 1))?;
            assert!(!res.used_split, "a WHOLE-leaf hand-on must not split");
            assert_eq!(res.total_sats, PAY, "the whole leaf value moves");

            let after = claim_leaf(&cc, to_w, &to_name, &leaf_sid).await?;
            hops_done += 1;
            holder = to;

            // One hop = one co-signature and one newly disclosed superseded state. Nothing else moves.
            expected_sigs += 1;
            let issued =
                assert_census_balances(&cc, &after, &format!("[hop {hops_done}] after the hand-on")).await?;
            assert_eq!(
                issued, expected_sigs,
                "hop {hops_done} must cost EXACTLY one co-signature"
            );
            assert_eq!(
                after.child_superseded_states.len(),
                before.child_superseded_states.len() + 1,
                "the replaced state is disclosed, not hidden"
            );
            assert_eq!(
                after.child_superseded_extensions.len(),
                before.child_superseded_extensions.len(),
                "a HOP replaces only the STATE — the extension is shared across the whole ownership \
                 history, which is precisely why the renewal guard has to be strict"
            );
            // Replace-by-lower-timelock, per hop.
            let csv_after = signed_csv(&after.child_state)?;
            assert_eq!(
                csv_after,
                csv_before - p.delta,
                "each whole-coin hop costs exactly one δ off the state rung"
            );
            assert_eq!(
                signed_csv(&after.child_extension)?,
                ext_before,
                "a hop must not move the extension rung — the derived epoch is read off it"
            );
            assert_eq!(
                tier_prevout(&after.child_extension)?,
                leaf_outpoint,
                "the leaf still hangs off the same SP.out[j] after {hops_done} hop(s)"
            );
            assert!(
                !tx_known(&cc, &sp_txid)?,
                "hop {hops_done} was OFF-CHAIN: SP has still never been broadcast"
            );
            assert!(
                outpoint_unspent(&cc, &f_txid, f_vout)?,
                "hop {hops_done} was OFF-CHAIN: F is still unspent"
            );
            assert_eq!(
                from_w.get_balance().await?.available_sats,
                0,
                "{from_name} paid the whole leaf away"
            );
            assert_eq!(
                to_w.get_balance().await?.available_sats,
                PAY,
                "{to_name} books the whole leaf"
            );
            // The last hop of an epoch must land ON the floor — that is what makes the refusal below
            // a refusal about the BUDGET rather than about anything else.
            if k + 1 == hops_per_epoch {
                assert_eq!(csv_after, p.d_floor, "the epoch's last hop lands on the state floor");
                assert!(
                    mercuryrustlib::tesr::child_needs_renewal(&after)?,
                    "at the floor the leaf must SAY it needs renewing"
                );
            } else {
                assert!(
                    !mercuryrustlib::tesr::child_needs_renewal(&after)?,
                    "with hops left in the epoch the leaf must not claim it needs renewing"
                );
            }
            println!(
                "SDK84 - [{}] hop {hops_done}/{total_hops} (epoch {epoch}, hop {}/{hops_per_epoch}): \
                 leaf {} moved {PAY} sat {from_name} -> {to_name}; state CSV {csv_before} -> \
                 {csv_after}, extension CSV {ext_before} unmoved over {leaf_outpoint}; num_sigs \
                 {expected_sigs}, {} superseded state(s) disclosed",
                if epoch == 0 { "2" } else { "5" },
                k + 1,
                leaf_sid.chars().take(8).collect::<String>(),
                after.child_superseded_states.len()
            );
        }
        println!(
            "SDK84 - [{}] epoch {epoch}: {hops_per_epoch} whole-leaf hop(s) took the state rung {} -> \
             {} (the floor); num_sigs {expected_sigs}; SP still un-broadcast",
            if epoch == 0 { "2" } else { "5" },
            p.state_csv(0),
            p.d_floor
        );

        // ── the refusal at the floor ──────────────────────────────────────────────────────────────
        let (holder_name, holder_w) = (holders[holder].0.clone(), &holders[holder].1);
        let at_floor = mercuryrustlib::tesr::load_child(&cc, &holder_name, &leaf_sid)
            .await?
            .ok_or_else(|| anyhow!("{holder_name} lost the leaf bundle"))?;
        let dead_end_addr =
            mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk84_sink").await?;
        let sigs_before_refusal = num_sigs(&cc, &leaf_sid).await?;
        let msg = refused_with(
            holder_w.transfer(&dead_end_addr, PAY).await,
            "a whole-leaf transfer with the state rung ON the floor",
            "RENEW IT",
        )?;
        assert!(
            msg.contains("renew_child"),
            "[3] the refusal must name the remedy a leaf ACTUALLY has: {msg}"
        );
        assert!(
            msg.contains(&p.state_csv(0).to_string()),
            "[3] the refusal must say what renewal restores the rung TO: {msg}"
        );
        assert!(
            !msg.contains("exit or re-anchor it instead of re-sending"),
            "[3] REGRESSION: the old refusal text is back. A leaf CANNOT re-anchor — `refresh()` \
             routes a `ctesr-` coin to `unilateral_exit` because `SP.out[j]` is un-broadcast — so \
             naming one told every leaf holder in the system to do something impossible: {msg}"
        );
        assert_eq!(
            num_sigs(&cc, &leaf_sid).await?,
            sigs_before_refusal,
            "[3] a refused transfer must burn NO co-signature"
        );
        assert_eq!(
            coin_of(&cc, &holder_name, &leaf_sid).await?.status,
            CoinStatus::CONFIRMED,
            "[3] the refused transfer must leave the leaf CONFIRMED — `child_retransfer` refuses at \
             the floor BEFORE its durable IN_TRANSFER mark, and a leaf parked for a hop that never \
             happened is a leaf the watchtower has stopped defending (and one `renew_child`'s own \
             [L1] liveness allowlist would then refuse)"
        );
        sink.claim().await?;
        assert_eq!(
            sink.get_balance().await?.available_sats,
            0,
            "[3] a REFUSED transfer must convey nothing — the sink was never paid"
        );
        println!(
            "SDK84 - [3] the next whole-leaf transfer is REFUSED at the floor, and the refusal names \
             RENEWAL: {msg}"
        );

        // ── the last epoch has nothing to renew onto: that refusal is the EXHAUSTION one ──────────
        if epoch + 1 == epochs {
            assert!(
                mercuryrustlib::tesr::child_needs_rollover(&at_floor)?,
                "[6] at the last epoch the leaf must SAY its extension schedule is spent"
            );
            let mut coin = coin_of(&cc, &holder_name, &leaf_sid).await?;
            let msg = refused_with(
                mercuryrustlib::tesr::renew_child_auto(&cc, &holder_name, &mut coin, &at_floor).await,
                "renewing a leaf whose extension schedule is spent",
                "child_in_ladder_split",
            )?;
            assert!(
                msg.contains("cannot re-anchor"),
                "[6] `re-anchor` may appear ONLY as an explicit denial — it is the remedy this whole \
                 lane exists because a leaf does NOT have: {msg}"
            );
            assert!(
                msg.contains("Split it, or exit it"),
                "[6] the exhaustion refusal must name the two things a spent leaf can still do: {msg}"
            );
            assert_eq!(
                num_sigs(&cc, &leaf_sid).await?,
                expected_sigs,
                "[6] the exhaustion refusal is taken in the PURE planner — no co-signature burned"
            );
            assert!(
                mercuryrustlib::tesr::child_renewal_open(&cc, &holder_name).await?.is_empty(),
                "[6] a refusal that never signed must leave no unfinished `{}` journal row",
                mercuryrustlib::tesr::CHILD_RENEWAL_JOURNAL_PREFIX
            );
            println!(
                "SDK84 - [6] the extension schedule is SPENT after {renewals_done} renewal(s) \
                 (epoch {} of {}), and the refusal names `child_in_ladder_split` — the leaf's \
                 analogue of `rollover` — never a re-anchor: {msg}",
                mercuryrustlib::tesr::child_renewal_epoch(&at_floor)?,
                p.m_max
            );
            break;
        }

        // =========================================================================================
        // [4] THE RENEWAL. Two SE co-signatures, ZERO on-chain bytes, no depth.
        // =========================================================================================
        let before = at_floor;
        let epoch_before = mercuryrustlib::tesr::child_renewal_epoch(&before)?;
        let ext_before = signed_csv(&before.child_extension)?;
        let mut coin = coin_of(&cc, &holder_name, &leaf_sid).await?;
        let (renewed, rollover_due) =
            mercuryrustlib::tesr::renew_child_auto(&cc, &holder_name, &mut coin, &before)
                .await
                .map_err(|e| anyhow!("[4] the leaf renewal FAILED at epoch {epoch_before}: {e:#}"))?;
        renewals_done += 1;
        expected_sigs += 2;

        // (a) THE BUDGET IS BACK — the state rung is at the top of the schedule again.
        assert_eq!(
            signed_csv(&renewed.child_state)?,
            p.state_csv(0),
            "[4] a renewal resets the state rung to `state_csv(0)` — that IS the budget coming back"
        );
        // (b) STRICTLY LOWER EXTENSION — the safety property, on the SIGNED nSequence.
        let ext_after = signed_csv(&renewed.child_extension)?;
        assert!(
            ext_after < ext_before,
            "[4] the renewed extension (CSV {ext_after}) must mature STRICTLY before the one it \
             supersedes (CSV {ext_before}) — they spend the SAME SP.out[j], so exactly one can ever \
             confirm and it must be the current owner's"
        );
        assert_eq!(
            ext_after,
            p.ext_csv(epoch_before + 1),
            "[4] the renewal lands on the schedule's NEXT rung, not an ad-hoc lower number"
        );
        assert_eq!(
            mercuryrustlib::tesr::child_renewal_epoch(&renewed)?,
            epoch_before + 1,
            "[4] the DERIVED epoch advances by exactly one — and it is derived, never declared"
        );
        // (c) THE SAME OUTPOINT, AND NO DEPTH SPENT.
        assert_eq!(
            tier_prevout(&renewed.child_extension)?,
            leaf_outpoint,
            "[4] the renewed extension rebuilds over the SAME SP.out[j] — that is what makes it a \
             renewal rather than a new level"
        );
        assert_eq!(
            renewed.sp_vout, before.sp_vout,
            "[4] same SP output funds the leaf"
        );
        assert_eq!(
            renewed.ancestors.len(),
            before.ancestors.len(),
            "[4] DEPTH UNCHANGED — a renewal must not add a segment"
        );
        assert_eq!(
            renewed.parent.levels.len(),
            before.parent.levels.len(),
            "[4] DEPTH UNCHANGED — the parent ladder is untouched"
        );
        assert_eq!(
            mercuryrustlib::tesr::child_exit_chain(&renewed).len(),
            mercuryrustlib::tesr::child_exit_chain(&before).len(),
            "[4] the exit chain is the SAME LENGTH — a renewal costs no extra tier to walk"
        );
        assert_eq!(
            renewed.child_owner_exit_address, before.child_owner_exit_address,
            "[4] a renewal is not a payment — the leaf still pays its current owner"
        );
        assert_eq!(
            renewed.child_state.out_value, before.child_state.out_value,
            "[4] ZERO COST: both tiers are rebuilt at the same committed fee, so the leaf is worth \
             exactly what it was worth"
        );
        // (d) ZERO ON-CHAIN BYTES.
        assert!(!tx_known(&cc, &sp_txid)?, "[4] the renewal broadcast nothing: SP is still off-chain");
        assert!(
            !tx_known(&cc, &renewed.child_extension.txid)?,
            "[4] the renewal broadcast nothing: the replacement extension is un-broadcast"
        );
        assert!(outpoint_unspent(&cc, &f_txid, f_vout)?, "[4] the renewal broadcast nothing: F unspent");
        // (e) FULL DISCLOSURE, and the census still balances EXACTLY.
        assert_eq!(
            renewed.child_superseded_extensions.len(),
            before.child_superseded_extensions.len() + 1,
            "[4] the replaced EXTENSION is disclosed — it consumed a real co-signature"
        );
        assert_eq!(
            renewed.child_superseded_states.len(),
            before.child_superseded_states.len() + 1,
            "[4] the replaced STATE is disclosed too — both tiers were co-signed"
        );
        let sup_ext = renewed
            .child_superseded_extensions
            .last()
            .ok_or_else(|| anyhow!("no superseded extension recorded"))?;
        assert_eq!(
            tier_prevout(sup_ext)?,
            leaf_outpoint,
            "[4] the superseded extension spends the SAME outpoint as the live one — DIRECT \
             contention, which is why its CSV has to lose"
        );
        assert!(
            signed_csv(sup_ext)? > ext_after,
            "[4] the superseded extension (CSV {}) must lose the race to the live one (CSV \
             {ext_after}) — this is the exact comparison `verify_superseded_segment` makes",
            signed_csv(sup_ext)?
        );
        let issued = assert_census_balances(&cc, &renewed, "[4] after the renewal").await?;
        assert_eq!(
            issued, expected_sigs,
            "[4] a renewal costs EXACTLY two co-signatures and discloses exactly two more superseded \
             tiers, so the exact-equality census moves by +2 on both sides"
        );
        // (f) THE REAL RECEIVER PATH still accepts it — the renewed shape needs NO format change.
        let exit_value = verifies_at_the_receiver(&cc, &holder_name, &renewed, "[4]").await?;
        assert_eq!(
            exit_value, renewed.child_state.out_value,
            "[4] the receiver's census-bound exit value is the renewed state's committed value"
        );
        // (g) the journal closed, and the leaf knows where it stands.
        assert!(
            mercuryrustlib::tesr::child_renewal_open(&cc, &holder_name).await?.is_empty(),
            "[4] a COMPLETED renewal must leave no unfinished `{}` journal row",
            mercuryrustlib::tesr::CHILD_RENEWAL_JOURNAL_PREFIX
        );
        assert!(
            !mercuryrustlib::tesr::child_needs_renewal(&renewed)?,
            "[4] a just-renewed leaf must not still claim it needs renewing"
        );
        assert_eq!(
            rollover_due,
            p.needs_rollover(epoch_before + 1),
            "[4] `renew_child_auto` must report the rollover verdict for the epoch it LANDED on"
        );
        println!(
            "SDK84 - [4] RENEWED in place over {leaf_outpoint}: state CSV {} -> {} (budget restored), \
             extension CSV {ext_before} -> {ext_after} (epoch {epoch_before} -> {}), depth unchanged, \
             0 on-chain bytes, +2 co-signatures (num_sigs {issued}), census balances, and the REAL \
             receiver path still accepts it",
            p.d_floor,
            p.state_csv(0),
            epoch_before + 1
        );

        // =========================================================================================
        // [N1] (the brief's step 7) A RENEWAL THAT DOES NOT STRICTLY LOWER THE EXTENSION RUNG IS
        //      REFUSED, AND NOTHING IS BURNED.
        //
        // Run HERE, immediately after the first renewal, and not at the end: `plan_child_renewal`
        // checks exhaustion (`needs_rollover`) BEFORE the strict-decrease guard, so at the end of the
        // schedule the exhaustion refusal would fire first and this step would pass while proving
        // nothing about the guard it is named for. The leaf must still be renewable for this to mean
        // anything.
        //
        // The plain root `renew` has NO such guard: it will co-sign an extension at an equal or
        // HIGHER CSV and the coin is discovered BRICKED only at the receiver's
        // `verify_superseded_segment` — after two irreversible co-signatures are gone and `num_sigs`
        // is permanently ahead of any persistable bundle. The leaf lane copies the COLOURED sibling's
        // guard instead, and it lives in the PURE planner so a refusal is provably free.
        // =========================================================================================
        if renewals_done == 1 {
            let cb = mercuryrustlib::tesr::load_child(&cc, &holder_name, &leaf_sid)
                .await?
                .ok_or_else(|| anyhow!("the renewed leaf vanished"))?;
            let live = mercuryrustlib::tesr::child_extension_csv(&cb)?;
            let sigs_before = num_sigs(&cc, &leaf_sid).await?;
            // The OFF-GRID case below is `live − 1`, which is only off-grid when the decrement is
            // bigger than one. Asserted rather than assumed: on a schedule with `δE == 1` that case
            // would quietly become a LEGAL renewal and the loop would fail with "was ACCEPTED",
            // blaming the guard instead of the test.
            assert!(
                p.delta_e > 1,
                "[N1] the off-grid case needs δE > 1 to be off-grid at all (δE = {})",
                p.delta_e
            );
            assert_eq!(
                p.ext_csv(p.m_max + 1),
                p.e_floor,
                "[N1] the clamped case below is only the FLOOR case if `ext_csv` really saturates \
                 there — that saturation is exactly why the `epoch_after > m_max` guard exists"
            );

            for (attack, csv_e, expect) in [
                (
                    "a renewal at the SAME extension rung",
                    live,
                    "must lower the extension rung STRICTLY",
                ),
                (
                    "a renewal at a HIGHER extension rung",
                    live + p.delta_e,
                    "must lower the extension rung STRICTLY",
                ),
                (
                    "a LOWER but OFF-GRID renewal rung, which the strict-decrease guard alone would \
                     wave through (`e0 − 1` is perfectly 'lower') and which would leave a leaf whose \
                     epoch is underivable forever",
                    live - 1,
                    "not a multiple of the decrement",
                ),
                (
                    "a renewal onto the CLAMPED extension floor, which is on the grid, in band, and \
                     strictly lower — and forfeits every remaining epoch at once",
                    p.ext_csv(p.m_max + 1),
                    "the design never operates there",
                ),
            ] {
                let mut coin = coin_of(&cc, &holder_name, &leaf_sid).await?;
                let msg = refused_with(
                    mercuryrustlib::tesr::renew_child(
                        &cc,
                        &holder_name,
                        &mut coin,
                        &cb,
                        csv_e,
                        p.state_csv(0),
                    )
                    .await,
                    attack,
                    expect,
                )?;
                println!("SDK84 - [N1] {attack} (CSV {csv_e}) is REFUSED: {msg}");
            }

            // NOTHING WAS BURNED, and nothing was written. Every one of those refusals is taken in
            // the pure planner, which cannot reach the SE and cannot mutate the bundle.
            assert_eq!(
                num_sigs(&cc, &leaf_sid).await?,
                sigs_before,
                "[N1] a refused renewal must burn NO co-signature — the leaf's census is exact \
                 equality, so a signature taken and then refused could never be rebalanced"
            );
            assert!(
                mercuryrustlib::tesr::child_renewal_open(&cc, &holder_name).await?.is_empty(),
                "[N1] a refused renewal must write no unfinished `{}` journal row",
                mercuryrustlib::tesr::CHILD_RENEWAL_JOURNAL_PREFIX
            );
            let unchanged = mercuryrustlib::tesr::load_child(&cc, &holder_name, &leaf_sid)
                .await?
                .ok_or_else(|| anyhow!("the leaf vanished after a refused renewal"))?;
            assert_eq!(
                unchanged.child_extension.txid, cb.child_extension.txid,
                "[N1] a refused renewal must leave the stored bundle untouched"
            );
            assert_eq!(
                unchanged.child_state.txid, cb.child_state.txid,
                "[N1] a refused renewal must leave the stored bundle untouched"
            );
            assert_eq!(
                disclosed_superseded(&unchanged),
                disclosed_superseded(&cb),
                "[N1] a refused renewal must disclose nothing new"
            );
            assert_eq!(
                coin_of(&cc, &holder_name, &leaf_sid).await?.status,
                CoinStatus::CONFIRMED,
                "[N1] a refused renewal must leave the leaf CONFIRMED and defended"
            );
            verifies_at_the_receiver(&cc, &holder_name, &unchanged, "[N1] after four refusals")
                .await?;
        }
    }

    assert_eq!(hops_done, total_hops, "every scheduled hop must have been taken");
    assert_eq!(renewals_done, renewals, "every available renewal must have been used");

    // =============================================================================================
    // [8] THE SAFETY PROPERTY, ON CHAIN. The renewed leaf still exits unilaterally on its OWN
    //     pre-signed tiers — no fresh SE signature (`exit_child_pass` is not even `async` and has no
    //     `ClientConfig`) — and the transaction that actually spends `SP.out[j]` is the RENEWED
    //     extension. Neither superseded extension ever reaches the chain: they spend the same
    //     outpoint at a strictly HIGHER CSV, so the current owner's matures first and wins.
    // =============================================================================================
    let (final_name, final_w) = (holders[holder].0.clone(), &holders[holder].1);
    let final_cb = mercuryrustlib::tesr::load_child(&cc, &final_name, &leaf_sid)
        .await?
        .ok_or_else(|| anyhow!("the final holder lost the leaf bundle"))?;
    assert_eq!(
        final_cb.child_superseded_extensions.len(),
        renewals,
        "[8] every renewal disclosed the extension it replaced"
    );
    assert_eq!(
        final_cb.child_superseded_states.len(),
        total_hops + renewals,
        "[8] one superseded state per hop, plus one per renewal"
    );
    let live_ext_txid = final_cb.child_extension.txid.clone();
    let dead_ext_txids: Vec<String> = final_cb
        .child_superseded_extensions
        .iter()
        .map(|t| t.txid.clone())
        .collect();
    // Every superseded extension is a RIVAL for the very same outpoint, at a strictly higher CSV.
    for (i, t) in final_cb.child_superseded_extensions.iter().enumerate() {
        assert_eq!(
            tier_prevout(t)?,
            leaf_outpoint,
            "[8] superseded extension {i} rivals the live one over {leaf_outpoint}"
        );
        assert!(
            signed_csv(t)? > signed_csv(&final_cb.child_extension)?,
            "[8] superseded extension {i} (CSV {}) must lose the maturity race to the live one \
             (CSV {})",
            signed_csv(t)?,
            signed_csv(&final_cb.child_extension)?
        );
    }
    let exit_spk = electrum_client::bitcoin::Address::from_str(&final_cb.child_owner_exit_address)?
        .require_network(electrum_client::bitcoin::Network::Regtest)?
        .script_pubkey();
    assert!(
        cc.electrum_client.script_list_unspent(&exit_spk)?.is_empty(),
        "[8] the final holder's exit address must start EMPTY, or the 'the value landed here' \
         assertion below proves nothing"
    );

    println!(
        "SDK84 - [8] starting the unilateral exit of leaf {} from {final_name}: live extension {} \
         (CSV {}) vs {} superseded extension(s) [{}] all rivalling {leaf_outpoint}; exit address \
         starts empty",
        leaf_sid.chars().take(8).collect::<String>(),
        live_ext_txid.chars().take(8).collect::<String>(),
        signed_csv(&final_cb.child_extension)?,
        dead_ext_txids.len(),
        dead_ext_txids
            .iter()
            .map(|t| t.chars().take(8).collect::<String>())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut passes = 0;
    loop {
        let st = final_w.unilateral_exit(Some(vec![leaf_sid.clone()]), None).await?;
        let s = st
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no exit status for the renewed leaf"))?;
        if s.complete {
            println!("SDK84 - [8] the renewed leaf EXITED unilaterally after {passes} pass(es)");
            break;
        }
        // Each pass waits out ONE tier's relative timelock; the chain is
        // F -> T -> X_m -> SP -> ext_child -> state_child, so a single block per pass would never
        // mature them.
        bitcoin_core::generatetoaddress(s.wait_blocks.max(1) + 1, &core)?;
        passes += 1;
        if passes > 60 {
            return Err(anyhow!(
                "the renewed leaf did not exit — a renewal that strands the leaf it renewed is a \
                 renewal that must not ship"
            ));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The chain settles the race. `SP.out[j]` is spent, and it is spent by the RENEWED extension.
    assert!(
        !outpoint_unspent(&cc, &sp_txid, final_cb.sp_vout)?,
        "[8] the exit consumed SP.out[j]"
    );
    assert!(
        tx_known(&cc, &live_ext_txid)?,
        "[8] the transaction that reached the chain is the LIVE (renewed) extension {live_ext_txid}"
    );
    for (i, dead) in dead_ext_txids.iter().enumerate() {
        assert!(
            !tx_known(&cc, dead)?,
            "[8] SAFETY: superseded extension {i} ({dead}) reached the chain. Every extension this \
             leaf ever had spends the SAME SP.out[j], so exactly one can confirm — if a superseded \
             one wins, every previous owner's retained state comes alive underneath it and the \
             current owner is robbed. This is the property the strict-decrease guard exists for."
        );
    }
    assert!(
        !outpoint_unspent(&cc, &f_txid, f_vout)?,
        "[8] only the final unilateral exit touches the chain — F is spent at the very end"
    );
    let landed = cc.electrum_client.script_list_unspent(&exit_spk)?;
    assert_eq!(
        landed.len(),
        1,
        "[8] exactly one UTXO at the final holder's own exit key; got {}",
        landed.len()
    );
    assert_eq!(
        landed[0].value, final_cb.child_state.out_value,
        "[8] the value that lands is the renewed state's committed value"
    );
    println!(
        "SDK84 - [8] the chain settled the race: SP.out[{}] ({sp_txid}:{}) was spent by the RENEWED \
         extension {live_ext_txid}, none of the {} superseded extension(s) reached the chain, F \
         {f_txid}:{f_vout} is spent, and {} sat landed at the final holder's own exit address",
        final_cb.sp_vout,
        final_cb.sp_vout,
        dead_ext_txids.len(),
        landed[0].value
    );

    println!(
        "SDK84 - SUCCESS: a LEAF spent its whole transfer budget, was RENEWED IN PLACE over the same \
         SP.out[{}] for ZERO on-chain bytes and no depth, and got the budget back — {total_hops} \
         whole-leaf hops across {epochs} epochs via {renewals} renewal(s), where before renewal \
         existed the leaf was dead after {hops_per_epoch} and was told to do something a leaf cannot \
         do. The census balanced exactly at every step ({expected_sigs} co-signatures issued = 2 live \
         tiers + {} disclosed superseded tiers), the REAL receiver path accepted every renewed \
         bundle, a renewal that did not strictly lower the extension rung was refused with nothing \
         burned, and when the leaf finally exited the transaction that took SP.out[{}] was the \
         RENEWED extension — neither superseded extension ever reached the chain. The whole flow \
         touched Bitcoin exactly twice: one deposit and one exit.",
        final_cb.sp_vout,
        disclosed_superseded(&final_cb),
        final_cb.sp_vout
    );
    Ok(())
}

/// Poll `claim()` until `w` adopts ANY `ctesr-` leaf, and return it.
///
/// Used exactly once — right after the in-ladder split, when the child's statechain id is not yet
/// known to this test. Every later hop knows the id and uses [`claim_leaf`], which is strictly
/// stronger because it names the row it is waiting for.
async fn claim_leaf_any(cc: &ClientConfig, w: &UtexoWallet, name: &str) -> Result<ChildTesrBundle> {
    for _ in 0..60 {
        w.claim().await?;
        for c in coins_of(cc, name).await? {
            if c.status != CoinStatus::CONFIRMED || c.duplicate_index != 0 {
                continue;
            }
            if let Some(sid) = c.statechain_id.as_deref() {
                if let Some(cb) = mercuryrustlib::tesr::load_child(cc, name, sid).await? {
                    return Ok(cb);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!(
        "{name} adopted no in-ladder split CHILD — a payment that took the PLAIN split lane [B1] \
         would leave a coin with no `ctesr-` row, and there would be no leaf to renew"
    ))
}
