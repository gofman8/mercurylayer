//! **RGB_E2E=16 — [D3] why rgb-lib refuses to colour a LEGACY-lane carrier.**
//!
//! sdk78 measured a `TOKEN_PIECE_SATS` piece — comfortably above the coloured ROOT floor, its
//! allocation booked, its funding on chain — that rgb-lib nonetheless refused to colour, with
//! `Invalid coloring info` over the piece's own funding output, stably across 40 claim passes. That
//! refusal is why the migration hatch had to key on EVIDENCE (a read-only `color_psbt` probe)
//! rather than on the carrier's size. This test reproduces it deterministically, in ~30 seconds,
//! with no Mercury server and no SE, and names the cause.
//!
//! # The answer, stated up front
//!
//! **It is NOT the E7 class, and no guard has a gap.** E7 is *"accepted into the stock, then
//! archived"* — a deliberately un-broadcast witness resolved with the plain blockchain resolver,
//! `WitnessStatus::Unresolved` mapping to `WitnessOrd::Archived`, and the allocation dropped by
//! `OutputAssignment::check_witness`. The legacy lane never gets that far. It **never accepts the
//! transfer into the stock at all.**
//!
//! The legacy receiver (`mercury_utexo_sdk::tokens`, `accept_incoming_tokens`) books a piece with
//! exactly two RGB calls:
//!
//! ```text
//! w.import_asset_offchain(&env.c, &txids)   // rgb-lib `save_new_asset` -> import_contract ONLY
//! w.register_statechain(txid, vout, sats, contract, booked, &[])   // a SQLITE row
//! ```
//!
//! plus `accept_offchain_amount` to derive the amount, whose own rgb-lib doc ends *"nothing is
//! persisted"*. `save_new_asset` imports the **genesis**; it never calls `accept_transfer`. So the
//! receiver's stock ends up with:
//!
//! * **no witness ord** for the split/combine txid — and `check_witness` fails on `None`, not just
//!   on `Archived`;
//! * **no revealed seal** at the piece's funding outpoint.
//!
//! `color_psbt` reads the allocation through `Stock::contract_assignments_for` →
//! `ContractStateRead::fungible_all()`, both filters applied, gets nothing, computes
//! `asset_available_amt = 0`, and answers
//! `InvalidColoringInfo { "total amount in output_map (N) greater than available (0)" }` — a
//! message that mentions neither filter. Meanwhile `get_asset_balance` is computed from sqlite and
//! reports the full amount, so the coin looks healthy from every angle except the one that matters.
//!
//! The COLOURED lane differs by exactly one call — `accept_ladder` (rgb-lib
//! `accept_offchain_ladder`), which reveals every tier seal via `store_secret_seal` and runs
//! `accept_transfer` under an `OffchainResolver` covering the whole un-broadcast chain. That single
//! call is the entire difference between a colourable carrier and an un-colourable one.
//!
//! # The migration fact the product owner needs
//!
//! **Every legacy-lane piece is un-colourable in its receiver's wallet — permanently, by
//! construction.** Not because of its size, not because of a race, and not repairably by waiting:
//! there is nothing in that wallet's stock to build a coloured ladder from, and no number of
//! `claim()` passes creates one. Size is simply the wrong question for the whole class, which is
//! why a size-keyed hatch would strand exactly the coins this test builds.
//!
//! It is repairable in principle — the receiver would have to accept the legacy consignment into
//! the stock (part D proves the same bytes do work through `accept_ladder`) — but that is a change
//! to the SDK's legacy receive path, not to rgb-lib, and it is NOT made here.
//!
//! # What each part pins
//!
//! * **A** — the setup is honest: a legacy-lane piece exists, above the coloured ROOT floor, whose
//!   consignment genuinely assigns it the amount (`accept_offchain_amount` agrees).
//! * **B** — THE REPRODUCTION. After the legacy booking, the sqlite balance says the piece holds
//!   the amount and `color_psbt` says `available (0)`. Both, at the same instant, asserted on the
//!   exact rgb-lib message.
//! * **C** — THE CAUSE, named: `diagnose_allocation` reports `WitnessUnknownToStock`, witness ord
//!   `None`, and **zero** invalid bundles. Zero is the load-bearing number: it rules out the E7
//!   class, which invalidates bundles recursively. Asserted as `!= WitnessArchived` explicitly.
//! * **D** — THE CONTROL, and it is what makes C a diagnosis rather than a guess. The SAME
//!   consignment, the SAME outpoint, the SAME wallet — accepted through `accept_ladder` instead —
//!   flips the verdict to `Spendable`, the witness ord to `Tentative`, and `color_psbt` to Ok. So
//!   nothing about the piece, its size, or its bytes is at fault: only the receive path is.
//! * **E** — the probe is discriminating: after D it refuses `amount + 1`. A probe that says yes to
//!   everything proves nothing.
//!
//! Requires the regtest stack (bitcoind + electrs:50001 + RGB proxy:3000). No Mercury server, no
//! lockbox, nothing co-signed, nothing broadcast.
//!
//! Run: RGB_E2E=16 cargo run

use std::{collections::HashMap, fs, str::FromStr};

use anyhow::{anyhow, Result};
use electrum_client::bitcoin::{
    absolute,
    psbt::{Input as PsbtInput, Psbt, PsbtSighashType},
    secp256k1::{PublicKey, Secp256k1, SecretKey},
    Address, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use electrum_client::{Client, ElectrumApi};
use mercury_rgb::RgbWallet;

use crate::bitcoin_core;

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";

const ISSUED: u64 = 1000;
/// The amount the legacy split pays the receiver.
const PAID: u64 = 250;
/// Sats on the piece. Deliberately `TOKEN_PIECE_SATS` — ABOVE the coloured root floor, so that
/// "too small to ladder" is excluded by construction and the refusal cannot be blamed on size.
const PIECE_SATS: u64 = mercury_utexo_sdk::tokens::TOKEN_PIECE_SATS;
/// The sender's blinding for the legacy split. Conveyed to the receiver, as the real lane does.
const LEGACY_BLINDING: u64 = 0xD3_D3_D3_D3;

/// A deterministic regtest P2TR address, keyed on `seed`.
fn p2tr_address(seed: u64) -> Result<String> {
    let mut bytes = [7u8; 32];
    bytes[..8].copy_from_slice(&seed.to_be_bytes());
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&bytes)?;
    let (xonly, _) = PublicKey::from_secret_key(&secp, &sk).x_only_public_key();
    Ok(Address::p2tr(&secp, xonly, None, Network::Regtest).to_string())
}

fn spk_of(address: &str) -> Result<ScriptBuf> {
    Ok(Address::from_str(address)?.require_network(Network::Regtest)?.script_pubkey())
}

/// scriptPubKey + value of a CONFIRMED outpoint, read from the indexer.
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
    let contract = rgb.issue_nia(ticker, "D3 legacy-lane repro", 0, vec![ISSUED])?;
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

/// A read-only `color_psbt` stock probe of `amount` out of `(txid, vout)` — the same call
/// `mercuryrustlib::rgb::probe_allocation` makes, built here so the test depends on rgb-lib's
/// answer rather than on an SDK wrapper. Never `get_asset_balance` (E7).
fn probe(
    rgb: &RgbWallet,
    contract: &str,
    txid: &str,
    vout: u32,
    value: u64,
    spk_hex: &str,
    amount: u64,
) -> Result<()> {
    let tx = Transaction {
        version: 2,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            previous_output: OutPoint { txid: Txid::from_str(txid)?, vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xFFFF_FFFD),
            witness: Witness::default(),
        }],
        output: vec![TxOut { value, script_pubkey: spk_of(&p2tr_address(99)?)? }],
    };
    let mut psbt = Psbt::from_unsigned_tx(tx)?;
    psbt.inputs = vec![PsbtInput {
        witness_utxo: Some(TxOut {
            value,
            script_pubkey: ScriptBuf::from(hex::decode(spk_hex)?),
        }),
        ..Default::default()
    }];
    let mut output_map = HashMap::new();
    output_map.insert(0u32, amount);
    rgb.probe_spendable(&psbt.to_string(), contract, output_map, 0)
}

/// One line of forensics for `(txid, vout)`.
fn report(rgb: &RgbWallet, label: &str, contract: &str, txid: &str, vout: u32) -> Result<String> {
    let (verdict, visible, ord, stash_tx, invalid) =
        rgb.diagnose_allocation(contract, txid, vout)?;
    let line = format!(
        "  {label}: verdict={verdict} visible={visible} witness_ord={ord:?} \
         stash_holds_witness_tx={stash_tx} invalid_bundles={invalid}"
    );
    println!("{line}");
    Ok(verdict)
}

pub fn execute() -> Result<()> {
    println!("RGB16 — [D3] why a LEGACY-lane carrier cannot be coloured\n");

    // ---- A. A genuine legacy-lane piece. --------------------------------------------------------
    //
    // alice issues, then builds the legacy coloured split EXACTLY as
    // `mercuryrustlib::rgb::create_colored_split_tx` does: one unsigned tx spending the carrier's
    // funding output into [piece, change], coloured with `rgb.color(...)` (rgb-lib
    // `color_psbt_and_consume`) under a sender-chosen blinding, and NOT broadcast. No signature is
    // needed — colouring does not look at witnesses — so no SE is involved.
    let (mut alice, contract, f_txid, f_vout, f_value, f_spk) =
        setup("rgb-data16-alice", "D3LEG")?;
    println!("=== RGB16 part A: a legacy-lane piece ===");
    println!("  contract={contract}  carrier F={f_txid}:{f_vout} ({f_value} sat)");

    let piece_addr = p2tr_address(1)?;
    let change_addr = p2tr_address(2)?;
    let change_sats = f_value - PIECE_SATS - 500; // 500 sat of fee; exact value is irrelevant here
    let split = Transaction {
        version: 2,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            previous_output: OutPoint { txid: Txid::from_str(&f_txid)?, vout: f_vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xFFFF_FFFD),
            witness: Witness::default(),
        }],
        output: vec![
            TxOut { value: PIECE_SATS, script_pubkey: spk_of(&piece_addr)? },
            TxOut { value: change_sats, script_pubkey: spk_of(&change_addr)? },
        ],
    };
    let mut psbt = Psbt::from_unsigned_tx(split)?;
    let mut input = PsbtInput {
        witness_utxo: Some(TxOut {
            value: f_value,
            script_pubkey: ScriptBuf::from(hex::decode(&f_spk)?),
        }),
        ..Default::default()
    };
    input.sighash_type = Some(PsbtSighashType::from_str("SIGHASH_ALL")?);
    psbt.inputs = vec![input];

    let mut output_map: HashMap<u32, u64> = HashMap::new();
    output_map.insert(0, PAID);
    output_map.insert(1, ISSUED - PAID);
    let (colored_b64, consignment) =
        alice.color(&psbt.to_string(), &contract, output_map, LEGACY_BLINDING)?;
    let colored = Psbt::from_str(&colored_b64)?.unsigned_tx;
    let split_txid = colored.txid().to_string();
    // The opret shifts every payload by one when any output is P2TR (the fork's `opreturn_first`),
    // so the piece's vout is DERIVED, never assumed.
    let piece_script = spk_of(&piece_addr)?;
    let piece_vout = colored
        .output
        .iter()
        .position(|o| o.script_pubkey == piece_script)
        .ok_or_else(|| anyhow!("the coloured split lost the piece output"))? as u32;
    let piece_spk = hex::encode(piece_script.as_bytes());
    println!("  legacy split (UN-BROADCAST) {split_txid}, piece at vout {piece_vout} ({PIECE_SATS} sat)");

    // The piece is above the coloured ROOT floor: "too small to ladder" is excluded by arithmetic,
    // both numbers read from the product rather than copied.
    let root_floor = mercuryrustlib::tesr::colored_ladder_floor(
        mercurylib::tesr::TesrParams::regtest().committed_fee_rate,
        mercuryrustlib::tesr::COLORED_LADDER_DUST,
    );
    assert!(
        PIECE_SATS >= root_floor,
        "the whole point of this test is a piece ABOVE the floor: {PIECE_SATS} < {root_floor}"
    );
    println!("  piece {PIECE_SATS} sat >= coloured ROOT floor {root_floor} sat — size is NOT the issue");

    // bob's wallet, and the consignment genuinely assigns him the amount.
    let (mut bob, _, _, _, _, _) = setup("rgb-data16-bob", "D3BOB")?;
    let assigned = bob.accept_offchain_amount(&consignment, &[split_txid.clone()], &split_txid, piece_vout)?;
    assert_eq!(
        assigned, PAID,
        "the consignment must genuinely assign {PAID} to the piece — otherwise this test is about \
         a broken consignment rather than about the receive path"
    );
    println!("  bob: accept_offchain_amount agrees the consignment assigns {assigned}\n");

    // ---- B. THE REPRODUCTION: the legacy booking, then both answers at once. ---------------------
    //
    // These two calls ARE the legacy receiver (`accept_incoming_tokens`): import the contract, then
    // write a sqlite allocation row. Nothing else. No `accept_transfer`, no `store_secret_seal`.
    println!("=== RGB16 part B: the legacy booking, and the refusal it produces ===");
    bob.import_asset_offchain(&consignment, &[split_txid.clone()])?;
    bob.register_statechain(&split_txid, piece_vout, PIECE_SATS, &contract, PAID, &[])?;

    let (settled, future, spendable) = bob.balance(&contract)?;
    println!("  bob balance(settled={settled}, future={future}, spendable={spendable}) — sqlite says the piece is his");
    assert_eq!(
        future, PAID,
        "the legacy booking must have booked {PAID}: if it did not, the refusal below would be \
         trivially explained and this test would prove nothing"
    );

    let refusal = probe(&bob, &contract, &split_txid, piece_vout, PIECE_SATS, &piece_spk, PAID)
        .err()
        .ok_or_else(|| {
            anyhow!(
                "RGB16 part B did NOT reproduce: rgb-lib coloured the legacy-lane piece. If the \
                 SDK's legacy receive path has since been changed to accept the consignment into \
                 the stock, this test has done its job and should be RE-DERIVED (not deleted) — \
                 the migration fact it documents would no longer hold."
            )
        })?
        .to_string();
    println!("  bob probe_spendable({PAID}) -> {refusal}");
    assert!(
        refusal.contains("Invalid coloring info"),
        "the refusal must be rgb-lib's colouring refusal, not something else: {refusal}"
    );
    assert!(
        refusal.contains("greater than available (0)"),
        "the refusal must be the AVAILABLE-ZERO one — that zero is the whole phenomenon: {refusal}"
    );
    println!(
        "  REPRODUCED: sqlite reports {PAID} booked and color_psbt reports available (0), at the \
         same instant, on the same outpoint\n"
    );

    // ---- C. THE CAUSE, named. --------------------------------------------------------------------
    println!("=== RGB16 part C: the cause ===");
    let verdict = report(&bob, "legacy piece", &contract, &split_txid, piece_vout)?;
    let (_, visible, ord, stash_tx, invalid) =
        bob.diagnose_allocation(&contract, &split_txid, piece_vout)?;
    assert_eq!(visible, 0, "contract_assignments_for must see nothing — that is the `available (0)`");
    assert_eq!(
        ord, None,
        "the stock must hold NO ord for the split witness: the transfer was never accepted into it"
    );
    assert_eq!(
        verdict, "WitnessUnknownToStock",
        "the cause must be 'never accepted', not anything else"
    );
    // THE E7 EXCLUSION, and it is the point of the whole part. E7 = accepted, then ARCHIVED, which
    // also invalidates bundles recursively. Neither happened here, so no guard has a gap: the
    // `TentativeStashResolver` added for E7 protects a witness the stock HOLDS an ord for, and this
    // stock holds none.
    assert_ne!(verdict, "WitnessArchived", "this is NOT the E7 class");
    assert_eq!(
        invalid, 0,
        "zero invalid bundles rules out the recursive half of E7 — archival would have left some"
    );
    assert!(
        !stash_tx,
        "the stash must not hold the split witness either: `save_new_asset` imports the CONTRACT, \
         never the transfer, so neither the material nor the verdict reaches this wallet"
    );
    println!(
        "  NOT E7: the witness was never archived — the stock holds no ord for it at all, the stash \
         holds no tx for it either, and no bundle was invalidated. Nothing was destroyed here; \
         nothing was ever accepted.\n"
    );

    // ---- D. THE CONTROL: the same bytes, through the coloured lane's accept. ----------------------
    //
    // This is what turns part C from a claim into a diagnosis. Same wallet, same consignment, same
    // outpoint, same amount — only `accept_ladder` (rgb-lib `accept_offchain_ladder`) added, which
    // is precisely the call the coloured receive path makes and the legacy one does not.
    println!("=== RGB16 part D (control): the same consignment through accept_ladder ===");
    let received = bob.accept_ladder(
        &consignment,
        &[split_txid.clone()],
        &[(split_txid.clone(), piece_vout, LEGACY_BLINDING)],
    )?;
    assert_eq!(received, PAID, "accept_ladder must land the same {PAID}");
    let verdict = report(&bob, "after accept_ladder", &contract, &split_txid, piece_vout)?;
    let (_, visible, ord, _, invalid) =
        bob.diagnose_allocation(&contract, &split_txid, piece_vout)?;
    assert_eq!(verdict, "Spendable", "the same piece must now be spendable");
    assert_eq!(visible, PAID, "the stock must now see exactly the paid amount");
    assert_eq!(
        ord.as_deref(),
        Some("tentative"),
        "an un-broadcast witness must be TENTATIVE — Archived here would be the E7 defect"
    );
    assert_eq!(invalid, 0, "and still no invalid bundle");
    probe(&bob, &contract, &split_txid, piece_vout, PIECE_SATS, &piece_spk, PAID).map_err(|e| {
        anyhow!("the control failed: even after accept_ladder rgb-lib will not colour the piece ({e})")
    })?;
    println!("  CONTROL PASSED: the piece was always colourable — only the receive path was wrong\n");

    // ---- E. The probe is discriminating. ---------------------------------------------------------
    println!("=== RGB16 part E: the probe discriminates ===");
    let over = probe(
        &bob,
        &contract,
        &split_txid,
        piece_vout,
        PIECE_SATS,
        &piece_spk,
        PAID + 1,
    );
    assert!(
        over.is_err(),
        "the probe accepted MORE than the allocation ({}), so its success in part D proves nothing",
        PAID + 1
    );
    println!("  probe_spendable({}) refused, as required\n", PAID + 1);

    println!(
        "RGB16 — OK\n\n\
         [D3] VERDICT: a legacy-lane piece is un-colourable because its receiver never accepts the\n\
         transfer into the RGB stock — `import_asset_offchain` imports the contract only and\n\
         `register_statechain` writes a sqlite row. Not the E7 class (nothing was archived, no\n\
         bundle invalidated), so no guard has a gap. Permanent for every such piece already in\n\
         circulation, and independent of the carrier's size — which is exactly why the migration\n\
         hatch must key on a `color_psbt` probe rather than on the coloured root floor."
    );
    Ok(())
}
