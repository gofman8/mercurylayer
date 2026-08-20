//! E2E (SDK_E2E=92) — **witness binding, live: an honest disclosure signs, a tampered one is refused.**
//!
//! # What this decides
//!
//! Everything built for REQ-57 so far is *reachable code*: the SE can rebuild a session from a
//! disclosed transaction and compare it, and unit differentials prove the rebuild is byte-correct.
//! None of that shows the gate actually fires on the live stack, and none of it shows a LIE is
//! caught. A binding that accepts everything is indistinguishable from no binding at all.
//!
//! So this test does both halves against the running lockbox:
//!
//! * **(a) the honest path still works.** A real laddering claim, with the client now attaching a
//!   disclosure to every co-signature. If the binding is wrong in any of the ways it could be —
//!   wrong hash type, fin nonce not stripped, wrong prevout script — this fails, and it fails for
//!   EVERY signature rather than intermittently. This is the regression half.
//! * **(b) a tampered disclosure is REFUSED.** The same request with one satoshi changed in the
//!   disclosed prevout value. That single byte changes the BIP-341 sighash, which changes the
//!   session, which must fail the compare. This is the half that proves the gate exists.
//!
//! # Why one satoshi, and why the prevout value
//!
//! It is the smallest possible lie, and it is a field the SE never checks directly — BIP-341 commits
//! the prevout amount into the sighash, so the binding catches it as a side effect of comparing
//! sessions rather than through any explicit amount check. If a one-satoshi difference is caught,
//! every larger lie about amounts, scripts, outpoints, version, locktime or sequences is caught by
//! the same mechanism, because they all feed the same hash.
//!
//! That is the property worth testing: not that the SE validates a list of fields, but that it
//! cannot be fooled by a field nobody remembered to validate.
//!
//! # What this does NOT prove
//!
//! That the binding is *required*. It is opt-in per request while the JS and Kotlin clients are
//! migrated, so a caller that simply omits the disclosure is still served. Making it mandatory is a
//! separate change with a deploy consequence, and until then a malicious client can decline to be
//! bound. Stated here so a green run is not read as "the SE now verifies every signature".
//!
//! Run: `SDK_E2E=92 ML_NETWORK=regtest cargo run` (regtest stack + a lockbox built with witness.cpp)

use anyhow::{anyhow, Result};
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::{OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercurylib::wallet::Coin;
use secp256k1_zkp::musig::{
    blinded_musig_pubkey_xonly_tweak_add, new_musig_nonce_pair, BlindingFactor, MusigAggNonce,
    MusigPubNonce, MusigSecNonce, MusigSession, MusigSessionId,
};
use secp256k1_zkp::{Message, PublicKey, Secp256k1, SecretKey};
use std::process::Command;
use std::time::Duration;

use crate::bitcoin_core;

const DEPOSIT: u64 = 150_000;

/// How many times the SE has logged a SUCCESSFUL binding.
///
/// Needed because "the honest path works" and "the gate never ran" are the same observation from
/// outside: an absent disclosure is served, so a client that silently failed to serialise one would
/// still complete a deposit. Counting the SE's own log line is what tells the two apart.
/// Returns `(successful binds, co-signature requests)`.
///
/// The RATIO is the real measurement. A count of binds > 0 only proves the gate can fire; it says
/// nothing about how much of a coin's lifecycle is covered. When these two disagree, some signing
/// path is reaching the SE without a disclosure and is therefore unbound — which is how a partially
/// wired gate reads as a working one.
fn bind_stats() -> (usize, usize) {
    let out = Command::new("docker")
        .args(["logs", "mercurylayer-lockbox-1"])
        .output();
    match out {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (
                s.matches("WITNESS_BIND_MATCH").count(),
                s.matches("POST /get_partial_signature").count(),
            )
        }
        Err(_) => (0, 0),
    }
}

/// A real transaction, the prevout it spends, and the session that transaction genuinely produces.
///
/// Every field is derived rather than invented: the prevout script is P2TR over the same blinded
/// output key the session is built with, and the sighash comes from `bitcoin`'s own BIP-341
/// implementation. That is the point — the SE must be fed something an honest client could have
/// sent, so that changing ONE satoshi is the only lie under test.
struct Honest {
    unsigned_tx_hex: String,
    prevout_value: u64,
    prevout_spk_hex: String,
    agg_pubkey_hex: String,
    agg_nonce_hex: String,
    blinding_hex: String,
    out_tweak_hex: String,
    session_hex: String,
}

fn build_honest(value: u64) -> Result<Honest> {
    build_honest_spending(value, Txid::all_zeros())
}

/// [#171] `build_honest`, but spending a CHOSEN outpoint. The attack case needs a disclosure that is
/// internally consistent — so the SE co-signs it — while naming a transaction the SE co-signed under
/// SOMEBODY ELSE'S sid, which is what the parent edge is resolved from.
fn build_honest_spending(value: u64, prevout_txid: Txid) -> Result<Honest> {
    let secp = Secp256k1::new();

    let sk1 = SecretKey::from_slice(&[0x21u8; 32])?;
    let sk2 = SecretKey::from_slice(&[0x63u8; 32])?;
    let pk1 = PublicKey::from_secret_key(&secp, &sk1);
    let pk2 = PublicKey::from_secret_key(&secp, &sk2);

    let id1 = MusigSessionId::assume_unique_per_nonce_gen([0x22u8; 32]);
    let id2 = MusigSessionId::assume_unique_per_nonce_gen([0x23u8; 32]);
    let seed_msg = Message::from_slice(&[0x24u8; 32])?;
    let (_s1, p1): (MusigSecNonce, MusigPubNonce) =
        new_musig_nonce_pair(&secp, id1, None, Some(sk1), pk1, Some(seed_msg), None)?;
    let (_s2, p2): (MusigSecNonce, MusigPubNonce) =
        new_musig_nonce_pair(&secp, id2, None, Some(sk2), pk2, Some(seed_msg), None)?;
    let aggnonce = MusigAggNonce::new(&secp, &[p1, p2]);

    let blinding = BlindingFactor::from_slice(&[0x2au8; 32])?;
    let tweak_in = SecretKey::from_slice(&[0x2cu8; 32])?;
    let (_parity, output_pubkey, out_tweak) =
        blinded_musig_pubkey_xonly_tweak_add(&secp, &pk1, tweak_in);

    // The prevout is P2TR over the tweaked key — byte-identical to what `calculate_musig_session`
    // builds for a real tier (`0x51 0x20 || xonly`).
    let mut spk_bytes = vec![0x51u8, 0x20];
    spk_bytes.extend_from_slice(&output_pubkey.x_only_public_key().0.serialize());
    let spk = ScriptBuf::from_bytes(spk_bytes.clone());

    let tx = Transaction {
        version: 3,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid: prevout_txid, vout: 0 },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xFFFF_FFFD),
            witness: Witness::new(),
        }],
        output: vec![TxOut { value: value - 500, script_pubkey: spk.clone() }],
    };

    let prevouts = vec![TxOut { value, script_pubkey: spk }];
    let sighash = SighashCache::new(&tx).taproot_key_spend_signature_hash(
        0,
        &Prevouts::All(&prevouts),
        TapSighashType::All,
    )?;

    let session = MusigSession::new_blinded_without_key_agg_cache(
        &secp,
        &output_pubkey,
        aggnonce,
        Message::from_slice(sighash.as_byte_array())?,
        None,
        &blinding,
        out_tweak,
    );

    Ok(Honest {
        unsigned_tx_hex: serialize_hex(&tx),
        prevout_value: value,
        prevout_spk_hex: hex::encode(&spk_bytes),
        agg_pubkey_hex: hex::encode(output_pubkey.serialize()),
        agg_nonce_hex: hex::encode(aggnonce.serialize()),
        blinding_hex: hex::encode(blinding.as_bytes()),
        out_tweak_hex: hex::encode(out_tweak.as_ref()),
        // The WIRE form — fin nonce stripped, as `calculate_musig_session` sends it.
        session_hex: hex::encode(session.remove_fin_nonce_from_session().serialize()),
    })
}

/// The disclosure body, with `prevout_values` overridable so the lie is a single field.
fn disclosure_body(sid: &str, h: &Honest, claimed_value: u64) -> serde_json::Value {
    serde_json::json!({
        "statechain_id": sid,
        "negate_seckey": 0,
        "session": h.session_hex,
        "disclosure": {
            "unsigned_tx": h.unsigned_tx_hex,
            "input_index": 0,
            "prevout_values": [claimed_value],
            "prevout_spks": [h.prevout_spk_hex],
            "agg_pubkey": h.agg_pubkey_hex,
            "agg_nonce": h.agg_nonce_hex,
            "blinding_factor": h.blinding_hex,
            "out_tweak": h.out_tweak_hex,
            "hash_type": 1
        }
    })
}

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn coins_of(cc: &ClientConfig, wallet_name: &str) -> Result<Vec<Coin>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins)
}

/// POST a `/get_partial_signature` request straight at the lockbox.
///
/// Direct rather than through the coordinator so the test controls the disclosure byte-for-byte —
/// the coordinator forwards the payload wholesale, so what it would send is exactly what is built
/// here, minus the ability to corrupt one field on purpose.
async fn raw_partial_signature(lockbox: &str, body: serde_json::Value) -> Result<(u16, String)> {
    let resp = reqwest::Client::new()
        .post(format!("{lockbox}/get_partial_signature"))
        .json(&body)
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text.chars().take(200).collect()))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let lockbox = std::env::var("LOCKBOX_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:18080".to_string());

    // ---- (a) THE HONEST PATH, end to end ------------------------------------------------------
    //
    // Every co-signature in this deposit+ladder now carries a disclosure. If the binding is wrong,
    // laddering fails here — which is the regression this half exists to catch.
    let (binds_before, reqs_before) = bind_stats();
    let alice = wallet("sdk92_alice").await?;
    let before: Vec<String> =
        coins_of(&cc, "sdk92_alice").await?.into_iter().filter_map(|c| c.statechain_id).collect();

    let t = mercuryrustlib::deposit::get_token(&cc).await?;
    alice.add_prepaid_token(&t.token_id).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;

    let mut sid = String::new();
    for _ in 0..60 {
        alice.claim().await?;
        if let Some(s) = coins_of(&cc, "sdk92_alice")
            .await?
            .into_iter()
            .filter_map(|c| c.statechain_id)
            .find(|s| !before.contains(s))
        {
            sid = s;
            // Keep claiming until the LADDER exists (or a skip is recorded). A statechain id
            // appears at deposit-init, well before confirmation and laddering, so breaking on the
            // id alone stops the test before any tier is ever co-signed.
            let laddered =
                mercuryrustlib::tesr::load(&cc, "sdk92_alice", &sid).await?.is_some();
            let skipped =
                mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk92_alice", &sid, 0)
                    .await
                    .is_some();
            if laddered || skipped {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if sid.is_empty() {
        return Err(anyhow!(
            "[a] alice never confirmed a coin. Every co-signature now carries a disclosure, so a \
             binding defect (wrong hash type, fin nonce not stripped, wrong prevout script) fails \
             here — and fails for EVERY signature, not intermittently."
        ));
    }
    println!("SDK92 - [a] deposit confirmed ({})", &sid[..8.min(sid.len())]);

    // THE COIN MUST BE LADDERED, or this test cannot see the lane it exists to cover.
    //
    // Tier co-signatures are the ones that go through `cosign_tier_request`, and that is where the
    // disclosed prevout value has to be the PARENT tier's output rather than `coin.amount`. A run
    // that only deposits never calls it, ends at `sig_count == 1`, and reports a 1-bound/1-request
    // ratio that looks like full coverage while proving nothing about tiers.
    //
    // Laddering is skipped SILENTLY when the SE's attestation identity is unpinned — the reason is
    // recorded rather than raised, so the failure surfaces as "no tiers" with no error anywhere.
    // Read that reason back and say it, instead of quietly measuring the narrow case.
    if let Some(reason) =
        mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk92_alice", &sid, 0).await
    {
        return Err(anyhow!(
            "[a] the coin was NOT laddered — recorded reason: {reason:?}. This run never reaches \
             `cosign_tier_request`, so it measures the deposit lane ONLY and any coverage ratio it \
             prints is vacuous.\n\
             \n\
             BOTH of these reasons mean the same thing here — a client-side attestation pin problem, \
             NOT a coordinator outage:\n\
               `attestation-identity-unpinned`  = no pin set at all\n\
               `coordinator-unavailable`        = a pin IS set but does not verify the served \
             attestation. The coordinator answered 200; the classifier only checks that a pin is \
             PRESENT, so a wrong one is mislabelled 'retry later'.\n\
             \n\
             Export the BARE x-only key — not the JSON body, which is what produces the second case:\n\
               export UTEXO_ATTESTATION_IDENTITY=$(curl -s {lockbox}/attestation_identity | \
             jq -r .attestation_identity_pubkey)"
        ));
    }
    if mercuryrustlib::tesr::load(&cc, "sdk92_alice", &sid).await?.is_none() {
        return Err(anyhow!(
            "[a] no ladder bundle for {sid} and no recorded skip reason either. The tier lane is \
             unexercised and unexplained — do not read the coverage ratio below as evidence."
        ));
    }
    println!("SDK92 - [a] PASS: coin laddered, so tier co-signatures were exercised");

    // A green deposit is NOT by itself evidence the gate ran: an absent disclosure is served, so a
    // client that failed to serialise one would look exactly like this. Count the SE's own
    // successful-bind log lines instead.
    let (binds_after, reqs_after) = bind_stats();
    let binds = binds_after - binds_before;
    let reqs = reqs_after - reqs_before;
    if binds == 0 {
        return Err(anyhow!(
            "[a] the deposit succeeded but the SE logged NO successful binding ({binds_before} -> \
             {binds_after}). Either the client is not attaching the disclosure or the gate is not \
             reached — in both cases the honest path proves nothing about binding, because an \
             absent disclosure is served."
        ));
    }
    println!("SDK92 - [a] binding coverage: {binds} bound / {reqs} co-signatures");
    // MEASURED, and a limit on what this half is worth: the coin ends with sig_count == 1, so this
    // flow makes ONE co-signature and never reaches `cosign_tier_request`. The ratio below is
    // therefore honest but narrow — it cannot see the TIER lane, which is precisely where the
    // prevout-value drift lived. Do not read a 1/1 here as "the lifecycle is bound".
    if binds != reqs {
        return Err(anyhow!(
            "[a] only {binds} of {reqs} co-signatures were bound. The unbound ones reached the SE \
             with NO disclosure and were signed unchecked. A partially wired gate reads as a \
             working one — every co-signature must carry a disclosure before REQ-57 may be \
             described as covering the lifecycle. (Known cause class: a builder that does not pass \
             the prevouts it hashed; see `calculate_musig_session`.)"
        ));
    }

    // The coin is laddered, which means the SE co-signed tiers while binding each one.
    let coin = coins_of(&cc, "sdk92_alice")
        .await?
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid.as_str()))
        .ok_or_else(|| anyhow!("[a] the confirmed coin vanished from the wallet"))?;
    println!("SDK92 - [a] coin amount {:?}", coin.amount);

    // ---- (b) THE TAMPERED DISCLOSURE ----------------------------------------------------------
    //
    // Reuse the shape of a real request but corrupt ONE field: the disclosed prevout value, by one
    // satoshi. The SE never checks that field directly — BIP-341 folds it into the sighash, so the
    // lie surfaces as a session mismatch. If this is accepted, the binding is decorative.
    // The transaction is REAL and the session is the one it genuinely produces. An earlier version of
    // this test sent garbage hex, was refused by the PARSER, and reported a pass — the refusal was
    // real but measured the wrong gate entirely. The pair below cannot make that mistake: both
    // requests are byte-identical apart from one satoshi, so only the amount can explain a
    // difference in outcome.
    let honest = build_honest(DEPOSIT)?;

    // (b1) THE LIE.
    let (lie_status, lie_body) =
        raw_partial_signature(&lockbox, disclosure_body(&sid, &honest, DEPOSIT + 1)).await?;
    println!("SDK92 - [b1] value+1 -> HTTP {lie_status}  {lie_body}");
    if lie_status == 200 {
        return Err(anyhow!(
            "[b1] the SE ACCEPTED a disclosure whose prevout value is one satoshi off. Witness \
             binding is not enforcing, and every rule in SPEC §5.4 that rests on it is unsupported."
        ));
    }
    // The refusal must be the SESSION COMPARE, not the parser and not some unrelated gate.
    if !lie_body.contains("does not produce the session") {
        return Err(anyhow!(
            "[b1] refused with HTTP {lie_status}, but NOT by the session comparison: {lie_body}. \
             A refusal from the parser or another gate says nothing about whether BIP-341's \
             commitment to the prevout amount is actually being checked. This is exactly the way \
             the previous version of this test passed while proving nothing."
        ));
    }

    // (b2) THE SAME BYTES, CORRECT VALUE — and this is REQ-68's decisive case, not a positive
    // control any more.
    //
    // `honest` is built from fixed keys (0x21.., 0x63..) that have nothing to do with this coin, and
    // it is submitted under THIS coin's sid. That is precisely the attack REQ-68 exists to stop: a
    // disclosure that is internally self-consistent, so REQ-57's session compare is satisfied by it,
    // describing a transaction the caller does not own. Before REQ-68 the SE served this and
    // recorded SE-authored state (se_signed_tx, se_latch, and in future the leaf registry) derived
    // from someone else's transaction — which is what would poison REQ-56's collapse predicate.
    //
    // So (b2) MUST be refused, and refused by the AGGREGATE check rather than by the session
    // compare. The two gates catch different faults and the test must not let one stand in for the
    // other.
    let (ok_status, ok_body) =
        raw_partial_signature(&lockbox, disclosure_body(&sid, &honest, DEPOSIT)).await?;
    println!("SDK92 - [b2] correct value -> HTTP {ok_status}  {ok_body}");
    if ok_status == 200 {
        return Err(anyhow!(
            "[b2] the SE SERVED a self-consistent disclosure describing a transaction built from \
             keys unrelated to this coin, submitted under this coin's sid. REQ-68 is not enforcing, \
             so any caller can author SE-side state about a coin it does not own."
        ));
    }
    if ok_body.contains("witness binding refused") {
        return Err(anyhow!(
            "[b2] refused, but by the SESSION COMPARE ({ok_body}) rather than by the aggregate \
             check. That would mean REQ-57 is masking REQ-68: the same refusal fires whether or not \
             the coin binding exists, so this test would keep passing after REQ-68 was removed."
        ));
    }
    if !ok_body.contains("not this coin's") {
        return Err(anyhow!(
            "[b2] refused with HTTP {ok_status}, but by neither named gate: {ok_body}. A refusal \
             from the parser or an unrelated check proves nothing about the coin binding."
        ));
    }

    // The ACCEPTANCE side of the differential is (a), not (b2): 4 bound / 4 co-signatures over this
    // coin's OWN tiers. That is what makes (b1) and (b2) detections rather than a gate that refuses
    // everything — and it is also the only evidence that the SE's own derivation of the aggregate
    // (combine -> TapTweak -> tweak-add -> x-only) reproduces the client's `output_pubkey` bit for
    // bit. A single bit of disagreement would have refused all four.
    println!(
        "SDK92 - [b] PASS: two INDEPENDENT gates, each firing on its own fault. A one-satoshi lie is \
         caught by BIP-341 folding the prevout amount into the sighash (400, session mismatch); a \
         truthful disclosure of the WRONG COIN is caught by the aggregate the SE derived for itself \
         at keygen (403). Neither refusal touches this coin's own tiers, which were served \
         {binds} of {reqs}."
    );

    // ===== (c) [V-7] A COIN WITH NO STORED AGGREGATE IS REFUSED, NOT SERVED ====================
    //
    // The aggregate check used to FAIL OPEN here: no stored aggregate meant "an old client sent no
    // key", and refusing would have bricked every pre-existing coin. That reasoning expired when the
    // coordinator started refusing an empty `user_public_key` — the unbound set became finite and
    // closed instead of growing on demand — so what is left behind this branch is exactly the
    // pre-0009 population D24 already ignores. Serving them was a hole any caller could aim at by
    // naming a legacy sid.
    //
    // Minted by asking the LOCKBOX directly, without a key: that is the one way left to produce an
    // aggregate-less sid, and it is precisely the shape the fail-open used to serve.
    let bare_sid = format!("{:032x}", rand::random::<u128>());
    let mint = reqwest::Client::new()
        .post(format!("{lockbox}/get_public_key"))
        .json(&serde_json::json!({ "statechain_id": bare_sid }))
        .send()
        .await?;
    assert!(mint.status().is_success(), "the lockbox must still mint a keypair without a client key");
    let (bare_status, bare_body) =
        raw_partial_signature(&lockbox, disclosure_body(&bare_sid, &honest, DEPOSIT)).await?;
    println!("SDK92 - [c] no stored aggregate -> HTTP {bare_status}  {bare_body}");
    if bare_status == 200 {
        return Err(anyhow!(
            "[c] a disclosure was SERVED for a coin with no aggregate on record. The check is \
             failing OPEN, so naming any legacy sid buys an unchecked co-signature (V-7)."
        ));
    }
    if !bare_body.contains("no aggregate on record") {
        return Err(anyhow!(
            "[c] refused with HTTP {bare_status}, but not by the aggregate-absent branch: \
             {bare_body}. A refusal from the parser or another gate would keep passing after V-7 \
             was reverted."
        ));
    }
    println!(
        "SDK92 - [c] PASS: the fail-open is CLOSED. An aggregate-less coin cannot buy a co-signature \
         against a disclosure nobody can check."
    );

    // ===== (d) [#162] AN UNBOUND REQUEST IS REFUSED ===========================================
    //
    // The residual (c) used to name: binding was OPT-IN, so a caller that simply omitted the field
    // was served without any check at all. An optional security gate is not a gate — it protects
    // exactly the callers who chose to be protected, and the attacker is by definition the one who
    // does not.
    //
    // This request is the (a) request with ONE field deleted. Everything else — sid, session,
    // negate_seckey — is byte-identical to a request the SE served four times in this same run, so
    // a refusal here cannot be explained by anything except the missing disclosure. That is what
    // makes this a test of the mandatory rule rather than of the parser.
    let mut unbound = disclosure_body(&sid, &honest, DEPOSIT);
    unbound
        .as_object_mut()
        .expect("the body is an object")
        .remove("disclosure")
        .expect("the (a) body carried a disclosure to remove");
    let (unbound_status, unbound_body) = raw_partial_signature(&lockbox, unbound).await?;
    println!("SDK92 - [d] disclosure omitted -> HTTP {unbound_status}  {unbound_body}");
    if unbound_status == 200 {
        return Err(anyhow!(
            "[d] the SE co-signed a request carrying NO disclosure. Binding is still opt-in, which \
             means REQ-57 protects only the callers who opt in — and an attacker never will."
        ));
    }
    if !unbound_body.contains("Refusing to co-sign blind") {
        return Err(anyhow!(
            "[d] refused with HTTP {unbound_status}, but not by the mandatory-binding branch: \
             {unbound_body}. A refusal from the parser or another gate would keep passing after the \
             mandatory rule was reverted."
        ));
    }
    println!(
        "SDK92 - [d] PASS: binding is MANDATORY. The same request the SE served with a disclosure is \
         refused without one, so REQ-57 is a property of the SE and not a convention among clients."
    );
    // ===== (e) [#171] CAN A CALLER GRAFT ITS LEAF ONTO SOMEBODY ELSE'S? ========================
    //
    // The frontier is "every node that is not the parent of another", and `owed` pays only FRONTIER
    // leaves. So a holder can be removed from the payout set WITHOUT forging their release: give
    // their leaf a CHILD, and it stops being frontier.
    //
    // The parent edge is resolved from `signed_tx_owners(prevout_txid)` — the sid the SE co-signed
    // that transaction under. The question this case answers is whether anything stops a caller
    // naming a prevout that belongs to a DIFFERENT coin. The SE cannot look at the chain (REQ-58),
    // so it cannot check that the outpoint really pays this coin; the disclosure is self-consistent
    // by construction, because the caller supplies the prevout spk and value it wants hashed.
    //
    // The signature produced would be useless — it would not validate against the real output — but
    // a useless signature that still writes a registry row is exactly the problem: the row is what
    // the frontier is computed from.
    let victim_bundle = mercuryrustlib::tesr::load(&cc, "sdk92_alice", &sid)
        .await?
        .ok_or(anyhow!("[e] the victim coin has no ladder to borrow a txid from"))?;
    let victim_txid: Txid = victim_bundle.trigger.txid.parse()?;

    let t2 = mercuryrustlib::deposit::get_token(&cc).await?;
    alice.add_prepaid_token(&t2.token_id).await;
    let addr2 = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr2)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut attacker_sid = String::new();
    for _ in 0..60 {
        alice.claim().await?;
        let coins = coins_of(&cc, "sdk92_alice").await?;
        if let Some(c) = coins.iter().find(|c| {
            c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.statechain_id.as_deref() != Some(sid.as_str())
        }) {
            attacker_sid = c.statechain_id.clone().unwrap_or_default();
            if !attacker_sid.is_empty() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if attacker_sid.is_empty() {
        return Err(anyhow!("[e] could not obtain a second coin to attack from"));
    }

    let graft = build_honest_spending(DEPOSIT, victim_txid)?;
    let (graft_status, graft_body) =
        raw_partial_signature(&lockbox, disclosure_body(&attacker_sid, &graft, DEPOSIT)).await?;
    println!(
        "SDK92 - [e] GRAFT PROBE: attacker {} signed a tx whose prevout is {}, a transaction the SE \
         co-signed under victim {} -> HTTP {graft_status}  {graft_body}",
        &attacker_sid[..8.min(attacker_sid.len())],
        &victim_bundle.trigger.txid[..16.min(victim_bundle.trigger.txid.len())],
        &sid[..8.min(sid.len())]
    );
    println!(
        "SDK92 - [e] INSPECT: SELECT statechain_id, parent_statechain_id FROM se_leaf WHERE \
         statechain_id IN ('{attacker_sid}','{sid}');"
    );
    // **THIS PROBE IS INCONCLUSIVE AS BUILT, AND MUST NOT BE READ AS A PASS.**
    //
    // `build_honest_spending` derives its aggregate from FIXED test keys, so the disclosure carries
    // an aggregate belonging to no real coin and REQ-68 refuses it (403) before `observe_leaf` is
    // ever reached. That is the wrong door: a real attacker signs under the aggregate of a coin it
    // actually owns, which is public and which it can reproduce.
    //
    // What a conclusive probe needs: the attacker's OWN coin keys, so the disclosure passes REQ-68
    // (agg matches its sid) and REQ-57 (session rebuilt from a self-consistent disclosure, with the
    // attacker's own prevout spk and value) — while the transaction's INPUT OUTPOINT names a
    // transaction the SE co-signed under the victim. Neither gate examines the outpoint's ownership,
    // and REQ-58 forbids the SE from resolving it on chain.
    //
    // Until that probe exists, the graft question is OPEN. A 403 here is evidence about REQ-68, not
    // about the frontier.
    if graft_status == 403 && graft_body.contains("not this coin's") {
        println!(
            "SDK92 - [e] INCONCLUSIVE: refused by REQ-68 (wrong aggregate), so the graft path was \
             never reached. This is NOT evidence that the frontier is safe — see the comment above \
             for what a real probe requires."
        );
    } else if graft_status == 200 {
        println!(
            "SDK92 - [e] the SE CO-SIGNED it. Whether that poisoned the frontier depends on the \
             registry row — check the query above before concluding either way."
        );
    }

    println!("SDK92 - ALL ASSERTIONS PASSED");
    Ok(())
}
