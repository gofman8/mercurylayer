//! **What actually bounds a funded tower's capacity.** [D31 → the funding rail]
//!
//! `PROTOCOL.md` §5.13 sizes a tower's fee bond in SATS — "~2 spike bumps, ≈15 000 sats". That is
//! the obvious unit and it is not the binding one. This test measures the constraint that is.
//!
//! A fee child is v3, and it spends TWO things: the stuck tier's P2A anchor, and a funding UTXO.
//! Under TRUC (BIP-431) a v3 transaction may have at most **one** unconfirmed ancestor. The tier is
//! already that one. So if the funding UTXO is the *change of a previous, still-unconfirmed fee
//! child*, the new child has two unconfirmed ancestor chains and cannot be accepted at any price.
//!
//! The consequence for the rail is structural rather than economic: **a tower's simultaneous-rescue
//! capacity is the number of CONFIRMED fee UTXOs it holds, not the number of sats.** A float of
//! 1 000 000 sats in one UTXO can rescue exactly one tier per confirmation window. A tower sized
//! only in sats looks solvent and is not.
//!
//! Run: `CORE_RPC_URL=http://127.0.0.1:18443 CORE_RPC_USER=user CORE_RPC_PASS=password \
//!       cargo test -p mercuryrustlib --test live_tower_float -- --nocapture`

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

fn fund(cfg: &CoreRpcConfig, addr: &str, sats: u64) -> (Txid, u32) {
    let txid_v =
        rpc(cfg, "sendtoaddress", serde_json::json!([addr, sats as f64 / 100_000_000.0]));
    let txid = Txid::from_str(txid_v.as_str().unwrap()).unwrap();
    let mine_to = rpc(cfg, "getnewaddress", serde_json::json!([])).as_str().unwrap().to_string();
    rpc(cfg, "generatetoaddress", serde_json::json!([1, mine_to]));
    let raw = rpc(cfg, "getrawtransaction", serde_json::json!([txid.to_string(), true]));
    let vout = raw["vout"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["scriptPubKey"]["address"].as_str() == Some(addr))
        .map(|o| o["n"].as_u64().unwrap() as u32)
        .expect("funded output");
    (txid, vout)
}

fn sign_p2tr(tx: &mut Transaction, i: usize, prevouts: &[TxOut], kp: &KeyPair) {
    let secp = Secp256k1::new();
    let sh = SighashCache::new(&*tx)
        .taproot_key_spend_signature_hash(i, &Prevouts::All(prevouts), TapSighashType::Default)
        .unwrap();
    let sig = secp.sign_schnorr_no_aux_rand(
        &Message::from_slice(sh.as_byte_array()).unwrap(),
        &kp.tap_tweak(&secp, None).to_inner(),
    );
    let mut w = Witness::new();
    w.push(sig.as_ref());
    tx.input[i].witness = w;
}

/// **THE MEASUREMENT.** Rescue one tier; then try to rescue a second using the FIRST rescue's
/// unconfirmed change as the funding input. If the second succeeds, a tower's capacity is its sats
/// and the rail can chain. If it is refused, capacity is its count of confirmed UTXOs.
#[test]
fn a_second_bump_cannot_be_funded_from_the_first_bumps_unconfirmed_change() {
    let Some(cfg) = cfg() else {
        eprintln!(
            "SKIP live_tower_float: CORE_RPC_URL unset — the TRUC capacity bound was NOT measured, \
             so any rail sizing that cites this test is citing nothing."
        );
        return;
    };

    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[0x5au8; 32]).unwrap();
    let kp = KeyPair::from_secret_key(&secp, &sk);
    let (xonly, _) = XOnlyPublicKey::from_keypair(&kp);
    let addr = Address::p2tr(&secp, xonly, None, Network::Regtest);
    let spk = addr.script_pubkey();

    let floor = rpc(&cfg, "getmempoolinfo", serde_json::json!([]))["minrelaytxfee"]
        .as_f64()
        .unwrap()
        * 100_000_000.0
        / 1000.0;

    // Two independent stuck tiers, and ONE confirmed fee UTXO.
    let tier_in = 100_000u64;
    let (a_txid, a_vout) = fund(&cfg, &addr.to_string(), tier_in);
    let (b_txid, b_vout) = fund(&cfg, &addr.to_string(), tier_in);
    let float_sats = 800_000u64;
    let (f_txid, f_vout) = fund(&cfg, &addr.to_string(), float_sats);

    let mk_tier = |txid: Txid, vout: u32, fee: u64| Transaction {
        version: 3,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid, vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![
            TxOut { value: tier_in - P2A_VALUE - fee, script_pubkey: spk.clone() },
            TxOut { value: P2A_VALUE, script_pubkey: p2a_script() },
        ],
    };
    let prevouts_of = |v: u64| vec![TxOut { value: v, script_pubkey: spk.clone() }];

    let vsize = {
        let mut probe = mk_tier(a_txid, a_vout, 1_000);
        sign_p2tr(&mut probe, 0, &prevouts_of(tier_in), &kp);
        probe.vsize() as u64
    };
    let required = (floor * vsize as f64).ceil() as u64;
    if required < 2 {
        eprintln!("SKIP: relay floor too low to build an under-paying tier. NOTHING measured.");
        return;
    }
    let tier_fee = required / 2;
    let target = (floor * 10.0).max(2.0);

    // ── Rescue 1: confirmed funding UTXO. Must succeed. ───────────────────────────────────────────
    let mut tier_a = mk_tier(a_txid, a_vout, tier_fee);
    sign_p2tr(&mut tier_a, 0, &prevouts_of(tier_in), &kp);
    let built_a = build_p2a_fee_child(
        &StuckParent {
            txid: tier_a.txid(),
            p2a_vout: 1,
            vsize: tier_a.vsize() as u64,
            fee: tier_fee,
        },
        &FundingInput {
            outpoint: OutPoint { txid: f_txid, vout: f_vout },
            value: float_sats,
            script_pubkey: spk.clone(),
        },
        spk.clone(),
        target,
    )
    .expect("build 1");
    let mut child_a = built_a.tx.clone();
    sign_p2tr(&mut child_a, 1, &built_a.prevouts, &kp);
    let r1 = submit_package(
        &cfg,
        &bitcoin::consensus::encode::serialize_hex(&tier_a),
        &bitcoin::consensus::encode::serialize_hex(&child_a),
    );
    assert!(r1.is_ok(), "the first rescue, from a CONFIRMED utxo, must succeed: {r1:?}");
    println!("rescue 1 (confirmed funding utxo): ACCEPTED");

    // ── Rescue 2: funded from rescue 1's UNCONFIRMED change. ──────────────────────────────────────
    let change_value = child_a.output[0].value;
    println!(
        "rescue 2 attempts to spend the unconfirmed change of child {} ({change_value} sats)",
        child_a.txid()
    );
    let mut tier_b = mk_tier(b_txid, b_vout, tier_fee);
    sign_p2tr(&mut tier_b, 0, &prevouts_of(tier_in), &kp);
    let built_b = build_p2a_fee_child(
        &StuckParent {
            txid: tier_b.txid(),
            p2a_vout: 1,
            vsize: tier_b.vsize() as u64,
            fee: tier_fee,
        },
        &FundingInput {
            outpoint: OutPoint { txid: child_a.txid(), vout: 0 },
            value: change_value,
            script_pubkey: spk.clone(),
        },
        spk.clone(),
        target,
    )
    .expect("build 2");
    let mut child_b = built_b.tx.clone();
    sign_p2tr(&mut child_b, 1, &built_b.prevouts, &kp);
    let r2 = submit_package(
        &cfg,
        &bitcoin::consensus::encode::serialize_hex(&tier_b),
        &bitcoin::consensus::encode::serialize_hex(&child_b),
    );

    match r2 {
        Ok(_) => panic!(
            "the second rescue SUCCEEDED off unconfirmed change. TRUC does not bind here the way \
             the rail assumes, and `TowerFloat`'s confirmed-UTXO capacity rule is over-strict — \
             re-derive it from this result rather than leaving the code claiming a limit that is not \
             real."
        ),
        Err(e) => {
            let msg = e.to_string();
            println!("rescue 2 (unconfirmed change as funding): REFUSED — {msg}");
            assert!(
                msg.contains("TRUC") || msg.to_lowercase().contains("ancestor"),
                "expected a TRUC/ancestor refusal, got: {msg}"
            );
        }
    }

    // ── And the same rescue from a CONFIRMED second utxo succeeds, proving the tier itself is fine
    //    and the refusal above was about the FUNDING input, not about tier B. ─────────────────────
    let (g_txid, g_vout) = fund(&cfg, &addr.to_string(), float_sats);
    let mut tier_b2 = mk_tier(b_txid, b_vout, tier_fee);
    sign_p2tr(&mut tier_b2, 0, &prevouts_of(tier_in), &kp);
    let built_b2 = build_p2a_fee_child(
        &StuckParent {
            txid: tier_b2.txid(),
            p2a_vout: 1,
            vsize: tier_b2.vsize() as u64,
            fee: tier_fee,
        },
        &FundingInput {
            outpoint: OutPoint { txid: g_txid, vout: g_vout },
            value: float_sats,
            script_pubkey: spk.clone(),
        },
        spk.clone(),
        target,
    )
    .expect("build 3");
    let mut child_b2 = built_b2.tx.clone();
    sign_p2tr(&mut child_b2, 1, &built_b2.prevouts, &kp);
    let r3 = submit_package(
        &cfg,
        &bitcoin::consensus::encode::serialize_hex(&tier_b2),
        &bitcoin::consensus::encode::serialize_hex(&child_b2),
    );
    assert!(
        r3.is_ok(),
        "the SAME tier funded from a SECOND CONFIRMED utxo must succeed — otherwise the refusal \
         above was not about the funding input and this test proves nothing: {r3:?}"
    );
    println!("rescue 2' (second CONFIRMED utxo): ACCEPTED — capacity is confirmed UTXOs, not sats");
}
