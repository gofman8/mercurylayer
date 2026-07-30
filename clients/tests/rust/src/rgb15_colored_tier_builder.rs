//! **RGB_E2E=15 — the CTES-R coloured tier builder and per-tier seal blinding.**
//!
//! Promotes the E1 and E2 gate harnesses (`docs/utexo/CTESR-GATE.md` §2.1, §2.2) into a standing
//! regression test for commit 3. Nothing here is wired into the live TES-R ladder — every
//! transaction is built, coloured and left **unsigned and un-broadcast**.
//!
//! What it pins:
//!
//! * **A** — a 1-payload coloured tier (`T`/`X_m`/`S_k` shape: P2TR payload + 240-sat P2A anchor,
//!   nVersion 3, BIP-68 nSequence) colours, lands its `opret` where the fork puts it rather than
//!   where anyone assumed, exposes EXPLICIT payload vouts, pays **exactly 2.000 sat/vB**, and
//!   `validate_offchain` returns `valid=true` against its **un-broadcast** txid.
//! * **B** — the same for an N-payload split-state (`SP`) shape, at n=3. Exactly 2.000 sat/vB again:
//!   the shortfall the gate measured is a constant `43 × rate`, independent of `n`, and
//!   `committed_fee_for_outputs(n + 1, rate)` closes it exactly.
//! * **C** — the reason `create_colored_split_tx` must NOT be reused: its `!is_op_return` vout
//!   filter counts the P2A anchor as a payload and trips its own length guard (§3.5).
//! * **D** — **the decisive one.** FOUR rival extensions over the same parent output, each with its
//!   own derived `TierSeal`; the tier chosen to continue from is deliberately the rival with the
//!   **maximum internal (little-endian) txid**, i.e. never the minimum. The state built over it must
//!   embed THAT rival's witness and no other, and the true branch must validate off-chain. A 2-rival
//!   test is not acceptable evidence — the gate observed it passing ~50% of the time by luck.
//! * **E** — the negative control that gives D its power: the identical 4-rival shape coloured with
//!   the retired global constant `777`. The collapse must reproduce: the max-txid rival's
//!   consignment comes back WITHOUT its own witness, carrying the min-txid rival's instead, and the
//!   true branch fails to validate. Deterministic because the consignment under test is taken
//!   **after** every rival is in the stock — see [`part_e`]; taking it at colour time is what used
//!   to make this part fail on roughly half of its runs.
//! * **F** — the build-time collision assert. A rival is *ground* (by varying its payee) until its
//!   internal txid is strictly greater than an existing rival's, then coloured with that rival's
//!   exact `TierSeal`. `build_colored_tier` must refuse it at build time rather than hand a receiver
//!   an unvalidatable consignment.
//!
//! Requires the regtest stack (bitcoind + electrs:50001 + RGB proxy:3000). It never touches the
//! Mercury server or the lockbox — no coin is deposited and nothing is co-signed.

use std::{collections::HashMap, fs, str::FromStr};

use anyhow::{anyhow, Result};
use electrum_client::bitcoin::{
    absolute,
    hashes::Hash as _,
    psbt::{Input as PsbtInput, Psbt, PsbtSighashType},
    secp256k1::{PublicKey, Secp256k1, SecretKey},
    Address, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use electrum_client::{Client, ElectrumApi};
use mercury_rgb::{consignment_witness_txids, RgbWallet};
use mercurylib::tesr;
use mercuryrustlib::rgb::{
    build_colored_tier, colored_committed_fee, colored_tier_out_total, ColoredTier,
    ColoredTierSpec, TierRole, TierSeal,
};

use crate::bitcoin_core;

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const ISSUED: u64 = 1000;
const FEE_RATE: f64 = 2.0;
const CSV_D: u16 = 36;
/// The retired global `TOKEN_BLINDING`. Used ONLY by part E, as the negative control.
const RETIRED_GLOBAL_BLINDING: u64 = 777;
/// Rival count. The gate makes ≥3 non-negotiable; 4 also gives part F a spare to grind against.
const RIVALS: u64 = 4;

// ---------------------------------------------------------------------------------- small helpers

/// A deterministic regtest P2TR address, keyed on `seed`. Deterministic so a failing run is
/// reproducible, and cheap so part F can grind thousands of candidates.
fn p2tr_address(seed: u64) -> Result<String> {
    let mut bytes = [1u8; 32];
    bytes[..8].copy_from_slice(&seed.to_be_bytes());
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&bytes)?;
    let (xonly, _) = PublicKey::from_secret_key(&secp, &sk).x_only_public_key();
    Ok(Address::p2tr(&secp, xonly, None, Network::Regtest).to_string())
}

fn spk_of(address: &str) -> Result<ScriptBuf> {
    Ok(Address::from_str(address)?.require_network(Network::Regtest)?.script_pubkey())
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

/// The **internal** (consensus, little-endian) byte order of a txid — the order rgb-lib's
/// `LargeOrdSet<Txid>` iterates in, and therefore the order that decides which rival wins the
/// witness lottery. NOT the displayed hex, which is byte-reversed.
fn internal_txid(txid: &str) -> Result<[u8; 32]> {
    Ok(Txid::from_str(txid)?.to_byte_array())
}

/// Rebuild, byte-for-byte, the transaction `build_colored_tier` builds — so a candidate's txid can
/// be known BEFORE it is coloured (part F grinds on it, part E colours it by hand).
fn tier_tx(
    prev_txid: &str,
    prev_vout: u32,
    sequence: u32,
    payloads: &[(String, u64, u64)],
) -> Result<Transaction> {
    let mut output = Vec::with_capacity(payloads.len() + 1);
    for (address, sats, _) in payloads {
        output.push(TxOut { value: *sats, script_pubkey: spk_of(address)? });
    }
    output.push(TxOut { value: tesr::P2A_VALUE, script_pubkey: tesr::p2a_script() });
    Ok(Transaction {
        version: 3,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            previous_output: OutPoint { txid: Txid::from_str(prev_txid)?, vout: prev_vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(sequence),
            witness: Witness::default(),
        }],
        output,
    })
}

/// Colour a tier the OLD way — one caller-supplied blinding, no build-time collision assert. This is
/// how the SDK coloured before commit 3, and it is what part E needs in order to *observe* the
/// collapse instead of being stopped by the new guard.
fn color_tier_unguarded(
    rgb: &RgbWallet,
    contract: &str,
    prev: (&str, u32, u64, &str),
    sequence: u32,
    payloads: &[(String, u64, u64)],
    blinding: u64,
) -> Result<(String, String, Vec<u32>)> {
    let (prev_txid, prev_vout, prev_value, prev_spk_hex) = prev;
    let tx = tier_tx(prev_txid, prev_vout, sequence, payloads)?;
    let mut psbt = Psbt::from_unsigned_tx(tx)?;
    let mut input = PsbtInput {
        witness_utxo: Some(TxOut {
            value: prev_value,
            script_pubkey: ScriptBuf::from(hex::decode(prev_spk_hex)?),
        }),
        ..Default::default()
    };
    input.sighash_type = Some(PsbtSighashType::from_str("SIGHASH_ALL")?);
    psbt.inputs = vec![input];

    let mut output_map: HashMap<u32, u64> = HashMap::new();
    for (i, (_, _, amount)) in payloads.iter().enumerate() {
        if *amount > 0 {
            output_map.insert(i as u32, *amount);
        }
    }
    let (colored_b64, consignment) = rgb.color(&psbt.to_string(), contract, output_map, blinding)?;
    let colored = Psbt::from_str(&colored_b64)?.unsigned_tx;
    let payload_vouts: Vec<u32> = colored
        .output
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            !o.script_pubkey.is_op_return() && o.script_pubkey.as_bytes() != tesr::P2A_SCRIPT_BYTES
        })
        .map(|(v, _)| v as u32)
        .collect();
    Ok((colored.txid().to_string(), consignment, payload_vouts))
}

/// Fresh RGB wallet + a fresh NIA issuance, returning the wallet, the contract id and the
/// (confirmed, on-chain) outpoint holding the whole issuance.
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
    let contract = rgb.issue_nia(ticker, "CTES-R tier builder", 0, vec![ISSUED])?;
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

/// Every assertion that must hold of ANY coloured tier, regardless of arity.
fn assert_tier_invariants(label: &str, tier: &ColoredTier, prev_value: u64, n_payload: usize) {
    // (§3.4) exactly the coloured fee, and therefore exactly the target rate on the real vsize.
    assert_eq!(
        tier.committed_fee,
        colored_committed_fee(n_payload, FEE_RATE),
        "{label}: committed fee must be committed_fee_for_outputs(n_payload + 1, rate)"
    );
    assert_eq!(
        tier.committed_fee,
        tier.signed_vsize * 2,
        "{label}: a coloured tier must pay EXACTLY {FEE_RATE} sat/vB — {} sat over {} vB",
        tier.committed_fee,
        tier.signed_vsize
    );
    assert!(
        tier.committed_fee > tesr::committed_fee_for_outputs(n_payload, FEE_RATE),
        "{label}: the coloured fee must exceed the uncoloured one, or the tier cannot relay standalone"
    );
    assert_eq!(
        tier.committed_fee,
        tesr::committed_fee_for_outputs(n_payload, FEE_RATE) + tesr::P2TR_OUT_VBYTES * 2,
        "{label}: the coloured/uncoloured gap must be the constant 43 vB * rate, independent of n"
    );

    // Explicit payload vouts: as many as asked for, all distinct, and never the opret or the anchor.
    assert_eq!(tier.payloads.len(), n_payload, "{label}: payload count");
    let vouts = tier.payload_vouts();
    assert!(
        vouts.iter().all(|v| *v != tier.opret_vout && *v != tier.p2a_vout),
        "{label}: a payload vout collided with the opret ({}) or the anchor ({}): {vouts:?}",
        tier.opret_vout,
        tier.p2a_vout
    );
    assert!(vouts.windows(2).all(|w| w[0] < w[1]), "{label}: payload vouts must be ascending");
    assert_ne!(tier.opret_vout, tier.p2a_vout, "{label}: opret and anchor cannot be one output");

    // Value conservation against the parent.
    let paid: u64 = tier.payloads.iter().map(|p| p.value).sum();
    assert_eq!(
        paid + tesr::P2A_VALUE + tier.committed_fee,
        prev_value,
        "{label}: payloads + anchor + fee must equal the parent's value exactly"
    );
    assert_eq!(
        paid,
        colored_tier_out_total(prev_value, n_payload, FEE_RATE).unwrap(),
        "{label}: payload total must be the coloured budget"
    );

    // The build-time collision assert already ran inside the builder; re-state it here so a failure
    // in this test is unambiguous about WHICH property broke.
    let witnesses = consignment_witness_txids(&tier.consignment).unwrap();
    assert!(
        witnesses.contains(&tier.txid),
        "{label}: the tier's own witness {} is missing from its consignment {witnesses:?}",
        tier.txid
    );
    assert_ne!(tier.blinding, RETIRED_GLOBAL_BLINDING, "{label}: the global constant is retired");
}

// ------------------------------------------------------------------------------------- the parts

/// A / C — the ordinary one-payload tier.
fn part_a(rgb: &RgbWallet, contract: &str, prev: (&str, u32, u64, &str)) -> Result<()> {
    println!("\n=== RGB15 part A: a 1-payload coloured tier ===");
    let (prev_txid, prev_vout, prev_value, prev_spk_hex) = prev;
    let out_value = colored_tier_out_total(prev_value, 1, FEE_RATE)
        .ok_or_else(|| anyhow!("parent too small for a coloured tier"))?;
    let payloads = vec![(p2tr_address(1)?, out_value, ISSUED)];
    let seal = TierSeal::new("rgb15-coin-a", TierRole::State, 0, 0);
    let tier = tokio::task::block_in_place(|| {
        build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: contract,
                prev_txid,
                prev_vout,
                prev_value,
                prev_spk_hex,
                sequence: tesr::csv_blocks(CSV_D).0,
                payloads: &payloads,
                network: NETWORK,
                fee_rate: FEE_RATE,
                nonce: None,
            },
            &seal,
        )
    })?;
    println!(
        "  txid={} payload vouts={:?} opret={} p2a={} fee={} over {} vB = {:.3} sat/vB",
        tier.txid,
        tier.payload_vouts(),
        tier.opret_vout,
        tier.p2a_vout,
        tier.committed_fee,
        tier.signed_vsize,
        tier.committed_fee as f64 / tier.signed_vsize as f64
    );
    assert_tier_invariants("A", &tier, prev_value, 1);
    assert_eq!(tier.blinding, seal.blinding(), "A: the builder must use the derived blinding");

    // The measured numbers from CTESR-GATE §2.1(c), pinned so a rgb-lib change that alters the
    // opret's size (and therefore the fee arithmetic) fails HERE and not in production.
    assert_eq!(tier.signed_vsize, 167, "A: coloured 1-payload tier vsize");
    assert_eq!(tier.committed_fee, 334, "A: (TIER_VBYTES 124 + 43) * 2");

    // The opret lands FIRST (the fork sets `opreturn_first` whenever any output is P2TR), so the
    // payload shifts from 0 to 1 and the anchor from 1 to 2. Asserted as an observation, never
    // assumed by the builder.
    assert_eq!(tier.opret_vout, 0, "A: the opret is inserted first when a P2TR output is present");
    assert_eq!(tier.payload_vouts(), vec![1], "A: the payload shifted 0 -> 1");
    assert_eq!(tier.p2a_vout, 2, "A: the anchor shifted 1 -> 2");

    // Un-broadcast: the indexer must not know this txid.
    let client = Client::new(ELECTRUM_URL)?;
    assert!(
        client.transaction_get_raw(&Txid::from_str(&tier.txid)?).is_err(),
        "A: the tier must NOT be broadcast"
    );

    let (valid, detail) =
        tokio::task::block_in_place(|| rgb.validate_offchain(&tier.consignment, &tier.txid))?;
    println!("  validate_offchain -> valid={valid} detail={detail:?}");
    assert!(valid, "A: the consignment must validate off-chain against the un-broadcast tier");
    Ok(())
}

/// B / C — the N-payload split-state shape, plus the `create_colored_split_tx` trap.
fn part_b(rgb: &RgbWallet, contract: &str, prev: (&str, u32, u64, &str)) -> Result<()> {
    println!("\n=== RGB15 part B: a 3-payload coloured split state ===");
    const N: usize = 3;
    let (prev_txid, prev_vout, prev_value, prev_spk_hex) = prev;
    let total = colored_tier_out_total(prev_value, N, FEE_RATE)
        .ok_or_else(|| anyhow!("parent too small for a coloured split state"))?;
    let each = total / N as u64;
    let share = ISSUED / N as u64;
    let mut payloads = Vec::with_capacity(N);
    for i in 0..N {
        let sats = if i == 0 { total - each * (N as u64 - 1) } else { each };
        let amount = if i == 0 { ISSUED - share * (N as u64 - 1) } else { share };
        payloads.push((p2tr_address(100 + i as u64)?, sats, amount));
    }
    let seal = TierSeal::new("rgb15-coin-b", TierRole::SplitState, 0, N as u32);
    let tier = tokio::task::block_in_place(|| {
        build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: contract,
                prev_txid,
                prev_vout,
                prev_value,
                prev_spk_hex,
                sequence: tesr::csv_blocks(CSV_D).0,
                payloads: &payloads,
                network: NETWORK,
                fee_rate: FEE_RATE,
                nonce: None,
            },
            &seal,
        )
    })?;
    println!(
        "  txid={} payload vouts={:?} opret={} p2a={} fee={} over {} vB = {:.3} sat/vB",
        tier.txid,
        tier.payload_vouts(),
        tier.opret_vout,
        tier.p2a_vout,
        tier.committed_fee,
        tier.signed_vsize,
        tier.committed_fee as f64 / tier.signed_vsize as f64
    );
    assert_tier_invariants("B", &tier, prev_value, N);
    assert_eq!(tier.signed_vsize, 253, "B: coloured 3-payload split-state vsize");
    assert_eq!(tier.committed_fee, 506, "B: (124 + 3*43) * 2");
    assert_eq!(tier.opret_vout, 0, "B: opret first");
    assert_eq!(tier.payload_vouts(), vec![1, 2, 3], "B: payloads shifted j -> j+1");
    assert_eq!(tier.p2a_vout, 4, "B: anchor shifted 3 -> 4");
    for (i, (address, sats, _)) in payloads.iter().enumerate() {
        assert_eq!(tier.payloads[i].value, *sats, "B: payload {i} value");
        assert_eq!(
            tier.payloads[i].script_pubkey_hex,
            hex::encode(spk_of(address)?.as_bytes()),
            "B: payload {i} must keep its own payee and its own position"
        );
    }

    let (valid, detail) =
        tokio::task::block_in_place(|| rgb.validate_offchain(&tier.consignment, &tier.txid))?;
    println!("  validate_offchain -> valid={valid} detail={detail:?}");
    assert!(valid, "B: the split-state consignment must validate off-chain");

    // --- part C: why create_colored_split_tx cannot be reused (CTESR-GATE §3.5) ---
    println!("\n=== RGB15 part C: the `!is_op_return` filter counts the P2A anchor ===");
    let colored: Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&tier.tx_hex)?)?;
    let naive: Vec<u32> = colored
        .output
        .iter()
        .enumerate()
        .filter(|(_, o)| !o.script_pubkey.is_op_return())
        .map(|(v, _)| v as u32)
        .collect();
    println!("  naive `!is_op_return` -> {naive:?} (len {}), splits.len() would be {N}", naive.len());
    assert_eq!(naive.len(), N + 1, "C: the anchor is counted as a payload by the naive filter");
    assert_ne!(
        naive.len(),
        N,
        "C: `create_colored_split_tx`'s `output_vouts.len() != splits.len()` guard MUST trip"
    );
    assert_eq!(tier.payload_vouts(), naive[..N].to_vec(), "C: the builder drops exactly the anchor");
    assert!(!tesr::p2a_script().is_op_return(), "C: the P2A script is not an OP_RETURN");
    Ok(())
}

/// D — the decisive ≥3-rival test, with a deliberately non-minimum-internal-txid selection.
fn part_d(
    rgb: &RgbWallet,
    contract: &str,
    prev: (&str, u32, u64, &str),
) -> Result<(ColoredTier, Vec<ColoredTier>)> {
    println!("\n=== RGB15 part D: {RIVALS} rivals, per-tier blinding, non-minimum selection ===");
    let (prev_txid, prev_vout, prev_value, prev_spk_hex) = prev;

    // T over the confirmed funding output.
    let t_value = colored_tier_out_total(prev_value, 1, FEE_RATE).unwrap();
    let t_payloads = vec![(p2tr_address(200)?, t_value, ISSUED)];
    let trigger = tokio::task::block_in_place(|| {
        build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: contract,
                prev_txid,
                prev_vout,
                prev_value,
                prev_spk_hex,
                sequence: tesr::TRIGGER_SEQUENCE.0,
                payloads: &t_payloads,
                network: NETWORK,
                fee_rate: FEE_RATE,
                nonce: None,
            },
            &TierSeal::new("rgb15-coin-d", TierRole::Trigger, 0, 0),
        )
    })?;
    assert_tier_invariants("D/T", &trigger, prev_value, 1);
    let t_payload = trigger.payloads[0].clone();
    println!("  T={} payload at vout {} ({} sat)", trigger.txid, t_payload.vout, t_payload.value);

    // RIVALS extensions over T's payload output. Same parent, same amounts, same arity — different
    // ONLY in their payee (hence txid) and in their derived seal.
    let rival_value = colored_tier_out_total(t_payload.value, 1, FEE_RATE).unwrap();
    let mut rivals = Vec::new();
    for m in 0..RIVALS {
        let payloads = vec![(p2tr_address(300 + m)?, rival_value, ISSUED)];
        let seal = TierSeal::new("rgb15-coin-d", TierRole::Extension, m as u32, 0);
        let tier = tokio::task::block_in_place(|| {
            build_colored_tier(
                rgb,
                &ColoredTierSpec {
                    contract_id: contract,
                    prev_txid: &trigger.txid,
                    prev_vout: t_payload.vout,
                    prev_value: t_payload.value,
                    prev_spk_hex: &t_payload.script_pubkey_hex,
                    sequence: tesr::csv_blocks(CSV_D).0,
                    payloads: &payloads,
                    network: NETWORK,
                    fee_rate: FEE_RATE,
                    nonce: None,
                },
                &seal,
            )
        })
        .map_err(|e| anyhow!("D: rival X_{m} failed to build with its own seal: {e}"))?;
        assert_tier_invariants(&format!("D/X_{m}"), &tier, t_payload.value, 1);
        println!("  X_{m} txid={} blinding={}", tier.txid, tier.blinding);
        rivals.push(tier);
    }

    // Every rival must have its OWN identity: distinct blindings, and each consignment carrying its
    // own witness (already asserted above, restated as the property under test).
    let mut blindings: Vec<u64> = rivals.iter().map(|r| r.blinding).collect();
    blindings.sort_unstable();
    blindings.dedup();
    assert_eq!(blindings.len(), RIVALS as usize, "D: rivals must not share a seal blinding");

    // Choose the rival with the MAXIMUM internal txid — guaranteed not to be the minimum, which is
    // the one rgb-lib would keep under collapse. This is the whole point of the test.
    let mut ordered: Vec<(usize, [u8; 32])> = Vec::new();
    for (i, r) in rivals.iter().enumerate() {
        ordered.push((i, internal_txid(&r.txid)?));
    }
    ordered.sort_by(|a, b| a.1.cmp(&b.1));
    let min_index = ordered.first().unwrap().0;
    let chosen_index = ordered.last().unwrap().0;
    assert_ne!(
        chosen_index, min_index,
        "D: the chosen rival MUST NOT be the internal-txid minimum (need >= 2 distinct rivals)"
    );
    let chosen = &rivals[chosen_index];
    let minimum = &rivals[min_index];
    println!(
        "  chosen (max internal txid) = X_{chosen_index} {}\n  minimum internal txid      = X_{min_index} {}",
        chosen.txid, minimum.txid
    );

    // S over the CHOSEN rival.
    let s_value = colored_tier_out_total(chosen.payloads[0].value, 1, FEE_RATE).unwrap();
    let s_payloads = vec![(p2tr_address(400)?, s_value, ISSUED)];
    let state = tokio::task::block_in_place(|| {
        build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: contract,
                prev_txid: &chosen.txid,
                prev_vout: chosen.payloads[0].vout,
                prev_value: chosen.payloads[0].value,
                prev_spk_hex: &chosen.payloads[0].script_pubkey_hex,
                sequence: tesr::csv_blocks(CSV_D).0,
                payloads: &s_payloads,
                network: NETWORK,
                fee_rate: FEE_RATE,
                nonce: None,
            },
            &TierSeal::new("rgb15-coin-d", TierRole::State, 0, 0),
        )
    })?;
    assert_tier_invariants("D/S", &state, chosen.payloads[0].value, 1);

    let witnesses = consignment_witness_txids(&state.consignment)?;
    println!("  S={}\n  S consignment witnesses = {witnesses:?}", state.txid);
    assert!(
        witnesses.contains(&chosen.txid),
        "D: THE decisive assertion — S must embed the rival it was actually built over ({}), \
         not whichever rival won an internal-txid lottery. Witnesses: {witnesses:?}",
        chosen.txid
    );
    for (i, rival) in rivals.iter().enumerate() {
        if i != chosen_index {
            assert!(
                !witnesses.contains(&rival.txid),
                "D: S must not embed the unrelated rival X_{i} ({})",
                rival.txid
            );
        }
    }
    assert!(witnesses.contains(&trigger.txid), "D: S must embed its grandparent T");

    // And the receiver's own check: the TRUE branch validates off-chain.
    let branch = vec![trigger.txid.clone(), chosen.txid.clone(), state.txid.clone()];
    let (valid, detail) =
        tokio::task::block_in_place(|| rgb.validate_offchain_chain(&state.consignment, &branch))?;
    println!("  validate_offchain_chain([T, X_{chosen_index}, S]) -> valid={valid} detail={detail:?}");
    assert!(valid, "D: the true branch must validate off-chain: {detail:?}");

    Ok((trigger, rivals))
}

/// E — the negative control, with the retired global constant. The collapse must reproduce.
///
/// **Why this is deterministic, and why it was not** (D9). The consignment a `color` call returns is
/// a SNAPSHOT of the stock taken at that instant, so rival `X_m`'s freshly-returned consignment
/// resolves the shared `BundleId` over only `{X_0 … X_m}` — the rivals that existed when it was
/// taken. `X_0` therefore always carries its own witness, whatever its txid, and asserting the
/// collapse on the maximum-internal-txid rival's *fresh* consignment reproduced it only when that
/// rival was not `X_0`: a 1-in-`RIVALS` coin flip. Measured: 2 of 4 runs panicked at exactly that
/// assertion, both times with `max_index == 0`.
///
/// The fix is to take the snapshot LAST. Once all `RIVALS` rivals are in the stock, the
/// maximum-internal-txid rival is **re-coloured** — byte-identical spec, therefore byte-identical
/// transaction and txid (asserted below) — and the consignment that comes back is the one under
/// test. It now resolves the shared `BundleId` over the COMPLETE rival set, which rgb-lib does by
/// keeping the numerically smallest internal (little-endian) txid, and `max ≠ min` holds by
/// construction for `RIVALS >= 2`. Nothing about the pass condition is relaxed: the collapse must
/// still reproduce, the witness must still be the minimum rival's, and the true branch must still
/// fail to validate.
fn part_e(rgb: &RgbWallet, contract: &str, prev: (&str, u32, u64, &str)) -> Result<()> {
    println!("\n=== RGB15 part E (control): the same shape at the retired global blinding 777 ===");
    let (prev_txid, prev_vout, prev_value, prev_spk_hex) = prev;

    let t_value = colored_tier_out_total(prev_value, 1, FEE_RATE).unwrap();
    let t_payloads = vec![(p2tr_address(500)?, t_value, ISSUED)];
    let (t_txid, _t_consignment, t_vouts) = tokio::task::block_in_place(|| {
        color_tier_unguarded(
            rgb,
            contract,
            (prev_txid, prev_vout, prev_value, prev_spk_hex),
            tesr::TRIGGER_SEQUENCE.0,
            &t_payloads,
            RETIRED_GLOBAL_BLINDING,
        )
    })?;
    let t_payload_vout = t_vouts[0];
    println!("  T={t_txid} payload at vout {t_payload_vout}");
    let t_payload_spk = hex::encode(spk_of(&t_payloads[0].0)?.as_bytes());

    let rival_value = colored_tier_out_total(t_value, 1, FEE_RATE).unwrap();
    // (payee address, txid, payload vout). The consignment each colouring returns is deliberately
    // DROPPED: it is a stock snapshot from before the later rivals existed, and reading the collapse
    // off it is precisely the order dependence this part used to have.
    let mut rivals: Vec<(String, String, u32)> = Vec::new();
    for m in 0..RIVALS {
        let payee = p2tr_address(600 + m)?;
        let payloads = vec![(payee.clone(), rival_value, ISSUED)];
        let (txid, _snapshot, vouts) = tokio::task::block_in_place(|| {
            color_tier_unguarded(
                rgb,
                contract,
                (&t_txid, t_payload_vout, t_value, &t_payload_spk),
                tesr::csv_blocks(CSV_D).0,
                &payloads,
                RETIRED_GLOBAL_BLINDING,
            )
        })?;
        println!("  X_{m} txid={txid}");
        rivals.push((payee, txid, vouts[0]));
    }

    let mut ordered: Vec<(usize, [u8; 32])> = Vec::new();
    for (i, (_, txid, _)) in rivals.iter().enumerate() {
        ordered.push((i, internal_txid(txid)?));
    }
    ordered.sort_by(|a, b| a.1.cmp(&b.1));
    let min_index = ordered.first().unwrap().0;
    let max_index = ordered.last().unwrap().0;
    assert_ne!(min_index, max_index, "E: need distinct rivals");
    let (max_payee, max_txid, max_payload_vout) = rivals[max_index].clone();
    let min_txid = rivals[min_index].1.clone();
    println!("  max internal txid = X_{max_index} {max_txid}\n  min internal txid = X_{min_index} {min_txid}");

    // THE COLLAPSE, deterministically. Re-colour the max-txid rival now that all RIVALS rivals are
    // in the stock; the consignment that comes back is a snapshot over the COMPLETE rival set, so
    // which witness it resolves the shared BundleId to no longer depends on the order the rivals
    // were built in. The transaction is rebuilt from the identical spec, so its txid must be
    // unchanged — asserted, because the whole reading below rests on it.
    let max_payloads = vec![(max_payee.clone(), rival_value, ISSUED)];
    let (re_txid, max_consignment, re_vouts) = tokio::task::block_in_place(|| {
        color_tier_unguarded(
            rgb,
            contract,
            (&t_txid, t_payload_vout, t_value, &t_payload_spk),
            tesr::csv_blocks(CSV_D).0,
            &max_payloads,
            RETIRED_GLOBAL_BLINDING,
        )
    })?;
    assert_eq!(
        re_txid, max_txid,
        "E: re-colouring X_{max_index} from the identical spec must reproduce the identical \
         transaction — the snapshot below would otherwise be about a different tier"
    );
    assert_eq!(re_vouts[0], max_payload_vout, "E: the re-coloured payload moved vout");
    let witnesses = consignment_witness_txids(&max_consignment)?;
    println!("  X_{max_index} consignment witnesses (taken after all {RIVALS} rivals) = {witnesses:?}");
    assert!(
        !witnesses.contains(&max_txid),
        "E: the control did NOT reproduce the collapse — X_{max_index} carried its own witness at \
         the shared blinding {RETIRED_GLOBAL_BLINDING}. Part D would then prove nothing."
    );
    assert!(
        witnesses.contains(&min_txid),
        "E: the collapsed bundle must resolve to the minimum internal txid ({min_txid}); got {witnesses:?}"
    );

    // And the consequence: a state over the max rival cannot be validated on its true branch.
    let s_value = colored_tier_out_total(rival_value, 1, FEE_RATE).unwrap();
    let s_payloads = vec![(p2tr_address(700)?, s_value, ISSUED)];
    let max_payload_spk = hex::encode(spk_of(&max_payee)?.as_bytes());
    let (s_txid, s_consignment, _) = tokio::task::block_in_place(|| {
        color_tier_unguarded(
            rgb,
            contract,
            (&max_txid, max_payload_vout, rival_value, &max_payload_spk),
            tesr::csv_blocks(CSV_D).0,
            &s_payloads,
            RETIRED_GLOBAL_BLINDING,
        )
    })?;
    let s_witnesses = consignment_witness_txids(&s_consignment)?;
    println!("  S={s_txid}\n  S consignment witnesses = {s_witnesses:?}");
    assert!(
        !s_witnesses.contains(&max_txid),
        "E: S was built over X_{max_index} but must embed the lottery winner instead"
    );
    assert!(
        s_witnesses.contains(&min_txid),
        "E: S's ancestor bundle must have collapsed onto the minimum rival ({min_txid}); got \
         {s_witnesses:?}"
    );
    let branch = vec![t_txid.clone(), max_txid.clone(), s_txid.clone()];
    let (valid, detail) =
        tokio::task::block_in_place(|| rgb.validate_offchain_chain(&s_consignment, &branch))?;
    println!("  validate_offchain_chain([T, X_{max_index}, S]) -> valid={valid} detail={detail:?}");
    assert!(
        !valid,
        "E: the true branch must NOT validate under the collapse — that is the bug being fixed"
    );
    Ok(())
}

/// F — the build-time collision assert refuses a rival that reuses another tier's seal.
fn part_f(
    rgb: &RgbWallet,
    contract: &str,
    trigger: &ColoredTier,
    rivals: &[ColoredTier],
) -> Result<()> {
    println!("\n=== RGB15 part F: the build-time collision assert ===");
    let t_payload = &trigger.payloads[0];
    let rival_value = colored_tier_out_total(t_payload.value, 1, FEE_RATE).unwrap();

    // Reuse the seal of the rival with the minimum internal txid. Every candidate below then
    // collides with it over the same parent output, and rgb-lib keeps whichever of the colliding
    // set has the smallest internal txid.
    //
    // Why this is a bounded LOOP and not a single shot: the lottery runs on the **coloured** txid,
    // which nobody can know before colouring (the opret's script commits an MPC root computed
    // inside rgb-lib). So a candidate is only refused when it loses, which is a coin flip on the
    // first try. Candidate `i` survives only by being the running minimum of `{victim, c_1..c_i}`,
    // so the chance that ALL of `MAX_CANDIDATES` survive is `1 / (MAX_CANDIDATES + 1)!` — at 10
    // that is ~2.5e-8, i.e. the assertion below is unconditional in practice. Each candidate that
    // *does* survive is itself proof the guard is sound (its own witness really is present).
    const MAX_CANDIDATES: u64 = 10;
    let mut ordered: Vec<(usize, [u8; 32])> = Vec::new();
    for (i, r) in rivals.iter().enumerate() {
        ordered.push((i, internal_txid(&r.txid)?));
    }
    ordered.sort_by(|a, b| a.1.cmp(&b.1));
    let victim_index = ordered.first().unwrap().0;
    let victim_seal = TierSeal::new("rgb15-coin-d", TierRole::Extension, victim_index as u32, 0);
    println!(
        "  reusing the seal of X_{victim_index} ({}) — blinding {}",
        rivals[victim_index].txid,
        victim_seal.blinding()
    );

    let mut refused = None;
    let mut last_payloads = None;
    for candidate in 0..MAX_CANDIDATES {
        let payloads = vec![(p2tr_address(900_000 + candidate)?, rival_value, ISSUED)];
        let result = tokio::task::block_in_place(|| {
            build_colored_tier(
                rgb,
                &ColoredTierSpec {
                    contract_id: contract,
                    prev_txid: &trigger.txid,
                    prev_vout: t_payload.vout,
                    prev_value: t_payload.value,
                    prev_spk_hex: &t_payload.script_pubkey_hex,
                    sequence: tesr::csv_blocks(CSV_D).0,
                    payloads: &payloads,
                    network: NETWORK,
                    fee_rate: FEE_RATE,
                    nonce: None,
                },
                // The SAME seal as an existing rival over the SAME parent output.
                &victim_seal,
            )
        });
        last_payloads = Some(payloads);
        match result {
            Err(e) => {
                refused = Some(format!("{e}"));
                println!("  candidate {candidate} refused, as required");
                break;
            }
            Ok(survivor) => {
                // It won the lottery this time — so it IS the new minimum, and its consignment must
                // genuinely carry its own witness. The guard must never fire on a sound tier.
                let witnesses = consignment_witness_txids(&survivor.consignment)?;
                assert!(
                    witnesses.contains(&survivor.txid),
                    "F: candidate {candidate} was accepted without its own witness — the guard is broken"
                );
                println!(
                    "  candidate {candidate} won the internal-txid lottery ({}); retrying",
                    survivor.txid
                );
            }
        }
    }

    let message = refused.ok_or_else(|| {
        anyhow!(
            "F: {MAX_CANDIDATES} colliding candidates in a row all won the internal-txid lottery \
             (p ~ 2.5e-8) — far more likely, the build-time collision assert never fires"
        )
    })?;
    println!("  {message}");
    assert!(message.contains("collided"), "F: the rejection must name the collision: {message}");
    assert!(
        message.contains("OWN witness is absent"),
        "F: the rejection must say the tier's own witness is missing: {message}"
    );
    assert!(
        message.contains(&format!("{}", victim_seal.blinding())),
        "F: the rejection must name the colliding blinding: {message}"
    );

    // Control: the SAME transaction with its OWN (fresh) seal builds fine — so part F is about the
    // seal, not about the transaction.
    let payloads = last_payloads.ok_or_else(|| anyhow!("F: no candidate was built"))?;
    let fresh = tokio::task::block_in_place(|| {
        build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: contract,
                prev_txid: &trigger.txid,
                prev_vout: t_payload.vout,
                prev_value: t_payload.value,
                prev_spk_hex: &t_payload.script_pubkey_hex,
                sequence: tesr::csv_blocks(CSV_D).0,
                payloads: &payloads,
                network: NETWORK,
                fee_rate: FEE_RATE,
                nonce: None,
            },
            &TierSeal::new("rgb15-coin-d", TierRole::Extension, 4_000, 0),
        )
    })?;
    assert_tier_invariants("F/control", &fresh, t_payload.value, 1);
    println!("  control: the same tx with a fresh seal builds fine ({})", fresh.txid);
    Ok(())
}

pub async fn execute() -> Result<()> {
    std::env::set_var("ML_NETWORK", NETWORK);
    let _ = fs::remove_dir_all("./rgb-data15");
    println!("RGB15 — CTES-R coloured tier builder + per-tier seal blinding");

    let (rgb_a, contract_a, txid_a, vout_a, value_a, spk_a) =
        tokio::task::block_in_place(|| setup("./rgb-data15/a", "R15A"))?;
    part_a(&rgb_a, &contract_a, (&txid_a, vout_a, value_a, &spk_a))?;

    let (rgb_b, contract_b, txid_b, vout_b, value_b, spk_b) =
        tokio::task::block_in_place(|| setup("./rgb-data15/b", "R15B"))?;
    part_b(&rgb_b, &contract_b, (&txid_b, vout_b, value_b, &spk_b))?;

    let (rgb_d, contract_d, txid_d, vout_d, value_d, spk_d) =
        tokio::task::block_in_place(|| setup("./rgb-data15/d", "R15D"))?;
    let (trigger, rivals) = part_d(&rgb_d, &contract_d, (&txid_d, vout_d, value_d, &spk_d))?;

    let (rgb_e, contract_e, txid_e, vout_e, value_e, spk_e) =
        tokio::task::block_in_place(|| setup("./rgb-data15/e", "R15E"))?;
    part_e(&rgb_e, &contract_e, (&txid_e, vout_e, value_e, &spk_e))?;

    part_f(&rgb_d, &contract_d, &trigger, &rivals)?;

    println!("\nRGB15 — OK");
    Ok(())
}
