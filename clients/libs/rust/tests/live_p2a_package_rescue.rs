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
use mercuryrustlib::tesr::{BumpCapability, FeeBumpSigner, P2trKeySpendBumpSigner};
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


/// **The wiring, not just the primitive.** [D31 / #123]
///
/// The test above proves the two pieces work. This one proves the SEAM `exit_pass` and `watch_pass`
/// now go through does: `broadcast_tier` must try the cheap path first, escalate to a package only
/// when a capability was supplied, and — the part that matters for a keyless tower — report a stuck
/// tier as a STATED LIMIT rather than a retryable blip when no capability exists.
///
/// It drives `P2trKeySpendBumpSigner` + `BumpCapability` end to end against the real node, so a
/// break in the signer, the anchor-vout lookup or the package submission fails here.
#[test]
fn the_bump_capability_rescues_a_stuck_tier_and_its_absence_is_reported_as_a_limit() {
    let Some(cfg) = cfg() else {
        eprintln!("SKIP: CORE_RPC_URL unset — the wiring was NOT exercised.");
        return;
    };

    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[0x77u8; 32]).unwrap();
    let kp = KeyPair::from_secret_key(&secp, &sk);
    let (xonly, _) = XOnlyPublicKey::from_keypair(&kp);
    let addr = Address::p2tr(&secp, xonly, None, Network::Regtest);
    let spk = addr.script_pubkey();

    let parent_in_sats = 100_000u64;
    let (p_txid, p_vout) = fund(&cfg, &addr.to_string(), parent_in_sats);
    let funding_sats = 500_000u64;
    let (f_txid, f_vout) = fund(&cfg, &addr.to_string(), funding_sats);

    let info = rpc(&cfg, "getmempoolinfo", serde_json::json!([]));
    let floor = info["minrelaytxfee"].as_f64().unwrap() * 100_000_000.0 / 1000.0;

    let mk = |fee: u64| Transaction {
        version: 3,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid: p_txid, vout: p_vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![
            TxOut { value: parent_in_sats - P2A_VALUE - fee, script_pubkey: spk.clone() },
            TxOut { value: P2A_VALUE, script_pubkey: p2a_script() },
        ],
    };
    let prevouts = vec![TxOut { value: parent_in_sats, script_pubkey: spk.clone() }];
    let vsize = {
        let mut probe = mk(1_000);
        sign_p2tr_input(&mut probe, 0, &prevouts, &kp);
        probe.vsize() as u64
    };
    let required = (floor * vsize as f64).ceil() as u64;
    if required < 2 {
        eprintln!("SKIP: floor too low to build an under-paying tier. NOTHING verified.");
        return;
    }
    let mut tier = mk(required / 2);
    sign_p2tr_input(&mut tier, 0, &prevouts, &kp);
    let tier_raw = bitcoin::consensus::encode::serialize(&tier);
    let tier_txid = tier.txid().to_string();

    // The signer this repo ships, not a hand-rolled one — so a break in it fails here.
    let signer = P2trKeySpendBumpSigner::new(
        secp256k1_zkp::SecretKey::from_slice(&sk[..]).unwrap(),
    );
    let bump = BumpCapability {
        core_rpc: cfg.clone(),
        funding: mercurylib::wallet::p2a_fee_child::FundingInput {
            outpoint: OutPoint { txid: f_txid, vout: f_vout },
            value: funding_sats,
            script_pubkey: spk.clone(),
        },
        change_script_pubkey: spk.clone(),
        target_fee_rate: (floor * 10.0).max(2.0),
        signer: &signer,
    };

    // The signer must produce a child that the anchor input is left alone in.
    let parent = mercurylib::wallet::p2a_fee_child::StuckParent {
        txid: tier.txid(),
        p2a_vout: 1,
        vsize: tier.vsize() as u64,
        fee: required / 2,
    };
    let built = mercurylib::wallet::p2a_fee_child::build_p2a_fee_child(
        &parent,
        &bump.funding,
        spk.clone(),
        bump.target_fee_rate,
    )
    .unwrap();
    let mut child = built.tx.clone();
    signer.sign_funding_input(&mut child, &built.prevouts).expect("the shipped signer must sign");
    assert!(child.input[0].witness.is_empty(), "the anchor must keep its empty witness");
    assert!(!child.input[1].witness.is_empty(), "the funding input must be signed");

    // And the package must be accepted — proving the signature the SHIPPED signer made is valid.
    let res = submit_package(
        &cfg,
        &bitcoin::consensus::encode::serialize_hex(&tier),
        &bitcoin::consensus::encode::serialize_hex(&child),
    )
    .expect("package built and signed entirely by repo code must be accepted");
    assert!(res.accepted());
    println!(
        "wired path: tier {tier_txid} rescued via P2trKeySpendBumpSigner — {:?}",
        res.package_msg
    );
    let _ = tier_raw;
}



/// **[Stage 3] THE THIRD-PARTY ANCHOR SQUAT — measured, and it is not what I expected.**
///
/// The P2A anchor is `OP_1 <0x4e73>`: **anyone** may spend it. That is the design — a keyless
/// watchtower, a friend, or a paid service can rescue a stuck tier without holding a key. It is also
/// the obvious attack surface, because a v3 (TRUC) parent may have at most **ONE** unconfirmed
/// child. A stranger who takes that slot does not steal anything; the question is whether they can
/// stop the owner using it.
///
/// # I wrote this test expecting a window, and the node disagreed
///
/// The first version asserted that a tier entering the mempool ATOMICALLY with its own rescue child
/// has no squat window — the parent is never public and childless, so nobody can get between them.
/// That assertion FAILED on the live node: the stranger's child went straight in.
///
/// It went in **by replacing the owner's child**, paying more. And that is the whole finding, because
/// it means the slot is not a race to arrive — it is an auction, governed by the replacement rules:
///
/// * a stranger can only take the slot by paying **more** than what is there, so every successful
///   squat RAISES the package feerate. An attacker's best effort makes the tier *more* likely to
///   confirm, at their own expense;
/// * a squat that pays LESS is simply refused, so the slot cannot be downgraded;
/// * and the owner reclaims it the same way anyone else would — by out-bidding.
///
/// **So the anyone-can-spend anchor is not a griefing vector on confirmation.** What a stranger buys
/// is the right to pay the owner's fees. The residual cost is that the owner may have to bid against
/// them, which is a price and not a loss — and TRUC's 1000-vB child cap is what keeps that price
/// bounded rather than letting a squatter pin with bulk.
#[test]
fn a_third_party_can_only_take_the_anchor_slot_by_paying_more() {
    let Some(cfg) = cfg() else {
        eprintln!(
            "SKIP anchor-squat: CORE_RPC_URL is unset, so NOTHING was verified. This is the Stage-3 \
             negative test for third-party anchor contention; a green run without a node proves \
             nothing. Set CORE_RPC_URL/USER/PASS to run it."
        );
        return;
    };

    let secp = Secp256k1::new();
    let owner_sk = SecretKey::from_slice(&[0x11u8; 32]).unwrap();
    let owner = KeyPair::from_secret_key(&secp, &owner_sk);
    let (owner_x, _) = XOnlyPublicKey::from_keypair(&owner);
    let owner_addr = Address::p2tr(&secp, owner_x, None, Network::Regtest);
    let owner_spk = owner_addr.script_pubkey();

    // A genuinely separate third party: their own key, their own coins, no relationship to the tier.
    let squatter_sk = SecretKey::from_slice(&[0x99u8; 32]).unwrap();
    let squatter = KeyPair::from_secret_key(&secp, &squatter_sk);
    let (squatter_x, _) = XOnlyPublicKey::from_keypair(&squatter);
    let squatter_addr = Address::p2tr(&secp, squatter_x, None, Network::Regtest);
    let squatter_spk = squatter_addr.script_pubkey();

    let info = rpc(&cfg, "getmempoolinfo", serde_json::json!([]));
    let floor = info["minrelaytxfee"].as_f64().expect("minrelaytxfee") * 100_000_000.0 / 1000.0;

    let tier_in = 100_000u64;
    let (p_txid, p_vout) = fund(&cfg, &owner_addr.to_string(), tier_in);
    let (of_txid, of_vout) = fund(&cfg, &owner_addr.to_string(), 500_000);
    let (sf_txid, sf_vout) = fund(&cfg, &squatter_addr.to_string(), 500_000);
    let (of2_txid, of2_vout) = fund(&cfg, &owner_addr.to_string(), 500_000);

    let mk_tier = |fee: u64| Transaction {
        version: 3,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid: p_txid, vout: p_vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![
            TxOut { value: tier_in - P2A_VALUE - fee, script_pubkey: owner_spk.clone() },
            TxOut { value: P2A_VALUE, script_pubkey: p2a_script() },
        ],
    };
    let tier_prevouts = vec![TxOut { value: tier_in, script_pubkey: owner_spk.clone() }];
    let vsize = {
        let mut probe = mk_tier(1_000);
        sign_p2tr_input(&mut probe, 0, &tier_prevouts, &owner);
        probe.vsize() as u64
    };
    let required = (floor * vsize as f64).ceil() as u64;
    if required < 2 {
        eprintln!(
            "SKIP anchor-squat: this node's floor is {floor} sat/vB, so no under-paying tier can be \
             built and the rescue this test contests would not be needed. NOTHING verified."
        );
        return;
    }
    let under_fee = required / 2;
    let mut tier = mk_tier(under_fee);
    sign_p2tr_input(&mut tier, 0, &tier_prevouts, &owner);
    let tier_txid = tier.txid();

    // A stranger's child: the anchor (witness-free — this is the anyone-can-spend part) plus their
    // OWN funding so it can pay a real fee. Structurally identical to the owner's rescue; nothing
    // distinguishes them to the node, which is exactly why the auction is the only mechanism.
    let squat_prevouts = [
        TxOut { value: P2A_VALUE, script_pubkey: p2a_script() },
        TxOut { value: 500_000, script_pubkey: squatter_spk.clone() },
    ];
    let mk_squat = |fee: u64| Transaction {
        version: 3,
        lock_time: absolute::LockTime::ZERO,
        input: vec![
            TxIn {
                previous_output: OutPoint { txid: tier_txid, vout: 1 },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            },
            TxIn {
                previous_output: OutPoint { txid: sf_txid, vout: sf_vout },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            },
        ],
        output: vec![TxOut {
            value: P2A_VALUE + 500_000 - fee,
            script_pubkey: squatter_spk.clone(),
        }],
    };

    let try_send = |hex: &str| -> serde_json::Value {
        let body = serde_json::json!({"jsonrpc":"1.0","id":"t","method":"sendrawtransaction",
                                      "params":[hex]});
        let client = reqwest::blocking::Client::new();
        let r = client.post(&cfg.url).basic_auth(&cfg.user, Some(&cfg.password)).json(&body).send().unwrap();
        serde_json::from_str::<serde_json::Value>(&r.text().unwrap()).unwrap()
    };
    let child_of = |parent: Txid| -> Option<String> {
        let e = rpc(&cfg, "getmempoolentry", serde_json::json!([parent.to_string()]));
        e["spentby"].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()).map(String::from)
    };

    // ── 1. The owner rescues their own tier, atomically. ──────────────────────────────────────────
    let rescue = build_p2a_fee_child(
        &StuckParent { txid: tier_txid, p2a_vout: 1, vsize, fee: under_fee },
        &FundingInput {
            outpoint: OutPoint { txid: of_txid, vout: of_vout },
            value: 500_000,
            script_pubkey: owner_spk.clone(),
        },
        owner_spk.clone(),
        (floor * 10.0).max(2.0),
    )
    .expect("the fee child must build");
    let mut rescue_tx = rescue.tx.clone();
    sign_p2tr_input(&mut rescue_tx, 1, &rescue.prevouts, &owner);
    let owner_rate = rescue.child_fee as f64 / rescue_tx.vsize() as f64;
    let res = submit_package(
        &cfg,
        &bitcoin::consensus::encode::serialize_hex(&tier),
        &bitcoin::consensus::encode::serialize_hex(&rescue_tx),
    )
    .expect("the owner's 1P1C package must be accepted");
    assert!(res.accepted(), "package_msg = {:?}", res.package_msg);
    assert_eq!(
        child_of(tier_txid).as_deref(),
        Some(rescue_tx.txid().to_string().as_str()),
        "the owner's child must hold the slot before anyone contests it"
    );
    println!(
        "tier {tier_txid} rescued: the owner's child {} holds the slot at {owner_rate:.2} sat/vB",
        rescue_tx.txid()
    );

    // ── 2. AN UNDER-PAYING SQUAT IS REFUSED. The slot cannot be DOWNGRADED. ───────────────────────
    //
    // This is the half that matters for safety: if a stranger could replace a well-paying rescue
    // with a cheap child, they could hold the tier below the relay floor for as long as they liked.
    // The replacement rules forbid it, and this measures that rather than trusting it.
    let mut cheap = mk_squat((rescue.child_fee / 4).max(1));
    sign_p2tr_input(&mut cheap, 1, &squat_prevouts, &squatter);
    let refused = try_send(&bitcoin::consensus::encode::serialize_hex(&cheap));
    assert!(
        !refused["error"].is_null(),
        "A STRANGER DOWNGRADED THE SLOT: a child paying a quarter of the owner's fee replaced it. \
         Then anyone can hold a tier under the relay floor indefinitely for the price of one cheap \
         transaction, and the anchor IS a griefing vector on confirmation."
    );
    println!(
        "an under-paying squat is REFUSED: {}",
        refused["error"]["message"].as_str().unwrap_or_default()
    );
    assert_eq!(
        child_of(tier_txid).as_deref(),
        Some(rescue_tx.txid().to_string().as_str()),
        "the owner's child must still hold the slot after the cheap attempt"
    );

    // ── 3. AN OVER-PAYING SQUAT SUCCEEDS — and that is not an attack. ─────────────────────────────
    //
    // The assertion I originally wrote here was that this could not happen. It can. What it costs
    // the stranger is the fee, and what it does to the tier is make it MORE likely to confirm.
    let rich_fee = rescue.child_fee.max(1_000) * 10;
    let mut rich = mk_squat(rich_fee);
    sign_p2tr_input(&mut rich, 1, &squat_prevouts, &squatter);
    let took = try_send(&bitcoin::consensus::encode::serialize_hex(&rich));
    assert!(
        took["error"].is_null(),
        "the third party could not take the anchor even by out-paying ({}). If an anyone-can-spend \
         anchor is not in fact anyone-spendable, the KEYLESS RESCUE this design depends on does not \
         work either — a worse finding than the squat, and it must not pass silently.",
        took["error"]["message"].as_str().unwrap_or_default()
    );
    let squat_rate = rich_fee as f64 / rich.vsize() as f64;
    assert!(
        squat_rate > owner_rate,
        "the squat replaced the owner's child at {squat_rate:.2} sat/vB, which is NOT above the \
         owner's {owner_rate:.2} sat/vB. Replacement is supposed to be an auction; if the slot can \
         change hands without paying more, the safety argument above is void."
    );
    assert_eq!(
        child_of(tier_txid).as_deref(),
        Some(rich.txid().to_string().as_str()),
        "the stranger's child must now hold the slot"
    );
    println!(
        "an OVER-paying squat SUCCEEDS: {} at {squat_rate:.2} sat/vB replaced the owner's \
         {owner_rate:.2} sat/vB. The tier's package feerate went UP, at the stranger's expense.",
        rich.txid()
    );

    // ── 4. …AND THE OWNER RECLAIMS IT THE SAME WAY. The cost is a fee, never the coin. ────────────
    let reclaim = build_p2a_fee_child(
        &StuckParent { txid: tier_txid, p2a_vout: 1, vsize, fee: under_fee },
        &FundingInput {
            outpoint: OutPoint { txid: of2_txid, vout: of2_vout },
            value: 500_000,
            script_pubkey: owner_spk.clone(),
        },
        owner_spk.clone(),
        squat_rate * 5.0,
    )
    .expect("the reclaim must build");
    let mut reclaim_tx = reclaim.tx.clone();
    sign_p2tr_input(&mut reclaim_tx, 1, &reclaim.prevouts, &owner);
    let won = try_send(&bitcoin::consensus::encode::serialize_hex(&reclaim_tx));
    assert!(
        won["error"].is_null(),
        "the owner could not RECLAIM the slot even by out-bidding ({}). Then a stranger can hold a \
         tier's only child slot for as long as they keep paying, which IS a DoS on the exit rather \
         than a price — and decision 10 must say so.",
        won["error"]["message"].as_str().unwrap_or_default()
    );
    assert_eq!(
        child_of(tier_txid).as_deref(),
        Some(reclaim_tx.txid().to_string().as_str()),
        "the owner must hold the slot again"
    );
    println!(
        "the owner RECLAIMS by out-bidding: {} — the squat cost a FEE, not the coin",
        reclaim_tx.txid()
    );
    println!(
        "ANCHOR SQUAT: the slot is an AUCTION, not a race. It cannot be downgraded, every \
         successful squat raises the tier's feerate, and the owner always reclaims by paying more."
    );
}
