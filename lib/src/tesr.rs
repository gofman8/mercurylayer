//! TES-R (Trigger / Extension / State) transaction tier builders for Utexo V2.
//!
//! See `docs/utexo/V2-DESIGN.md`. A coin's funding UTXO `F` (P2TR of the aggregate key `A`) rests
//! on-chain; above it hangs a pre-signed, **un-broadcast** tree of three tiers, all v3 (TRUC) with a
//! P2A anchor for fee-bumping:
//!
//! ```text
//! F ──▶ T  TRIGGER    no relative-timelock, signed once at deposit
//!       └▶ X_m EXTENSION  input nSequence = CSV E_m = E0 − m·δE   (renewal replaces horizontally)
//!          └▶ S_k STATE   input nSequence = CSV Δ_k = D0 − k·δ    (decrements per transfer)
//! ```
//!
//! The core property: BIP-68 relative timelocks only start counting once the PARENT confirms, and
//! `T` has no timelock, so **nothing anywhere matures until someone broadcasts `T` on-chain**. An
//! idle coin never ages. Every tier pays the same aggregate key `A` (Stage-1: no per-tier H_tag
//! tweaks yet — the sighash binds prevout+outputs, so no cross-tier signature replay is possible),
//! which is why one blind-MuSig2 co-sign path ([`cosign_tier_request`]) serves all tiers: only the
//! prevout *amount* differs (a tier spends the parent tier's output, not `coin.amount`).

use std::str::FromStr;

use bitcoin::{
    absolute,
    sighash::{self, SighashCache, TapSighashType},
    Address, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::MercuryError,
    transaction::{calculate_musig_session, PartialSignatureMsg1},
    utils::get_network,
    wallet::Coin,
};

/// Pay-to-Anchor scriptPubKey `OP_1 <0x4e73>` — the standard anyone-can-spend anchor (Bitcoin Core
/// 28+ relays it) that lets any party (owner, keyless tower, operator) attach a live-rate fee child
/// to a pre-signed tier tx during a fee spike.
pub const P2A_SCRIPT_BYTES: [u8; 4] = [0x51, 0x02, 0x4e, 0x73];
/// Value parked in each tier tx's P2A anchor output (sats). Above the P2A dust floor.
pub const P2A_VALUE: u64 = 240;
/// Approximate virtual size of a tier tx (1 P2TR key-spend in + 1 P2TR out + 1 P2A out + v3
/// overhead), used to size the committed fee baked into each pre-signed tx.
pub const TIER_VBYTES: u64 = 124;

/// The Pay-to-Anchor output script.
pub fn p2a_script() -> ScriptBuf {
    ScriptBuf::from(P2A_SCRIPT_BYTES.to_vec())
}

/// Committed fee (sats) baked into a tier tx at `fee_rate_sats_per_vb`, so the base case relays and
/// confirms standalone (restoring V1's self-funding property); the P2A anchor tops it up in a spike.
pub fn committed_fee(fee_rate_sats_per_vb: f64) -> u64 {
    (TIER_VBYTES as f64 * fee_rate_sats_per_vb).ceil() as u64
}

/// Value flowing to a tier's main output = parent value − committed fee − the P2A anchor value.
/// Returns `None` if the coin is too small to carry one more tier (the terminal "dust" case).
pub fn tier_out_value(prev_value: u64, fee_rate_sats_per_vb: f64) -> Option<u64> {
    prev_value.checked_sub(committed_fee(fee_rate_sats_per_vb) + P2A_VALUE)
}

/// nSequence for a BIP-68 relative-block-height lock of `blocks` (the tx must be v≥2; TES-R tiers are
/// v3). For a block-height lock the raw value equals `blocks` — the type flag (bit 22) and the
/// disable bit (bit 31) are both clear.
pub fn csv_blocks(blocks: u16) -> Sequence {
    Sequence(blocks as u32)
}

/// Protocol parameters for the TES-R ladder — the relative-timelock schedule the wallet uses to size
/// each tier and to decide when to renew or roll over. Mainnet defaults are from
/// `docs/utexo/V2-DESIGN.md` §5.2; in production the SE serves them via `/info/config` so a receiver
/// can detect a per-victim parameter split, but they are pure protocol constants (no fund is at
/// stake in getting them "wrong" — only exit-wait length and renewal cadence).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TesrParams {
    /// State-tier initial CSV `D0` (blocks): the head start the current owner has over a stale state.
    pub d0: u16,
    /// Per-transfer state decrement `δ`.
    pub delta: u16,
    /// State floor `D_floor`: when the next state would fall below this, renew.
    pub d_floor: u16,
    /// Extension-tier initial CSV `E0`.
    pub e0: u16,
    /// Per-renewal extension decrement `δE`.
    pub delta_e: u16,
    /// Extension floor `E_floor`: when the next extension would fall below this, roll over.
    pub e_floor: u16,
    /// Renewals allowed before a forced rollover (never operate at the extension floor).
    pub m_max: u16,
    /// Committed self-funding fee rate (sat/vB) baked into each pre-signed tier tx.
    pub committed_fee_rate: f64,
}

impl Default for TesrParams {
    fn default() -> Self {
        Self::mainnet()
    }
}

impl TesrParams {
    /// Mainnet defaults (V2-DESIGN §5.2): 36-block (~6 h) head starts, ~17 extension epochs, forced
    /// rollover at m=15, 2 sat/vB committed fee.
    pub fn mainnet() -> Self {
        Self { d0: 1440, delta: 36, d_floor: 144, e0: 720, delta_e: 36, e_floor: 144, m_max: 15, committed_fee_rate: 2.0 }
    }

    /// Test-scale schedule for regtest (fast to mine a full lifecycle).
    pub fn regtest() -> Self {
        Self { d0: 24, delta: 6, d_floor: 6, e0: 12, delta_e: 3, e_floor: 3, m_max: 2, committed_fee_rate: 2.0 }
    }

    /// The mainnet or regtest preset for a network string (`"bitcoin"` → mainnet, else regtest).
    pub fn for_network(network: &str) -> Self {
        if network.eq_ignore_ascii_case("bitcoin") || network.eq_ignore_ascii_case("mainnet") {
            Self::mainnet()
        } else {
            Self::regtest()
        }
    }

    /// State CSV at state-count `k`: `D0 − k·δ`, clamped to the floor.
    pub fn state_csv(&self, k: u16) -> u16 {
        self.d0.saturating_sub(k.saturating_mul(self.delta)).max(self.d_floor)
    }

    /// Extension CSV at renewal epoch `m`: `E0 − m·δE`, clamped to the floor.
    pub fn ext_csv(&self, m: u16) -> u16 {
        self.e0.saturating_sub(m.saturating_mul(self.delta_e)).max(self.e_floor)
    }

    /// True once the NEXT state (`k+1`) would fall below the floor — renew before spending again.
    pub fn needs_renewal(&self, k: u16) -> bool {
        self.d0.saturating_sub(k.saturating_add(1).saturating_mul(self.delta)) < self.d_floor
    }

    /// True once the renewal budget is spent (`m ≥ m_max`) — roll over to a fresh level instead.
    pub fn needs_rollover(&self, m: u16) -> bool {
        m >= self.m_max
    }
}

/// nSequence for the trigger tier: relative-timelock DISABLED, still RBF-signalling. `T` spends the
/// confirmed funding UTXO with no wait.
pub const TRIGGER_SEQUENCE: Sequence = Sequence(0xFFFF_FFFD);

/// Build one un-signed TES-R tier transaction (nVersion=3 / TRUC): spends `prev` (a P2TR(A) output)
/// under `sequence`, pays `out_value` to `out_spk`, plus the P2A anchor. `out_value` must already be
/// net of the committed fee and the anchor (see [`tier_out_value`]). No absolute locktime.
pub fn build_tier_tx(
    prev_txid: Txid,
    prev_vout: u32,
    sequence: Sequence,
    out_spk: ScriptBuf,
    out_value: u64,
) -> Transaction {
    Transaction {
        version: 3,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            previous_output: OutPoint { txid: prev_txid, vout: prev_vout },
            script_sig: ScriptBuf::new(),
            sequence,
            witness: Witness::default(),
        }],
        output: vec![
            TxOut { value: out_value, script_pubkey: out_spk },
            TxOut { value: P2A_VALUE, script_pubkey: p2a_script() },
        ],
    }
}

/// Encoded (hex) result of building a tier tx, plus the value it pays forward (the prevout value the
/// child tier will spend) and its txid. The txid is stable across signing (a key-spend adds only
/// witness data), so a child tier can reference its parent before the parent is co-signed.
pub struct TierTx {
    pub txid: String,
    pub tx_hex: String,
    pub out_value: u64,
}

fn encode(tx: &Transaction) -> TierTx {
    TierTx {
        txid: tx.txid().to_string(),
        tx_hex: hex::encode(bitcoin::consensus::encode::serialize(tx)),
        out_value: tx.output[0].value,
    }
}

/// The scriptPubKey for a bech32(m) address string on `network`.
fn spk_from_address(address: &str, network: &str) -> Result<ScriptBuf, MercuryError> {
    let net = get_network(network)?;
    Ok(Address::from_str(address)
        .map_err(|_| MercuryError::InvalidBitcoinAddressError)?
        .require_network(net)
        .map_err(|_| MercuryError::BitcoinAddressMismatchNetworkError)?
        .script_pubkey())
}

/// TRIGGER `T`: spends the funding UTXO `F`, no relative-timelock. Pays `to_address` = P2TR(A) so the
/// coin's own key can co-op de-trigger or the ladder can hang beneath it. `FeeTooHigh` if `F` is too
/// small to carry another tier (the terminal maintenance-cost case).
pub fn build_trigger(
    f_txid: &str,
    f_vout: u32,
    f_value: u64,
    to_address: &str,
    network: &str,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    let txid = Txid::from_str(f_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    let out_value = tier_out_value(f_value, fee_rate).ok_or(MercuryError::FeeTooHigh)?;
    let spk = spk_from_address(to_address, network)?;
    Ok(encode(&build_tier_tx(txid, f_vout, TRIGGER_SEQUENCE, spk, out_value)))
}

/// EXTENSION `X_m`: spends `T.out[0]` under relative-timelock `csv_e = E0 − m·δE`, paying P2TR(A).
pub fn build_extension(
    t_txid: &str,
    t_out_value: u64,
    to_address: &str,
    network: &str,
    csv_e: u16,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    let txid = Txid::from_str(t_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    let out_value = tier_out_value(t_out_value, fee_rate).ok_or(MercuryError::FeeTooHigh)?;
    let spk = spk_from_address(to_address, network)?;
    Ok(encode(&build_tier_tx(txid, 0, csv_blocks(csv_e), spk, out_value)))
}

/// STATE `S_k`: spends `X_m.out[0]` under relative-timelock `csv_d = D0 − k·δ`, paying `owner_address`
/// (for a resting off-chain state this is P2TR(A); for a unilateral exit, the owner's own address).
pub fn build_state(
    x_txid: &str,
    x_out_value: u64,
    owner_address: &str,
    network: &str,
    csv_d: u16,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    let txid = Txid::from_str(x_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    let out_value = tier_out_value(x_out_value, fee_rate).ok_or(MercuryError::FeeTooHigh)?;
    let spk = spk_from_address(owner_address, network)?;
    Ok(encode(&build_tier_tx(txid, 0, csv_blocks(csv_d), spk, out_value)))
}

/// COOPERATIVE DE-TRIGGER: a FRESH spend of `T.out[0]` with the relative-timelock DISABLED (paying
/// `to_address`). Because it has no CSV wait while every pre-signed extension needs `E ≥ E_floor`
/// confirmations, it confirms first — collapsing a hostile trigger to a priced nuisance and killing
/// the stale ladder (its extensions can never confirm once `T.out[0]` is spent).
pub fn build_detrigger(
    t_txid: &str,
    t_out_value: u64,
    to_address: &str,
    network: &str,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    let txid = Txid::from_str(t_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    let out_value = tier_out_value(t_out_value, fee_rate).ok_or(MercuryError::FeeTooHigh)?;
    let spk = spk_from_address(to_address, network)?;
    Ok(encode(&build_tier_tx(txid, 0, TRIGGER_SEQUENCE, spk, out_value)))
}

/// Blind-co-sign one TES-R tier tx (client half). Mirrors the colored-tx co-sign path exactly, but
/// takes an explicit `prevout_value`: a tier spends the PARENT tier's output (value net of that
/// tier's fee), which is not `coin.amount`. The prevout scriptPubKey is always the coin's aggregate
/// address (every tier pays the same key `A`), so the same MuSig2 session applies. Returns the
/// client's partial signature + session; the caller relays the payload to the SE (`/sign/second`),
/// aggregates with [`crate::transaction::create_signature`], and finalises with
/// [`crate::transaction::new_backup_transaction`].
pub fn cosign_tier_request(
    coin: &Coin,
    encoded_unsigned_tx: String,
    prevout_value: u64,
    network: String,
) -> core::result::Result<PartialSignatureMsg1, MercuryError> {
    let network = get_network(&network)?;

    let tx_bytes = hex::decode(&encoded_unsigned_tx)?;
    let unsigned_tx: Transaction = bitcoin::consensus::encode::deserialize(&tx_bytes)?;
    if unsigned_tx.input.len() != 1 {
        return Err(MercuryError::MoreThanOneInputError);
    }

    let input_address =
        Address::from_str(coin.aggregated_address.as_ref().unwrap())?.require_network(network)?;
    let prevout = TxOut { value: prevout_value, script_pubkey: input_address.script_pubkey() };

    let hash = SighashCache::new(&unsigned_tx).taproot_key_spend_signature_hash(
        0,
        &sighash::Prevouts::All(&[prevout]),
        TapSighashType::All,
    )?;

    calculate_musig_session(coin, hash, encoded_unsigned_tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p2a_script_is_op1_4e73() {
        assert_eq!(p2a_script().as_bytes(), &[0x51, 0x02, 0x4e, 0x73]);
    }

    #[test]
    fn csv_encodes_relative_block_lock() {
        // Raw nSequence equals the block count; disable bit (31) and type bit (22) are clear.
        let s = csv_blocks(24);
        assert_eq!(s.0, 24);
        assert_eq!(s.0 & (1 << 31), 0, "disable bit must be clear (lock enabled)");
        assert_eq!(s.0 & (1 << 22), 0, "type bit must be clear (block-height lock)");
    }

    #[test]
    fn trigger_sequence_disables_relative_lock() {
        assert_ne!(TRIGGER_SEQUENCE.0 & (1 << 31), 0, "disable bit set: no relative lock");
        assert!(TRIGGER_SEQUENCE.0 < 0xFFFF_FFFE, "still RBF-signalling");
    }

    #[test]
    fn tier_value_decrements_by_fee_and_anchor() {
        // 100_000 sats, 2 sat/vB → fee 248, anchor 240 → 99_512 forward.
        assert_eq!(tier_out_value(100_000, 2.0), Some(100_000 - 248 - 240));
        // Too small → terminal dust case.
        assert_eq!(tier_out_value(300, 2.0), None);
    }

    #[test]
    fn params_schedule_decrements_and_clamps() {
        let p = TesrParams::mainnet();
        // State CSV decrements by δ each transfer, clamped at the floor.
        assert_eq!(p.state_csv(0), 1440);
        assert_eq!(p.state_csv(1), 1440 - 36);
        assert_eq!(p.state_csv(10_000), p.d_floor, "clamped at floor, never underflows");
        // Extension CSV decrements by δE each renewal, clamped at the floor.
        assert_eq!(p.ext_csv(0), 720);
        assert_eq!(p.ext_csv(1), 720 - 36);
        assert_eq!(p.ext_csv(10_000), p.e_floor);
    }

    #[test]
    fn params_renewal_and_rollover_thresholds() {
        let p = TesrParams::regtest(); // d0=24, δ=6, floor=6  → renew when next state < 6
        // next state = d0 - (k+1)*δ ; renew when that < d_floor(6): k+1 > 3 → k >= 3.
        assert!(!p.needs_renewal(0));
        assert!(!p.needs_renewal(2));
        assert!(p.needs_renewal(3), "24 - 4*6 = 0 < 6 → must renew");
        // Rollover at the renewal cap m_max=2.
        assert!(!p.needs_rollover(1));
        assert!(p.needs_rollover(2));
    }

    #[test]
    fn params_network_presets() {
        assert_eq!(TesrParams::for_network("bitcoin"), TesrParams::mainnet());
        assert_eq!(TesrParams::for_network("regtest"), TesrParams::regtest());
        assert_eq!(TesrParams::for_network("signet"), TesrParams::regtest());
    }

    #[test]
    fn tier_tx_is_v3_with_anchor() {
        let txid = Txid::from_str(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let tx = build_tier_tx(txid, 0, csv_blocks(24), p2a_script(), 50_000);
        assert_eq!(tx.version, 3);
        assert_eq!(tx.input[0].sequence, csv_blocks(24));
        assert_eq!(tx.output.len(), 2);
        assert_eq!(tx.output[1].value, P2A_VALUE);
        assert_eq!(tx.output[1].script_pubkey.as_bytes(), &[0x51, 0x02, 0x4e, 0x73]);
    }
}
