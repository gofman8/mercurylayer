//! **The WP1 acceptance criterion, executed.** [D31, #123]
//!
//! WP1 measured that a v3 TES-R tier under the relay floor is refused alone and accepted as a 1P1C
//! package — but it measured that with a hand-run `bitcoin-cli`, and recorded the gap plainly: *no
//! `submitpackage` caller exists anywhere in the tree*. The criterion it set was therefore not "the
//! rescue works" but:
//!
//! > An under-paying v3 tier is rescued **through this repo's own code path**, on a node with
//! > `minrelayfee` above the tier's committed rate — not through a hand-run `bitcoin-cli`.
//!
//! This test is that. It builds an under-paying v3 parent with a P2A anchor, proves the node
//! REFUSES it alone, then rescues it with `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child`
//! and `mercuryrustlib::core_rpc::submit_package` — repo code for both halves.
//!
//! **It skips, loudly, when there is no node.** A test that silently passes without exercising
//! anything is worse than no test: this one prints why it skipped, so a green run cannot be mistaken
//! for a verified rescue.
//!
//! Run: `CORE_RPC_URL=http://127.0.0.1:18443 CORE_RPC_USER=user CORE_RPC_PASS=password \
//!       cargo test -p mercuryrustlib --test live_p2a_package_rescue -- --nocapture`

use bitcoin::{
    absolute,
    hashes::Hash,
    key::TapTweak,
    secp256k1::{KeyPair, Message, Secp256k1, SecretKey, XOnlyPublicKey},
    sighash::{Prevouts, SighashCache, TapSighashType},
    Address, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use mercurylib::tesr::{p2a_script, P2A_VALUE};
use mercurylib::wallet::p2a_fee_child::{build_p2a_fee_child, FundingInput, StuckParent};
use mercuryrustlib::core_rpc::{submit_package, CoreRpcConfig};
use std::str::FromStr;

fn cfg() -> Option<CoreRpcConfig> {
    let url = std::env::var("CORE_RPC_URL").ok()?;
    Some(CoreRpcConfig::new(
        url,
        std::env::var("CORE_RPC_USER").unwrap_or_else(|_| "user".into()),
        std::env::var("CORE_RPC_PASS").unwrap_or_else(|_| "password".into()),
    ))
}

fn rpc(cfg: &CoreRpcConfig, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({"jsonrpc":"1.0","id":"t","method":method,"params":params});
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let resp = client
        .post(&cfg.url)
        .basic_auth(&cfg.user, Some(&cfg.password))
        .json(&body)
        .send()
        .unwrap_or_else(|e| panic!("{method} unreachable: {e}"));
    let v: serde_json::Value = serde_json::from_str(&resp.text().unwrap()).unwrap();
    if !v["error"].is_null() {
        panic!("{method} failed: {}", v["error"]);
    }
    v["result"].clone()
}

/// `sendtoaddress` on whatever wallet the node has loaded, tolerating a node with no wallet by
/// naming that as the reason rather than failing obscurely.
fn fund(cfg: &CoreRpcConfig, addr: &str, sats: u64) -> (Txid, u32) {
    let btc = sats as f64 / 100_000_000.0;
    let txid_v = rpc(cfg, "sendtoaddress", serde_json::json!([addr, btc]));
    let txid = Txid::from_str(txid_v.as_str().unwrap()).unwrap();
    // Confirm it so the parent spends a settled input (keeps the package to 1P1C: TRUC allows at most
    // one unconfirmed ancestor, and an unconfirmed funder would make it two).
    let mine_to = rpc(cfg, "getnewaddress", serde_json::json!([])).as_str().unwrap().to_string();
    rpc(cfg, "generatetoaddress", serde_json::json!([1, mine_to]));

    let raw = rpc(cfg, "getrawtransaction", serde_json::json!([txid.to_string(), true]));
    let vout = raw["vout"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| {
            o["scriptPubKey"]["address"].as_str() == Some(addr)
                || o["scriptPubKey"]["addresses"][0].as_str() == Some(addr)
        })
        .map(|o| o["n"].as_u64().unwrap() as u32)
        .expect("funded output not found in the funding tx");
    (txid, vout)
}

/// Sign one P2TR key-spend input of `tx` in place.
fn sign_p2tr_input(tx: &mut Transaction, index: usize, prevouts: &[TxOut], kp: &KeyPair) {
    let secp = Secp256k1::new();
    let sighash = SighashCache::new(&*tx)
        .taproot_key_spend_signature_hash(
            index,
            &Prevouts::All(prevouts),
            TapSighashType::Default,
        )
        .expect("sighash");
    let tweaked = kp.tap_tweak(&secp, None);
    let sig = secp.sign_schnorr_no_aux_rand(
        &Message::from_slice(sighash.as_byte_array()).unwrap(),
        &tweaked.to_inner(),
    );
    let mut w = Witness::new();
    w.push(sig.as_ref());
    tx.input[index].witness = w;
}

#[test]
fn an_underpaying_v3_tier_is_rescued_through_this_repos_own_code() {
    let Some(cfg) = cfg() else {
        eprintln!(
            "SKIP live_p2a_package_rescue: CORE_RPC_URL is unset, so NOTHING was verified. This \
             test is the WP1 acceptance criterion (a rescue through repo code, not bitcoin-cli); a \
             green run without it proves nothing. Set CORE_RPC_URL/USER/PASS to run it."
        );
        return;
    };

    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[0x42u8; 32]).unwrap();
    let kp = KeyPair::from_secret_key(&secp, &sk);
    let (xonly, _) = XOnlyPublicKey::from_keypair(&kp);
    let addr = Address::p2tr(&secp, xonly, None, Network::Regtest);
    let spk = addr.script_pubkey();

    // ── 1. Fund the parent, and separately fund the child. ────────────────────────────────────────
    let parent_in_sats = 100_000u64;
    let (p_txid, p_vout) = fund(&cfg, &addr.to_string(), parent_in_sats);
    let funding_sats = 500_000u64;
    let (f_txid, f_vout) = fund(&cfg, &addr.to_string(), funding_sats);

    // ── 2. Build a parent that is under THIS NODE'S floor — read it, do not assume it. ───────────
    //
    // The first version of this test hardcoded a 200-sat fee and asserted the node would refuse it.
    // On this node it did not: the floor is 0.1 sat/vB, so 200 sats over 124 vB (1.61 sat/vB) relays
    // perfectly well, and the "rescue" would have been demonstrated against a transaction that never
    // needed rescuing. The assertion caught it, which is the only reason this comment is not a bug
    // report. Derive the fee from `minrelaytxfee` instead, so the test is meaningful on a default
    // node AND on one with a raised floor.
    let info = rpc(&cfg, "getmempoolinfo", serde_json::json!([]));
    let floor_btc_per_kvb = info["minrelaytxfee"].as_f64().expect("minrelaytxfee");
    let floor_sat_per_vb = floor_btc_per_kvb * 100_000_000.0 / 1000.0;

    // Build once with a placeholder fee purely to MEASURE vsize; output values do not affect it.
    let mk = |fee: u64| -> Transaction {
        Transaction {
            version: 3,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint { txid: p_txid, vout: p_vout },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![
                TxOut {
                    value: parent_in_sats - P2A_VALUE - fee,
                    script_pubkey: spk.clone(),
                },
                TxOut { value: P2A_VALUE, script_pubkey: p2a_script() },
            ],
        }
    };
    let parent_prevouts = vec![TxOut { value: parent_in_sats, script_pubkey: spk.clone() }];
    let measured_vsize = {
        let mut probe = mk(1_000);
        sign_p2tr_input(&mut probe, 0, &parent_prevouts, &kp);
        probe.vsize() as u64
    };

    let required_at_floor = (floor_sat_per_vb * measured_vsize as f64).ceil() as u64;
    if required_at_floor < 2 {
        eprintln!(
            "SKIP: this node's minrelaytxfee is {floor_sat_per_vb} sat/vB, so a {measured_vsize} vB              tier needs only {required_at_floor} sat to clear it and no under-paying parent can be              built. Start bitcoind with a higher -minrelaytxfee to exercise the rescue. NOTHING was              verified."
        );
        return;
    }
    // Comfortably under the floor, so the refusal is unambiguous rather than a rounding accident.
    let parent_fee = required_at_floor / 2;

    let mut parent = mk(parent_fee);
    sign_p2tr_input(&mut parent, 0, &parent_prevouts, &kp);
    let parent_hex = bitcoin::consensus::encode::serialize_hex(&parent);
    let parent_vsize = parent.vsize() as u64;
    let parent_txid = parent.txid();
    println!(
        "node floor: {floor_sat_per_vb:.3} sat/vB => a {parent_vsize} vB tier needs \
         {required_at_floor} sat"
    );
    println!(
        "parent {parent_txid}: {parent_vsize} vB, {parent_fee} sats => {:.3} sat/vB — deliberately \
         UNDER the floor",
        parent_fee as f64 / parent_vsize as f64
    );

    // ── 3. Prove the node REFUSES it alone. If this ever succeeds, the rest proves nothing. ───────
    let alone = {
        let body = serde_json::json!({"jsonrpc":"1.0","id":"t","method":"sendrawtransaction",
                                      "params":[parent_hex]});
        let client = reqwest::blocking::Client::new();
        let r = client
            .post(&cfg.url)
            .basic_auth(&cfg.user, Some(&cfg.password))
            .json(&body)
            .send()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&r.text().unwrap()).unwrap();
        v
    };
    assert!(
        !alone["error"].is_null(),
        "the node ACCEPTED a tier paying {:.3} sat/vB against a stated floor of {floor_sat_per_vb:.3} \
         sat/vB. Either the floor moved under us or the fee derivation is wrong — either way the \
         rescue below would prove nothing, so this fails rather than passing vacuously.",
        parent_fee as f64 / parent_vsize as f64
    );
    let refusal = alone["error"]["message"].as_str().unwrap_or_default().to_string();
    println!("alone: REFUSED — {refusal}");
    assert!(
        refusal.contains("min relay fee not met") || refusal.to_lowercase().contains("fee"),
        "expected a fee-floor refusal, got: {refusal}"
    );

    // ── 4. Build the rescue with REPO CODE. ───────────────────────────────────────────────────────
    // Well above the floor, so the package is unambiguously valid rather than marginal.
    let target = (floor_sat_per_vb * 10.0).max(2.0);
    let child = build_p2a_fee_child(
        &StuckParent { txid: parent_txid, p2a_vout: 1, vsize: parent_vsize, fee: parent_fee },
        &FundingInput {
            outpoint: OutPoint { txid: f_txid, vout: f_vout },
            value: funding_sats,
            script_pubkey: spk.clone(),
        },
        spk.clone(),
        target,
    )
    .expect("the fee child must build");
    println!(
        "child: {} vB est, fee {} sats, package {:.2} sat/vB",
        child.child_vsize, child.child_fee, child.package_fee_rate
    );

    // ── 5. Sign ONLY the funding input. The P2A input keeps its empty witness. ────────────────────
    let mut child_tx = child.tx.clone();
    sign_p2tr_input(&mut child_tx, 1, &child.prevouts, &kp);
    assert!(
        child_tx.input[0].witness.is_empty(),
        "the P2A anchor is anyone-can-spend and must stay witness-free"
    );

    // The estimate must not have been optimistic: a real child LARGER than estimated pays under the
    // target, which is the one way this builder could produce a package that is still refused.
    let real_vsize = child_tx.vsize() as u64;
    println!("child: {real_vsize} vB actual vs {} vB estimated", child.child_vsize);
    assert!(
        real_vsize <= child.child_vsize,
        "estimate_child_vsize() UNDER-estimated ({} vB) against a real {real_vsize} vB child — the \
         package would pay under target and be refused. The estimate must round up.",
        child.child_vsize
    );

    // ── 6. Submit the package through repo code. ──────────────────────────────────────────────────
    let child_hex = bitcoin::consensus::encode::serialize_hex(&child_tx);
    let res = submit_package(&cfg, &parent_hex, &child_hex)
        .expect("the 1P1C package must be ACCEPTED — this is the whole point of the P2A anchor");
    assert!(res.accepted(), "package_msg = {:?}", res.package_msg);
    println!("package: ACCEPTED — package_msg = {:?}", res.package_msg);

    // ── 7. And the parent is genuinely in the mempool now. ────────────────────────────────────────
    let entry = rpc(&cfg, "getmempoolentry", serde_json::json!([parent_txid.to_string()]));
    println!(
        "parent is in the mempool: vsize={} descendantcount={}",
        entry["vsize"], entry["descendantcount"]
    );
    assert_eq!(
        entry["descendantcount"].as_u64(),
        Some(2),
        "the parent should have exactly itself + the fee child as its descendant set"
    );
}
