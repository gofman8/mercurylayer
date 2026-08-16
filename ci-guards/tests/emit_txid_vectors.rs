//! GENERATES txid vectors for the SE, including a WITNESS-BEARING transaction.
//!
//! # Why the SE needs a txid at all
//!
//! The leaf registry's parent edge must be SE-AUTHORED, not client-asserted. A child's tier spends
//! `(SP.txid, j)`, and only the SE can say which sid it co-signed `SP` under — but only if it can
//! compute `SP.txid` itself from the bytes it was shown. Taking a parent id from the request would
//! let a caller graft its leaf onto someone else's tree, which is the whole ballgame for REQ-56: the
//! frontier decides who gets paid.
//!
//! # The two traps this pins
//!
//! 1. **Witness data must NOT be hashed.** `txid` is the double-SHA256 of the LEGACY serialisation;
//!    including the witness yields `wtxid`, a different 32 bytes. Every tier the SE sees today is
//!    unsigned and therefore witness-free, so a wrong implementation would agree with reality until
//!    the first signed transaction reached it — which is exactly the kind of bug that ships. One
//!    case below carries a witness for that reason.
//! 2. **Byte order.** `Txid`'s Display is REVERSED (the form block explorers show); the outpoint on
//!    the wire uses internal order. Comparing the wrong one is a bug that looks like a hash
//!    mismatch and is usually "fixed" by reversing something else. Both forms are emitted, and the
//!    C++ side compares internal order — the one an outpoint actually contains.
//!
//! Run: `cargo test -p ci-guards --test emit_txid_vectors -- --nocapture`

use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use std::fmt::Write as _;

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

fn p2tr(byte: u8) -> ScriptBuf {
    let mut v = vec![0x51u8, 0x20];
    v.extend_from_slice(&[byte; 32]);
    ScriptBuf::from_bytes(v)
}

/// The P2A anchor every tier carries: `OP_1 <2-byte>`. Not P2TR, so it must never be mistaken for
/// the payload output.
fn p2a() -> ScriptBuf {
    ScriptBuf::from_bytes(vec![0x51, 0x02, 0x4e, 0x73])
}

struct Case {
    name: &'static str,
    tx: Transaction,
    /// The vout the SE must select as the payload, and the key/value it must read from it.
    payload_vout: u32,
    payload_key: u8,
    payload_value: u64,
}

fn cases() -> Vec<Case> {
    let input = |txid_byte: u8, witness: bool| TxIn {
        previous_output: OutPoint {
            txid: Txid::from_byte_array([txid_byte; 32]),
            vout: 0,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence(0xFFFF_FFFD),
        witness: if witness {
            let mut w = Witness::new();
            w.push([0x99u8; 64]); // a schnorr signature's worth of bytes
            w
        } else {
            Witness::new()
        },
    };

    vec![
        Case {
            name: "tier_unsigned",
            tx: Transaction {
                version: bitcoin::transaction::Version(3),
                lock_time: LockTime::ZERO,
                input: vec![input(0x11, false)],
                output: vec![
                    TxOut { value: Amount::from_sat(99_000), script_pubkey: p2tr(0x22) },
                    TxOut { value: Amount::from_sat(240), script_pubkey: p2a() },
                ],
            },
            payload_vout: 0,
            payload_key: 0x22,
            payload_value: (99_000),
        },
        Case {
            // THE ONE THAT MATTERS: a witness is present. txid must ignore it entirely. An
            // implementation that hashes the segwit serialisation produces wtxid here and matches
            // on every other case in this file.
            name: "tier_WITH_witness",
            tx: Transaction {
                version: bitcoin::transaction::Version(3),
                lock_time: LockTime::ZERO,
                input: vec![input(0x33, true)],
                output: vec![
                    TxOut { value: Amount::from_sat(88_000), script_pubkey: p2tr(0x44) },
                    TxOut { value: Amount::from_sat(240), script_pubkey: p2a() },
                ],
            },
            payload_vout: 0,
            payload_key: 0x44,
            payload_value: (88_000),
        },
        Case {
            // Payload NOT at index 0: the anchor comes first. The SE must select structurally, not
            // positionally (REQ-61a) — a client-supplied index is attacker-controlled.
            name: "anchor_first",
            tx: Transaction {
                version: bitcoin::transaction::Version(3),
                lock_time: LockTime::ZERO,
                input: vec![input(0x55, false)],
                output: vec![
                    TxOut { value: Amount::from_sat(240), script_pubkey: p2a() },
                    TxOut { value: Amount::from_sat(77_000), script_pubkey: p2tr(0x66) },
                ],
            },
            payload_vout: 1,
            payload_key: 0x66,
            payload_value: (77_000),
        },
        Case {
            name: "locktime_and_high_value",
            tx: Transaction {
                version: bitcoin::transaction::Version(3),
                lock_time: LockTime::from_consensus(800_000),
                input: vec![input(0x77, false)],
                output: vec![
                    TxOut { value: Amount::from_sat(2_100_000_000_000_000), script_pubkey: p2tr(0x88) },
                    TxOut { value: Amount::from_sat(240), script_pubkey: p2a() },
                ],
            },
            payload_vout: 0,
            payload_key: 0x88,
            payload_value: (2_100_000_000_000_000),
        },
    ]
}

#[test]
fn emit_txid_vectors() {
    let mut out = String::new();
    out.push_str("// GENERATED by `cargo test -p ci-guards --test emit_txid_vectors`.\n");
    out.push_str("// Regenerate rather than hand-edit.\n");
    out.push_str("// txid_internal is the byte order an OUTPOINT carries; txid_display is the\n");
    out.push_str("// reversed form explorers show. The C++ side compares txid_internal.\n");
    out.push_str("// Fields: name, tx_hex, txid_internal, txid_display, payload_vout,\n");
    out.push_str("//         payload_xonly, payload_value\n");

    for c in cases() {
        let tx_hex = serialize_hex(&c.tx);
        let txid = c.tx.txid();
        let internal = hex(&txid.to_raw_hash().to_byte_array());
        let display = txid.to_string();

        // Sanity, so a broken emitter cannot quietly produce agreeing-but-wrong vectors: the two
        // forms must be byte-reverses of each other.
        let mut rev: Vec<u8> = txid.to_raw_hash().to_byte_array().to_vec();
        rev.reverse();
        assert_eq!(hex(&rev), display, "{}: display form is not the reverse of internal", c.name);

        let _ = writeln!(
            out,
            "    {{\"{}\", \"{}\", \"{}\", \"{}\", {}, \"{}\", {}}},",
            c.name,
            tx_hex,
            internal,
            display,
            c.payload_vout,
            hex(&[c.payload_key; 32]),
            c.payload_value
        );
    }

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../lockbox/tests/txid_vectors.inc");
    std::fs::write(path, &out).expect("write vectors");
    println!("wrote {} txid vectors to {path}", cases().len());
    print!("{out}");
}
