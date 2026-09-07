//! E2E (RGB_E2E=12): **off-chain resolver safety over a COLOURED LADDER — the receiver REJECTS a
//! leaf consignment when an un-broadcast ancestor TIER's witness txid is omitted.**
//!
//! Re-derived for the ladder rule. There are no un-broadcast BRANCH transactions over a funding
//! output any more (that lane is retired: a coin's only exit is its TES-R ladder, whose trigger
//! already spends `F`). What a receiver validates is the COLOURED LADDER itself — `T` over `F`, `X`
//! over `T`'s payload output, `S` over `X`'s — every tier built, coloured and left UN-BROADCAST by
//! `build_colored_tier`, the CTES-R tier builder the SDK co-signs at establish (sdk74) and at every
//! renewal, transfer and in-ladder split (sdk77). A receiver books a coin only if the leaf
//! consignment resolves against EVERY un-broadcast tier in the chain it is handed
//! (`validate_offchain_chain` — the gate `accept_ladder` / `colored_child_health` run); if an
//! ancestor witness is unaccounted for, the single-use / value-conservation guarantee is void and
//! the receiver must refuse.
//!
//!   F  = a confirmed issuance outpoint holding 600 (on chain)
//!   T  = coloured trigger over F            (un-broadcast)
//!   X  = coloured extension over T's payload (un-broadcast)
//!   S  = coloured state over X's payload     (un-broadcast — the leaf a receiver is handed)
//!
//!   POSITIVE: validate_offchain_chain(S.consignment, [T, X, S]) => valid == true
//!             (every un-broadcast ancestor witness supplied — the resolver can walk the ladder).
//!   NEGATIVE: validate_offchain_chain(S.consignment, [X, S])    => valid == false AND
//!             detail == Some(..) — `T`, which funds `X`'s input, is missing from the off-chain
//!             witness list, so the resolver falls through to the indexer for it and finds nothing.
//!   NEGATIVE: validate_offchain_chain(S.consignment, [S])       => valid == false (both ancestors
//!             omitted).
//!
//! No coin is deposited and nothing is co-signed: the tiers here are the same transactions the
//! ladder co-signs, and colouring/validation never touch the coordinator. Requires bitcoind +
//! electrs:50001 + the RGB proxy:3000 only (no Mercury server, no lockbox).

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
/// THIS FIXTURE'S rate, not the protocol's (see rgb15): the tiers are built and checked at one
/// self-consistent rate; nothing here has to agree with a ladder the SDK built.
const FEE_RATE: f64 = 2.0;
const CSV_D: u16 = 36;
/// The statechain id the tier seals are derived from. Any string: both parties derive the seal
/// blinding from `(sid, role, index, rung)`, and only the derivation matters here.
const SID: &str = "rgb12-coin";

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
    let contract = rgb.issue_nia(ticker, "RGB12 resolver safety", 0, vec![ISSUED])?;
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

pub async fn execute() -> Result<()> {
    std::env::set_var("ML_NETWORK", NETWORK);
    let _ = fs::remove_dir_all("./rgb-data12");

    let (rgb, contract, f_txid, f_vout, f_value, f_spk) =
        tokio::task::block_in_place(|| setup("./rgb-data12/issuer", "R12"))?;
    println!("RGB12 - issued {ISSUED} units of {contract}; F = {f_txid}:{f_vout} ({f_value} sat, confirmed)");

    // ---- The coloured ladder, every tier UN-broadcast: T over F, X over T, S over X. ----
    let t = tier(&rgb, &contract, (&f_txid, f_vout, f_value, &f_spk), tesr::TRIGGER_SEQUENCE.0, TierRole::Trigger, 0, 1)?;
    let t_out = t.payloads[0].clone();
    let x = tier(
        &rgb, &contract, (&t.txid, t_out.vout, t_out.value, &t_out.script_pubkey_hex),
        tesr::csv_blocks(CSV_D).0, TierRole::Extension, 0, 2,
    )?;
    let x_out = x.payloads[0].clone();
    let s = tier(
        &rgb, &contract, (&x.txid, x_out.vout, x_out.value, &x_out.script_pubkey_hex),
        tesr::csv_blocks(CSV_D).0, TierRole::State, 0, 3,
    )?;
    println!("RGB12 - T = {} (payload vout {})", t.txid, t_out.vout);
    println!("RGB12 - X = {} (payload vout {}) over T:{}", x.txid, x_out.vout, t_out.vout);
    println!("RGB12 - S = {} over X:{}  <- the leaf a receiver is handed", s.txid, x_out.vout);

    // Every tier must be off-chain at validation time, and F must still be unspent.
    for (label, tr) in [("T", &t), ("X", &x), ("S", &s)] {
        assert!(!tx_exists(&tr.txid)?, "{label} ({}) must NOT be on chain — the ladder is un-broadcast", tr.txid);
    }
    let witnesses = consignment_witness_txids(&s.consignment)?;
    println!("RGB12 - S consignment witnesses = {witnesses:?}");
    for (label, tr) in [("T", &t), ("X", &x), ("S", &s)] {
        assert!(
            witnesses.contains(&tr.txid),
            "the leaf consignment must embed {label}'s witness ({}) — otherwise the positive case \
             below would prove nothing about the ancestor list",
            tr.txid
        );
    }

    // ---- POSITIVE: validate the leaf against the FULL ancestor chain [T, X, S]. ----
    let full = vec![t.txid.clone(), x.txid.clone(), s.txid.clone()];
    let (valid_ok, detail_ok) =
        tokio::task::block_in_place(|| rgb.validate_offchain_chain(&s.consignment, &full))?;
    println!("RGB12 - [POSITIVE] validate_offchain_chain([T, X, S]) -> valid={valid_ok} detail={detail_ok:?}");
    assert!(
        valid_ok,
        "receiver MUST validate the leaf when EVERY un-broadcast ancestor tier is supplied: {detail_ok:?}"
    );

    // ---- NEGATIVE (the point of this test): omit T, supply only [X, S]. ----
    // T's witness funds X's input; not in the off-chain list, the resolver asks the indexer for it,
    // which has never seen it. The receiver must REJECT and say why.
    let no_t = vec![x.txid.clone(), s.txid.clone()];
    let (valid_bad, detail_bad) =
        tokio::task::block_in_place(|| rgb.validate_offchain_chain(&s.consignment, &no_t))?;
    println!("RGB12 - [NEGATIVE] validate_offchain_chain([X, S], T omitted) -> valid={valid_bad} detail={detail_bad:?}");
    assert!(
        !valid_bad,
        "receiver MUST REJECT the leaf when the un-broadcast trigger's witness txid is omitted \
         (unresolved ancestor tier)"
    );
    assert!(
        detail_bad.is_some(),
        "a rejection must carry a detail explaining the missing/unresolved ancestor witness"
    );

    // ---- NEGATIVE: the leaf alone, both ancestors omitted. ----
    let only_s = vec![s.txid.clone()];
    let (valid_leaf, detail_leaf) =
        tokio::task::block_in_place(|| rgb.validate_offchain_chain(&s.consignment, &only_s))?;
    println!("RGB12 - [NEGATIVE] validate_offchain_chain([S] only) -> valid={valid_leaf} detail={detail_leaf:?}");
    assert!(
        !valid_leaf,
        "receiver MUST REJECT the leaf when both un-broadcast ancestor tiers are omitted"
    );

    // Nothing was broadcast by validating: the ladder is still entirely off-chain.
    for (label, tr) in [("T", &t), ("X", &x), ("S", &s)] {
        assert!(!tx_exists(&tr.txid)?, "{label} must still be un-broadcast after validation");
    }

    println!(
        "RGB12 - SUCCESS: over a coloured ladder the off-chain resolver ACCEPTS the leaf against the \
         full un-broadcast tier chain [T, X, S] but REJECTS it (valid=false, detail={detail_bad:?}) \
         when the trigger's witness txid is omitted, and again with only the leaf. A receiver will \
         not book a coin whose ancestor tier is unaccounted for."
    );
    Ok(())
}
