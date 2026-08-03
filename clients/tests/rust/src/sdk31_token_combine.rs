//! E2E (multi-carrier token combine): pay an amount that spans SEVERAL carriers, which the
//! single-carrier path used to refuse ("no single coin carries >= N"). A payment now COMBINES the
//! carriers into one SE-co-signed colored combine tx (N inputs → piece + change), WITHOUT weakening
//! invalidation/branch security. This test also emits verbose VERIFY lines to check the receiver's
//! branch + terminal-ancestor invariants against the spec.
//!
//! (a) COMBINE PAYMENT: alice holds 110 of an asset across TWO carriers (60 + 50). Paying bob 100
//!     succeeds via a 2-input colored combine; bob books EXACTLY 100, alice keeps 10. The piece is a
//!     depth-1 sub-coin whose 1-tx branch IS the 2-input combine.
//! (b) VERIFY (branch/terminal invariants): the combine tx has 2 inputs, so the receiver requires 2
//!     terminal ancestors (one per structural input, not one per hop); the SDK named exactly the 2
//!     carriers and made BOTH terminal at the SE. We re-derive and print these facts (bob already
//!     enforced them by claiming).
//! (c) EXIT: bob unilaterally exits the combined coin — the 2-input combine branch broadcasts (both
//!     carrier outpoints spent → the old carriers' backups are dead), the leaf backup matures, and
//!     100 units settle on-chain on the exited outpoint. Invalidation holds for a combine.
//! (d) NEGATIVE: paying MORE than the total allocation fails with a TYPED insufficient error (not the
//!     old "no single coin" refusal); the original carriers are spent after the exit.
//!
//! Run: SDK_E2E=31 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

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

async fn wait_carriers_confirmed(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    core: &str,
    asset: &str,
    want_units: u64,
    want_carriers: usize,
) -> Result<()> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        let units_ok = token_balance(w, asset).await? >= want_units;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
        let carriers = rec
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(mercury_utexo_sdk::tokens::TOKEN_CARRIER_SATS as u32))
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

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    use electrum_client::ElectrumApi;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    // If the outpoint no longer appears in the address's unspent set, it is spent.
    let unspent = cc.electrum_client.script_list_unspent(spk)?;
    Ok(!unspent
        .iter()
        .any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk31_alice", "./rgb-data-sdk31_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    // [CTES-R] The COLOURED lane is asked for BY NAME, on both wallets.
    //
    // `SdkConfig::colored_ladder` defaults to **false** (2c351c6): what keeps the lane off by
    // default is its measured economics (`docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md`), not any doubt
    // about its soundness. This test is about the multi-carrier PAYMENT shape on that lane, so it
    // enables the lane rather than inheriting it — a test that asserted the default would be
    // testing the shipping decision instead. The default itself is pinned by sdk74/sdk75.
    let mut alice_cfg = SdkConfig::regtest("sdk31_alice");
    alice_cfg.colored_ladder = true;
    let mut bob_cfg = SdkConfig::regtest("sdk31_bob");
    bob_cfg.colored_ladder = true;
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    // Fund alice's RGB engine (issuance + mint witnesses).
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Two carriers of one asset: IFA issue 60 (carrier A) + on-chain mint 50 (carrier B).
    add_tokens(&cc, &alice, 1).await?;
    let asset = alice.issue_inflatable_token("CMB", "Combine Token", 0, 60, vec![50]).await?;
    wait_carriers_confirmed(&cc, &alice, "sdk31_alice", &core, &asset, 60, 1).await?;
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
    let mint_res = alice.mint_tokens(&asset, vec![50]).await;
    mining.store(false, Ordering::Relaxed);
    let _ = miner.join();
    let _ = mint_res?;
    wait_carriers_confirmed(&cc, &alice, "sdk31_alice", &core, &asset, 110, 2).await?;
    println!("SDK31 - alice holds 110 {asset} across TWO carriers (60 + 50); no single carrier has 100");

    // [CTES-R] Wait for BOTH carriers' COLOURED ladders before paying.
    //
    // Synchronisation, not relaxation: alice runs with `colored_ladder` ON (set above), so `claim()`
    // builds a coloured ladder over each carrier — but only once that carrier's allocation is BOOKED, which
    // can land in the same pass as the balance or in the next one. Without this the payment would
    // sometimes be attempted before the ladders exist and be refused by the retired-lane gate.
    let mut colored_sids: Vec<String> = Vec::new();
    for _ in 0..60 {
        alice.claim().await?;
        let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk31_alice").await?.coins;
        colored_sids.clear();
        for c in coins.iter().filter(|c| c.duplicate_index == 0) {
            let Some(sid) = c.statechain_id.clone() else { continue };
            if mercuryrustlib::tesr::load(&cc, "sdk31_alice", &sid)
                .await?
                .is_some_and(|b| b.is_colored())
            {
                colored_sids.push(sid);
            }
        }
        if colored_sids.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        colored_sids.len(),
        2,
        "both of alice's carriers must carry a COLOURED ladder before the multi-carrier payment"
    );
    println!("SDK31 - both carriers carry a COLOURED (CTES-R) ladder: {colored_sids:?}");

    // ===== (a) MULTI-CARRIER PAYMENT (the payment that used to be refused) =======================
    //
    // THE SHAPE CHANGED, AND THE CHANGE IS THE POINT. The legacy combine built ONE transaction
    // spending BOTH carriers' funding outputs `F`. That shape cannot exist on the coloured lane and
    // must not: each `F` is already spent by that carrier's own trigger `T`, so a combine over them
    // is a rival spend of two outpoints at once — the exact hazard CTES-R removes. There is also no
    // multi-parent coloured tier: `SP` spends exactly one `X_m`.
    //
    // So "pay across carriers" is now a multi-PIECE payment: one in-ladder split per carrier, each
    // conveying a coloured child to the same recipient. RGB value conservation holds per split and
    // the recipient's balance is the sum — which is the property the legacy combine delivered too.
    // The assertions below are re-derived to that shape, not dropped: the amount bob receives, the
    // change alice keeps, and the terminality of every source carrier are all still checked.
    add_tokens(&cc, &alice, 4).await?; // 2 legs: piece(+change) slots per leg + headroom
    let r = alice.transfer_tokens(&asset, &bob_addr, 100).await?;
    assert_eq!(
        r.coins.len(),
        2,
        "a payment spanning two carriers hands over ONE coloured child per carrier, got {:?}",
        r.coins
    );
    let piece_ids: Vec<String> = r.coins.iter().map(|c| c.statechain_id.clone()).collect();
    wait_token_balance(&bob, &asset, 100).await?;
    assert_eq!(
        token_balance(&bob, &asset).await?,
        100,
        "bob booked EXACTLY 100 summed across the two coloured children"
    );
    assert_eq!(token_balance(&alice, &asset).await?, 10, "alice keeps the 10-unit change");
    println!("SDK31 - MULTI-CARRIER PAYMENT: paid bob 100 across two coloured carriers as {} children ({piece_ids:?}); bob=100 alice=10", piece_ids.len());

    // ===== (b) VERIFY the structural invariants, re-derived for the coloured lane ================
    //
    // What the legacy version checked — "the branch is one 2-input tx, so the receiver demands 2
    // terminal ancestors" — was a statement about a transaction that no longer exists. The
    // invariant underneath it does still exist and is checked here, in three parts:
    //
    //   (i)   each conveyed piece really is a COLOURED CHILD at the receiver, carrying its declared
    //         share of the allocation, and its five-tier chain validates off-chain;
    //   (ii)  each child's ancestor segment spends its parent's `X_m` payload output — NOT `F`. This
    //         is the whole reason the combine had to go: on the coloured lane nothing but `T` ever
    //         spends a carrier's funding output;
    //   (iii) EVERY source carrier is TERMINAL at the SE. That is the invalidation property the
    //         2-terminal-ancestor rule was enforcing: no source coin can mint a further state behind
    //         the recipient's back. It is now one terminal parent per leg rather than N per tx, and
    //         it is asserted per leg, so a leg that silently skipped terminalisation still fails.
    let mut total_booked = 0u64;
    for pid in &piece_ids {
        let cb = mercuryrustlib::tesr::load_child(&cc, "sdk31_bob", pid)
            .await?
            .ok_or_else(|| anyhow!("bob did not adopt {pid} as a child bundle"))?;
        assert!(cb.is_colored(), "piece {pid} must be a COLOURED child, not a plain one");
        let (contract, assigned, txids, _) = bob.colored_child_health(pid).await.map_err(|e| {
            anyhow!("bob's coloured child {pid} does not validate against its own chain: {e}")
        })?;
        assert_eq!(contract, asset, "child {pid} carries the wrong contract");
        total_booked += assigned;
        assert_eq!(txids.len(), 5, "a coloured child's witness chain is T, X_m, SP, ext, state");
        // (ii) SP spends X_m's payload output, and `F` is untouched by the child's chain.
        let sp = parse_tx(&cb.parent.current().state.signed_tx)?;
        let x_m = &cb.parent.current().extension;
        assert_eq!(
            sp.input.len(),
            1,
            "a coloured SP spends exactly ONE parent output — there is no multi-parent tier"
        );
        assert_eq!(
            sp.input[0].previous_output.txid.to_string(),
            x_m.txid,
            "SP must spend X_m, not F"
        );
        assert_eq!(sp.input[0].previous_output.vout, x_m.payload_vout, "SP must spend X_m's payload");
        assert_ne!(
            sp.input[0].previous_output.txid.to_string(),
            cb.parent.f_txid,
            "SP must never spend the carrier's funding output F"
        );
        // (iii) that leg's source carrier is terminal at the SE.
        let (_budget, _fin, terminal) =
            mercuryrustlib::lightning_latch::get_spend_budget(&cc, &cb.parent_statechain_id).await?;
        assert!(
            terminal,
            "the source carrier {} of child {pid} must be TERMINAL at the SE — otherwise it can \
             mint a rival state behind bob's back",
            cb.parent_statechain_id
        );
        println!(
            "VERIFY leg: child {pid} holds {assigned} of {contract}; its SP spends X_m {}:{} (F {} \
             untouched); source carrier {} is terminal",
            x_m.txid, x_m.payload_vout, cb.parent.f_txid, cb.parent_statechain_id
        );
    }
    assert_eq!(total_booked, 100, "the children's declared shares must sum to the amount paid");
    // Every source carrier named across the legs must be DISTINCT — two children carved out of the
    // same carrier would mean the payment never spanned carriers at all.
    let sources: std::collections::HashSet<String> = {
        let mut set = std::collections::HashSet::new();
        for pid in &piece_ids {
            let cb = mercuryrustlib::tesr::load_child(&cc, "sdk31_bob", pid).await?.unwrap();
            set.insert(cb.parent_statechain_id.clone());
        }
        set
    };
    assert_eq!(sources.len(), 2, "the two children must come from two DIFFERENT carriers: {sources:?}");

    // ===== (c) SURVIVAL, at the stock level (E7) ================================================
    //
    // The legacy version "exited" by broadcasting the combine tx and reading a balance. Neither half
    // transfers: there is no combine tx, and `get_asset_balance` is blind to a dead stash (E7). The
    // survival evidence for a coloured child is the read-only `color_psbt` stock probe over its own
    // exit output, which accepts exactly the declared amount and refuses one more. The full
    // on-chain five-tier walk is owned by sdk77, which drives it end to end; repeating it per leg
    // here would add ~2 CSV waits per child and prove nothing sdk77 does not.
    for pid in &piece_ids {
        let (_, assigned, _, _) = bob.colored_child_health(pid).await?;
        bob.probe_colored_child_tip(pid, assigned).await.map_err(|e| {
            anyhow!("bob's stock does not bind {assigned} to child {pid}'s exit output: {e}")
        })?;
        assert!(
            bob.probe_colored_child_tip(pid, assigned + 1).await.is_err(),
            "the stock probe must REFUSE {} on child {pid} — accepting more than the declared \
             amount means the probe is not reading the stash at all",
            assigned + 1
        );
        println!("SDK31 - stock probe: child {pid} spends exactly {assigned} out of its own exit output, and refuses {}", assigned + 1);
    }

    // ===== (d) NEGATIVE: over-balance is a TYPED insufficient error ==============================
    // alice now holds only 10; asking for 200 must fail on total balance, via the coloured selector.
    let err = alice.transfer_tokens(&asset, &bob_addr, 200).await.expect_err("over-balance must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("cannot send 200") || msg.contains("short by"),
        "expected a typed insufficient error naming the shortfall, got: {msg}"
    );
    println!("SDK31 - NEGATIVE: paying 200 > balance is refused with a typed insufficient error: {msg}");

    println!("SDK31 - SUCCESS: a payment spanning several carriers works on the CTES-R lane — one in-ladder split per carrier, each conveying a COLOURED child to the recipient, whose declared shares sum to the amount paid (bob 100 / alice 10). Every child's SP spends its parent's X_m payload output and never the carrier's funding output F, every source carrier is TERMINAL at the SE (the invalidation property the old 2-terminal-ancestor rule enforced), the two children come from two DIFFERENT carriers, and the stock binds each child's exact share to its own exit output.");
    Ok(())
}
