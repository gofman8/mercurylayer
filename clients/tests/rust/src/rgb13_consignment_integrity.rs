//! E2E (rgb-tests parity, negative, RGB_E2E=13): **consignment integrity over a COLOURED LADDER
//! TIER.** The receiver of a coloured ladder must ACCEPT a well-formed tier consignment against the
//! tier's own UN-BROADCAST txid but REJECT one whose payload has been tampered with, one presented
//! against a witness that is not its own, and one presented against a RIVAL tier over the same
//! parent output. This is the statechain analogue of the upstream `validate_consignment_*_fail`
//! family — `validate_offchain` / `validate_offchain_chain` is the single gate that protects a
//! receiver before it books a coin it was handed off-chain.
//!
//! Re-derived for the ladder rule. The consignment used to come from an un-broadcast
//! `create_colored_split_tx` over a funding output — the retired off-chain branch lane. It now comes
//! from `build_colored_tier`, the CTES-R tier builder: the coloured trigger `T` over a confirmed
//! issuance outpoint `F`, plus two RIVAL extensions `X_a` / `X_b` over `T`'s payload output (rivals
//! over one outpoint are the NORMAL ladder case — every renewal and transfer makes one). Nothing is
//! broadcast and nothing is co-signed; validation never touches the coordinator.
//!
//!   POSITIVE:    T.consignment vs T.txid              => valid (single-tier and chain forms)
//!   NEGATIVE 1:  payload-corrupted T.consignment       => rejected (Err or valid=false)
//!   NEGATIVE 2:  T.consignment vs an all-zero txid     => rejected
//!   NEGATIVE 3:  X_a.consignment vs [T, X_b.txid]      => rejected — the receiver was told the
//!                sibling rival's txid instead of the tier it was handed, so X_a's own witness is
//!                not in the off-chain list and cannot be resolved.
//!   CONTROL:     X_a.consignment vs [T, X_a.txid]      => valid (so NEGATIVE 3 is about the list,
//!                not about the tier).
//!
//! Requires bitcoind + electrs:50001 + the RGB proxy:3000 only (no Mercury server, no lockbox).

use std::{fs, str::FromStr};

use anyhow::{anyhow, Result};
use electrum_client::bitcoin::{
    secp256k1::{PublicKey, Secp256k1, SecretKey},
    Address, Network, Transaction, Txid,
};
use electrum_client::{Client, ElectrumApi};
use mercury_rgb::{consignment_witness_txids, RgbWallet};
use mercurylib::tesr;
use mercuryrustlib::rgb::{
    build_colored_tier, colored_tier_out_total, ColoredTier, ColoredTierSpec, TierRole, TierSeal,
};

use crate::bitcoin_core;

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const ISSUED: u64 = 600;
/// THIS FIXTURE'S rate, not the protocol's (see rgb15).
const FEE_RATE: f64 = 2.0;
const CSV_D: u16 = 36;
const SID: &str = "rgb13-coin";

// ---------------------------------------------------------------------------------- small helpers

/// A deterministic regtest P2TR address, keyed on `seed`, so a failing run is reproducible.
fn p2tr_address(seed: u64) -> Result<String> {
    let mut bytes = [1u8; 32];
    bytes[..8].copy_from_slice(&seed.to_be_bytes());
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&bytes)?;
    let (xonly, _) = PublicKey::from_secret_key(&secp, &sk).x_only_public_key();
    Ok(Address::p2tr(&secp, xonly, None, Network::Regtest).to_string())
}

/// The scriptPubKey + value of a CONFIRMED outpoint, read from the indexer.
fn onchain_prevout(txid: &str, vout: u32) -> Result<(String, u64)> {
    let client = Client::new(ELECTRUM_URL)?;
    let raw = client.transaction_get_raw(&Txid::from_str(txid)?)?;
    let tx: Transaction = electrum_client::bitcoin::consensus::deserialize(&raw)?;
    let out = tx
        .output
        .get(vout as usize)
        .ok_or_else(|| anyhow!("{txid}:{vout} is out of range"))?;
    Ok((hex::encode(out.script_pubkey.as_bytes()), out.value))
}

/// Does the indexer know this txid (mempool or chain)?
fn tx_exists(txid: &str) -> Result<bool> {
    let client = Client::new(ELECTRUM_URL)?;
    Ok(client.transaction_get_raw(&Txid::from_str(txid)?).is_ok())
}

/// Fresh RGB wallet + a fresh NIA issuance, returning the wallet, the contract id and the
/// (confirmed, on-chain) outpoint `F` holding the whole issuance: `(rgb, contract, txid, vout, value, spk_hex)`.
fn setup(data_dir: &str, ticker: &str) -> Result<(RgbWallet, String, String, u32, u64, String)> {
    let _ = fs::remove_dir_all(data_dir);
    fs::create_dir_all(data_dir)?;
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(500_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    rgb.create_utxos(1, 200_000, 2)?;
    let _ = bitcoin_core::generatetoaddress(2, &core)?;
    rgb.refresh(None)?;
    let contract = rgb.issue_nia(ticker, "Integrity asset", 0, vec![ISSUED])?;
    let _ = bitcoin_core::generatetoaddress(2, &core)?;
    rgb.refresh(None)?;

    let (outpoint, _, _) = rgb
        .list_allocations(&contract)?
        .into_iter()
        .find(|(_, amount, _)| *amount == ISSUED)
        .ok_or_else(|| anyhow!("no issuance allocation for {contract}"))?;
    let (txid, vout) = outpoint.split_once(':').ok_or_else(|| anyhow!("bad outpoint"))?;
    let vout: u32 = vout.parse()?;
    let (spk_hex, value) = onchain_prevout(txid, vout)?;
    Ok((rgb, contract, txid.to_string(), vout, value, spk_hex))
}

/// Build and colour ONE un-broadcast, one-payload tier over `prev = (txid, vout, value, spk_hex)`,
/// carrying the whole allocation forward, sealed with this coin's derived `TierSeal`.
fn tier(
    rgb: &RgbWallet,
    contract: &str,
    prev: (&str, u32, u64, &str),
    sequence: u32,
    role: TierRole,
    tier_index: u32,
    payee_seed: u64,
) -> Result<ColoredTier> {
    let (prev_txid, prev_vout, prev_value, prev_spk_hex) = prev;
    let out_value = colored_tier_out_total(prev_value, 1, FEE_RATE).ok_or_else(|| {
        anyhow!("parent {prev_txid}:{prev_vout} ({prev_value} sat) is too small for a coloured tier")
    })?;
    let payloads = vec![(p2tr_address(payee_seed)?, out_value, ISSUED)];
    tokio::task::block_in_place(|| {
        build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: contract,
                prev_txid,
                prev_vout,
                prev_value,
                prev_spk_hex,
                sequence,
                payloads: &payloads,
                network: NETWORK,
                fee_rate: FEE_RATE,
                nonce: None,
            },
            &TierSeal::new(SID, role, tier_index, 0),
        )
    })
}

/// Corrupt a run of base64 chars in the middle of a consignment (keeps it valid base64 but mangles
/// the decoded payload, so the RGB `Transfer` fails to load/validate).
fn tamper_consignment(consignment_b64: &str) -> String {
    let mut t = consignment_b64.to_string();
    let len = t.len();
    if len < 64 {
        return t; // too short to meaningfully tamper (shouldn't happen for a real consignment)
    }
    let start = len / 2;
    // 16 valid-base64 chars, guaranteed different from the original run at this position.
    let run: String = (0..16)
        .map(|i| {
            let orig = t.as_bytes()[start + i];
            let c = if orig == b'A' { b'B' } else { b'A' };
            c as char
        })
        .collect();
    t.replace_range(start..start + 16, &run);
    t
}

fn rejected(res: Result<(bool, Option<String>)>) -> (bool, String) {
    match res {
        Ok((valid, detail)) => (!valid, format!("valid={valid} detail={:?}", detail)),
        Err(e) => (true, format!("Err({e})")),
    }
}

pub async fn execute() -> Result<()> {
    std::env::set_var("ML_NETWORK", NETWORK);
    let _ = fs::remove_dir_all("./rgb-data13");

    let (rgb, contract, f_txid, f_vout, f_value, f_spk) =
        tokio::task::block_in_place(|| setup("./rgb-data13/issuer", "INTGR"))?;
    println!("RGB13 - issued {ISSUED} units of {contract}; F = {f_txid}:{f_vout} ({f_value} sat, confirmed)");

    // The coloured trigger T over F, and two RIVAL extensions over T's payload output — all
    // UN-broadcast. Each rival has its own derived seal, so each consignment carries its own witness.
    let t = tier(&rgb, &contract, (&f_txid, f_vout, f_value, &f_spk), tesr::TRIGGER_SEQUENCE.0, TierRole::Trigger, 0, 1)?;
    let t_out = t.payloads[0].clone();
    let x_a = tier(
        &rgb, &contract, (&t.txid, t_out.vout, t_out.value, &t_out.script_pubkey_hex),
        tesr::csv_blocks(CSV_D).0, TierRole::Extension, 0, 2,
    )?;
    let x_b = tier(
        &rgb, &contract, (&t.txid, t_out.vout, t_out.value, &t_out.script_pubkey_hex),
        tesr::csv_blocks(CSV_D).0, TierRole::Extension, 1, 3,
    )?;
    println!("RGB13 - built T = {} (NOT broadcast), rivals X_a = {} / X_b = {} over T:{}", t.txid, x_a.txid, x_b.txid, t_out.vout);
    assert_ne!(x_a.txid, x_b.txid, "the two rivals must be distinct transactions");
    for (label, tr) in [("T", &t), ("X_a", &x_a), ("X_b", &x_b)] {
        assert!(!tx_exists(&tr.txid)?, "{label} ({}) must NOT be on chain", tr.txid);
        assert!(
            consignment_witness_txids(&tr.consignment)?.contains(&tr.txid),
            "{label}'s consignment must carry its own witness"
        );
    }

    // POSITIVE: the untouched trigger consignment validates against its own witness txid, in both
    // the single-tier and the chain form a receiver uses.
    let (valid, detail) = tokio::task::block_in_place(|| rgb.validate_offchain(&t.consignment, &t.txid))?;
    println!("RGB13 - [POSITIVE] validate_offchain(T, T.txid) -> valid={valid} detail={detail:?}");
    assert!(valid, "the well-formed tier consignment MUST validate off-chain: {detail:?}");
    let (valid_chain, detail_chain) =
        tokio::task::block_in_place(|| rgb.validate_offchain_chain(&t.consignment, &[t.txid.clone()]))?;
    println!("RGB13 - [POSITIVE] validate_offchain_chain(T, [T.txid]) -> valid={valid_chain} detail={detail_chain:?}");
    assert!(valid_chain, "the well-formed tier consignment MUST validate in chain form: {detail_chain:?}");

    // NEGATIVE 1: a payload-corrupted consignment is rejected.
    let tampered = tamper_consignment(&t.consignment);
    assert_ne!(tampered, t.consignment, "tamper helper must actually change the consignment");
    let (rej1, why1) = rejected(tokio::task::block_in_place(|| rgb.validate_offchain(&tampered, &t.txid)));
    println!("RGB13 - [NEGATIVE 1] tampered consignment -> rejected={rej1} ({why1})");
    assert!(rej1, "a payload-corrupted consignment MUST be rejected (got {why1})");

    // NEGATIVE 2: the genuine consignment presented against a bogus witness txid is rejected.
    let bogus = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    let (rej2, why2) = rejected(tokio::task::block_in_place(|| rgb.validate_offchain_chain(&t.consignment, &[bogus])));
    println!("RGB13 - [NEGATIVE 2] consignment vs bogus witness txid -> rejected={rej2} ({why2})");
    assert!(rej2, "a consignment validated against a witness txid that is not its own MUST be rejected (got {why2})");

    // NEGATIVE 3: X_a's consignment presented against the SIBLING rival's txid. The receiver was
    // handed X_a but named X_b as the off-chain witness, so X_a's own witness is not in the list
    // and cannot be resolved — the ladder-shaped "wrong witness".
    let wrong_rival = vec![t.txid.clone(), x_b.txid.clone()];
    let (rej3, why3) = rejected(tokio::task::block_in_place(|| rgb.validate_offchain_chain(&x_a.consignment, &wrong_rival)));
    println!("RGB13 - [NEGATIVE 3] X_a consignment vs [T, X_b] -> rejected={rej3} ({why3})");
    assert!(rej3, "a tier consignment validated against its RIVAL's txid MUST be rejected (got {why3})");

    // CONTROL: the same X_a consignment against its OWN chain [T, X_a] validates — so NEGATIVE 3 is
    // about the witness list, not about the tier.
    let right = vec![t.txid.clone(), x_a.txid.clone()];
    let (valid_a, detail_a) = tokio::task::block_in_place(|| rgb.validate_offchain_chain(&x_a.consignment, &right))?;
    println!("RGB13 - [CONTROL] X_a consignment vs [T, X_a] -> valid={valid_a} detail={detail_a:?}");
    assert!(valid_a, "X_a must validate against its own chain [T, X_a]: {detail_a:?}");

    // Validation broadcast nothing.
    for (label, tr) in [("T", &t), ("X_a", &x_a), ("X_b", &x_b)] {
        assert!(!tx_exists(&tr.txid)?, "{label} must still be un-broadcast after validation");
    }

    println!(
        "RGB13 - SUCCESS: the receiver of a coloured ladder ACCEPTS a well-formed tier consignment \
         against the tier's own un-broadcast witness but REJECTS a payload-tampered one, one \
         presented against a bogus witness, and one presented against its rival's txid — \
         consignment integrity holds over the ladder."
    );
    Ok(())
}
