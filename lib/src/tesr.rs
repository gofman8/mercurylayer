//! TES-R (Trigger / Extension / State) transaction tier builders for Mercury Utexo.
//!
//! See `docs/utexo/PROTOCOL.md`. A coin's funding UTXO `F` (P2TR of the aggregate key `A`) rests
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
/// **The signed virtual size of a one-payload UNCOLOURED tier — MEASURED, not approximated.**
///
/// Byte for byte, over the shape [`build_tier_tx`] emits and
/// [`crate::transaction::new_backup_transaction`] finalises:
///
/// | part                                                     | bytes |
/// |----------------------------------------------------------|-------|
/// | nVersion (3) + nLockTime                                 | 8     |
/// | input count + output count                               | 2     |
/// | input (32 txid + 4 vout + 1 scriptSig len + 4 nSequence)  | 41    |
/// | P2TR payload out (8 value + 1 len + 34 scriptPubKey)      | 43    |
/// | P2A anchor out (8 + 1 + 4 = `OP_1 <0x4e73>`)              | 13    |
///
/// base = **107 B** ⟹ weight = `4·107 + 2` (segwit marker+flag) `+ 67` (witness: 1 item-count varint
/// + 1 length varint + a **65-byte** signature) = **497 WU** ⟹ vsize = `⌈497/4⌉` = **125 vB**.
///
/// **The 65th signature byte is the whole of [D4].** A taproot key-spend signature is 64 bytes *only*
/// under `SIGHASH_DEFAULT`. TES-R does not sign that way: [`cosign_tier_request`] hashes with
/// [`TapSighashType::All`], and `new_backup_transaction{,_multi}` serialise
/// `taproot::Signature { hash_ty: All }`, which appends the explicit `0x01` sighash byte. The former
/// value **124** modelled the 64-byte `SIGHASH_DEFAULT` witness (`4·107 + 2 + 66 = 496` ⟹ 124 vB) and
/// therefore understated EVERY uncoloured tier by exactly 1 vB: at the default 2 sat/vB it committed
/// 248 sat to a transaction that relays at 125 vB — 1.984 sat/vB, short of the rate the pre-signed
/// tier is supposed to be able to confirm at with **no P2A child attached**, which is the entire
/// reason the committed fee exists.
///
/// Pinned against a real, production-finalised transaction by the unit test
/// `tests::the_uncoloured_fee_matches_a_measured_signed_tier`. The coloured sibling is
/// `mercuryrustlib::rgb::COLORED_TIER_VBYTES` = **168**, and the two now differ by exactly one
/// [`P2TR_OUT_VBYTES`] (the RGB `opret` output) — `125 + 43 = 168`, the surcharge identity
/// `SDK_E2E=74` asserts.
pub const TIER_VBYTES: u64 = 125;

/// The Pay-to-Anchor output script.
pub fn p2a_script() -> ScriptBuf {
    ScriptBuf::from(P2A_SCRIPT_BYTES.to_vec())
}

/// Committed fee (sats) baked into a tier tx at `fee_rate_sats_per_vb`, so the base case relays and
/// confirms standalone (the same self-funding property a backup tx of an un-laddered coin has); the
/// P2A anchor tops it up in a spike.
pub fn committed_fee(fee_rate_sats_per_vb: f64) -> u64 {
    (TIER_VBYTES as f64 * fee_rate_sats_per_vb).ceil() as u64
}

/// Value flowing to a tier's main output = parent value − committed fee − the P2A anchor value.
/// Returns `None` if the coin is too small to carry one more tier (the terminal "dust" case).
pub fn tier_out_value(prev_value: u64, fee_rate_sats_per_vb: f64) -> Option<u64> {
    // `checked_add` on the ANCHOR too, not just the subtraction. `committed_fee` ends in
    // `(.. as f64 * rate).ceil() as u64`, which SATURATES at `u64::MAX` for an absurd rate rather
    // than erroring — so `committed_fee(..) + P2A_VALUE` overflowed and PANICKED in debug builds.
    //
    // That is reachable from attacker-controlled input: `fee_rate` arrives on a conveyed bundle, and
    // a panic in a verifier is worse than a wrong answer — it takes down the whole claim pass, not
    // one coin. Found by `forged_yardstick_attack_tests`, which pinned it as a live gap.
    committed_fee(fee_rate_sats_per_vb)
        .checked_add(P2A_VALUE)
        .and_then(|cost| prev_value.checked_sub(cost))
}

/// The smallest value an in-ladder split CHILD can carry and still be exitable.
///
/// A child of an in-ladder split is not a bare output: `establish_child` hangs the child's OWN
/// headless ladder off `SP.out[j]` — an extension tier and a state tier — and each tier burns
/// `committed_fee(rate) + P2A_VALUE`. The child's final state output must then still clear the
/// caller's dust floor to be broadcastable.
///
/// This is strictly larger than the plain backup-fee floor (`DUST_LIMIT + backup fee`) that sizes
/// un-laddered sub-coins, and it is the value an admission guard MUST use: `establish_child` runs *after*
/// the parent's spend budget is consumed and `SP` is co-signed, so a child admitted below this
/// dies with [`MercuryError::FeeTooHigh`] once the parent is already terminal — stranding the
/// parent to unilateral-exit-only.
pub fn min_child_value(fee_rate_sats_per_vb: f64, dust_limit: u64) -> u64 {
    // Saturating rather than checked: this is a FLOOR used to refuse small coins, so an absurd rate
    // yielding `u64::MAX` correctly refuses everything instead of panicking.
    committed_fee(fee_rate_sats_per_vb)
        .saturating_add(P2A_VALUE)
        .saturating_mul(2)
        .saturating_add(dust_limit)
}

/// **[CATS change 2 / V5] The smallest value a SPINE TIP can carry and still be exitable.**
///
/// The tip is the sender's CHANGE leg, and its shape is not the piece's: it gets ONE cap tier
/// directly over `SP.out[K]` and **no extension** (the extension exists to reset the state budget by
/// renewal, and on the spine every payment already lands the change on a virgin outpoint at a virgin
/// `D0`, so the rung is dead weight). So it funds ONE rung, not two:
///
/// ```text
/// min_spine_tip_value = committed_fee + P2A + dust = 250 + 240 + 330 = 820   @ 2 sat/vB
/// min_child_value     = 2·(committed_fee + P2A) + dust                = 1 310
/// ```
///
/// ⚠️ **This floor is for the CHANGE leg only.** Applying it to a payee's piece admits a piece that
/// cannot fund its own two tiers — and it dies inside `establish_child`, i.e. *after* the parent's
/// spend budget is consumed and `SP` is co-signed, stranding the parent to unilateral-exit-only. That
/// is why the SDK's floor accessor returns a floor **per leg** rather than one number for both, and
/// why the leg's shape is not a caller's choice: see `mercuryrustlib::tesr::change_leg_role`.
pub fn min_spine_tip_value(fee_rate_sats_per_vb: f64, dust_limit: u64) -> u64 {
    // Saturating for the same reason as `min_child_value`: an absurd rate must refuse everything, not
    // panic in a floor computation that exists to refuse.
    committed_fee(fee_rate_sats_per_vb)
        .saturating_add(P2A_VALUE)
        .saturating_add(dust_limit)
}

/// nSequence for a BIP-68 relative-block-height lock of `blocks` (the tx must be v≥2; TES-R tiers are
/// v3). For a block-height lock the raw value equals `blocks` — the type flag (bit 22) and the
/// disable bit (bit 31) are both clear.
pub fn csv_blocks(blocks: u16) -> Sequence {
    Sequence(blocks as u32)
}

/// Protocol parameters for the TES-R ladder — the relative-timelock schedule the wallet uses to size
/// each tier and to decide when to renew or roll over. Mainnet defaults are from
/// `docs/utexo/PROTOCOL.md` §5.2; in production the SE serves them via `/info/config` so a receiver
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
    /// Mainnet defaults (PROTOCOL.md §5.2): 36-block (~6 h) head starts, ~17 extension epochs, forced
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

/// **THE payload-vout accessor.** The output index at which a tier's PAYLOAD (value-carrying,
/// P2TR(A)-or-payee) outputs begin. Every chaining site — "the child spends its parent's payload
/// output", "the tier pays `A` on its payload output", the `live_csv_by_outpoint` census key — must
/// read this rather than assuming `0`.
///
/// It is `0` today because a tier is `[payload…, P2A]`. Under CTES-R a coloured tier is
/// `[opret, payload…, P2A]` (the fork sets `opreturn_first = true` whenever any output is P2TR, and
/// the P2A anchor is *not* P2TR — see `docs/utexo/CTESR-GATE.md` §2.1(a)), so every payload shifts by
/// one. Routing every site through this one name is what makes wiring colouring a change of ONE
/// value instead of an audit of a dozen literals.
///
/// Do NOT hard-code "the opret is index 0" anywhere; derive payload vouts from the builder's
/// [`TierTx::payload_vout`], never from a positional assumption.
pub const UNCOLORED_PAYLOAD_VOUT: u32 = 0;

/// Encoded (hex) result of building a tier tx, plus the value it pays forward (the prevout value the
/// child tier will spend) and its txid. The txid is stable across signing (a key-spend adds only
/// witness data), so a child tier can reference its parent before the parent is co-signed.
pub struct TierTx {
    pub txid: String,
    pub tx_hex: String,
    pub out_value: u64,
    /// Index of this tier's FIRST payload output — [`UNCOLORED_PAYLOAD_VOUT`] today. `out_value` is
    /// read from THIS output, and a child tier spends `(txid, payload_vout)`.
    pub payload_vout: u32,
}

/// Encode a built tier, reading its forward value from the PAYLOAD output rather than `output[0]`.
/// Fails closed if `payload_vout` is out of range (a builder bug, or a coloured shape whose payload
/// index was mis-derived) rather than panicking or silently reading the wrong output.
fn encode(tx: &Transaction, payload_vout: u32) -> Result<TierTx, MercuryError> {
    let payload = tx
        .output
        .get(payload_vout as usize)
        .ok_or(MercuryError::TransactionReconstructionError)?;
    Ok(TierTx {
        txid: tx.txid().to_string(),
        tx_hex: hex::encode(bitcoin::consensus::encode::serialize(tx)),
        out_value: payload.value,
        payload_vout,
    })
}

/// The scriptPubKey a TES-R tier output should pay for `address` on `network`. Two forms, mirroring
/// `create_tx_out` exactly so a state tier can pay a transfer recipient (Model A,
/// `docs/utexo/history/MIGRATION.md`):
/// - a **Mercury transfer address** (`utexoinv…`/`tml…` HRP) → the recipient's DERIVED
///   `P2TR(recipient_user_pubkey)`, so the sender can pre-sign the receiver-paying state `S'`;
/// - a plain bech32(m) address → itself.
fn spk_from_address(address: &str, network: &str) -> Result<ScriptBuf, MercuryError> {
    let net = get_network(network)?;
    if address.starts_with(crate::MAINNET_HRP) || address.starts_with(crate::TESTNET_HRP) {
        let (_, recipient_user_pubkey, _) = crate::decode_transfer_address(address)?;
        return Ok(Address::p2tr(
            &secp256k1_zkp::Secp256k1::new(),
            recipient_user_pubkey.x_only_public_key().0,
            None,
            net,
        )
        .script_pubkey());
    }
    Ok(Address::from_str(address)
        .map_err(|_| MercuryError::InvalidBitcoinAddressError)?
        .require_network(net)
        .map_err(|_| MercuryError::BitcoinAddressMismatchNetworkError)?
        .script_pubkey())
}

/// The plain P2TR address a TES-R state should pay for `address` (Model A `owner_exit_address`).
/// A Mercury transfer address resolves to the recipient's derived `P2TR(recipient_user_pubkey)` — the
/// SAME key the backup chain's `create_tx_out` pays and that the recipient holds; a plain address is
/// returned as-is.
/// The receiver compares this against `get_user_backup_address(coin)` to confirm the ladder pays it.
pub fn payee_address(address: &str, network: &str) -> Result<String, MercuryError> {
    let net = get_network(network)?;
    if address.starts_with(crate::MAINNET_HRP) || address.starts_with(crate::TESTNET_HRP) {
        let (_, recipient_user_pubkey, _) = crate::decode_transfer_address(address)?;
        return Ok(Address::p2tr(
            &secp256k1_zkp::Secp256k1::new(),
            recipient_user_pubkey.x_only_public_key().0,
            None,
            net,
        )
        .to_string());
    }
    Ok(address.to_string())
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
    encode(&build_tier_tx(txid, f_vout, TRIGGER_SEQUENCE, spk, out_value), UNCOLORED_PAYLOAD_VOUT)
}

/// EXTENSION `X_m`: spends the trigger's PAYLOAD output under relative-timelock `csv_e = E0 − m·δE`,
/// paying P2TR(A).
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
    encode(
        &build_tier_tx(txid, UNCOLORED_PAYLOAD_VOUT, csv_blocks(csv_e), spk, out_value),
        UNCOLORED_PAYLOAD_VOUT,
    )
}

/// STATE `S_k`: spends the extension's PAYLOAD output under relative-timelock `csv_d = D0 − k·δ`,
/// paying `owner_address`
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
    encode(
        &build_tier_tx(txid, UNCOLORED_PAYLOAD_VOUT, csv_blocks(csv_d), spk, out_value),
        UNCOLORED_PAYLOAD_VOUT,
    )
}

/// [in-ladder split] Like [`build_extension`] but roots at an ARBITRARY output `(prev_txid, prev_vout)`
/// instead of the trigger's `out[0]`. A split child's ladder hangs off `SP.out[j]` (j = the child's
/// index), so its first extension spends that specific output, under the CHILD's aggregate address.
pub fn build_extension_from(
    prev_txid: &str,
    prev_vout: u32,
    prev_out_value: u64,
    to_address: &str,
    network: &str,
    csv_e: u16,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    let txid = Txid::from_str(prev_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    let out_value = tier_out_value(prev_out_value, fee_rate).ok_or(MercuryError::FeeTooHigh)?;
    let spk = spk_from_address(to_address, network)?;
    encode(&build_tier_tx(txid, prev_vout, csv_blocks(csv_e), spk, out_value), UNCOLORED_PAYLOAD_VOUT)
}

/// [in-ladder split] Like [`build_state`] but roots at an ARBITRARY output `(prev_txid, prev_vout)`.
/// A split child's owner state spends its extension's `out[0]`, so `prev_vout` is 0 in the common case,
/// but the explicit form keeps the child builders symmetric with [`build_extension_from`].
pub fn build_state_from(
    prev_txid: &str,
    prev_vout: u32,
    prev_out_value: u64,
    owner_address: &str,
    network: &str,
    csv_d: u16,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    let txid = Txid::from_str(prev_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    let out_value = tier_out_value(prev_out_value, fee_rate).ok_or(MercuryError::FeeTooHigh)?;
    let spk = spk_from_address(owner_address, network)?;
    encode(&build_tier_tx(txid, prev_vout, csv_blocks(csv_d), spk, out_value), UNCOLORED_PAYLOAD_VOUT)
}

/// vbytes of one extra P2TR resting output (8 value + 1 length + 34 scriptPubKey).
pub const P2TR_OUT_VBYTES: u64 = 43;

/// Committed fee for a tier carrying `n_payload` value outputs (plus the P2A anchor). `TIER_VBYTES` is
/// sized for the 1-payload base case, so each extra child adds `P2TR_OUT_VBYTES`. A split state MUST
/// use this rather than [`committed_fee`], or it underpays and cannot relay standalone.
pub fn committed_fee_for_outputs(n_payload: usize, fee_rate_sats_per_vb: f64) -> u64 {
    let extra = (n_payload.saturating_sub(1)) as u64 * P2TR_OUT_VBYTES;
    ((TIER_VBYTES + extra) as f64 * fee_rate_sats_per_vb).ceil() as u64
}

/// Total value available to the `n_payload` children of a tier = parent value − committed fee − P2A.
/// `None` if the parent cannot carry the tier at all.
pub fn tier_out_total(prev_value: u64, n_payload: usize, fee_rate_sats_per_vb: f64) -> Option<u64> {
    // Checked on the anchor too — same saturating-`as u64` overflow as `tier_out_value`, same
    // attacker-controlled `fee_rate`, same panic.
    committed_fee_for_outputs(n_payload, fee_rate_sats_per_vb)
        .checked_add(P2A_VALUE)
        .and_then(|cost| prev_value.checked_sub(cost))
}

/// **SPLIT STATE `SP`** — the in-ladder split (PROTOCOL.md §5.4). Spends `X_m.out[0]` under
/// relative-timelock `csv_d`, paying `children` (exact amounts) plus the P2A anchor.
///
/// This addresses (does NOT fully dissolve) **B1**. An un-laddered split spends the coin's funding `F` — and
/// so does the trigger `T`, which every prior owner of a Model-A-conveyed coin retains, un-timelocked and
/// already co-signed. `SP` instead spends `X_m.out[0]`, so it **descends from `T` rather than racing it**:
/// a retained trigger can only start the clock on the current owner's own chain.
///
/// ⚠️ B1 is **RELOCATED, not dissolved** (split-child-bundle design review). The *trigger* stops being a
/// rival, but the parent's own retained STATE over `X_m.out[0]` becomes one. A child spending `SP.out[j]`
/// is only safe if it verifies the PARENT's full-disclosure census (parent num_sigs == parent's disclosed
/// tiers), which is only meaningful if the parent is terminalized AND that terminality is enforced at
/// co-sign time. See `docs/utexo/history/SPLIT-FINDINGS.md` — that census currently rests on server/enclave
/// guarantees that do not hold (the enclave has NO notion of terminality; sign/second must re-check the
/// gates — fixed 9d63f15). `build_trigger` is the ONLY builder that touches `f_txid/f_vout`.
///
/// Each child resting output then hosts its own extension+state tiers; no child needs its own trigger
/// because `SP` is itself un-broadcast (nothing ticks until it confirms).
///
/// `Σ children == tier_out_total(x_out_value, children.len(), fee_rate)` is REQUIRED — value
/// conservation is checked here rather than trusted, so a caller cannot silently mint or burn.
pub fn build_split_state(
    x_txid: &str,
    x_out_value: u64,
    children: &[(String, u64)],
    network: &str,
    csv_d: u16,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    build_split_state_from(
        x_txid,
        UNCOLORED_PAYLOAD_VOUT,
        x_out_value,
        children,
        network,
        csv_d,
        fee_rate,
    )
}

/// [CATS] [`build_split_state`] rooted at an ARBITRARY outpoint `(prev_txid, prev_vout)`.
///
/// **Why the vout cannot stay hard-coded.** `build_split_state` fixed its input vout at
/// [`UNCOLORED_PAYLOAD_VOUT`], which is right for the two shapes that existed when it was written —
/// `SP` over `X_m.out[0]` and `CSP` over `ext_child.out[0]`. It is WRONG for every spine batch after
/// the first: the sender's tip lives at `SP_i.out[K]` with `K >= 1` (it is the LAST payload output of
/// the previous batch), so batch *i+1* spends `(SP_i.txid, K)`.
///
/// Reusing the vout-0 builder there would not fail loudly. It would produce a transaction whose input
/// names `SP_i.out[0]` — a PAYEE's slot — while the co-sign it is about to be given commits, through
/// the taproot `SIGHASH_ALL` sighash, to that same wrong outpoint. The signature would be valid, the
/// transaction would be un-broadcastable forever, and the discovery would come AFTER
/// `set_spend_budget` had already terminalized the tip: the sender's own change, stranded by a
/// positional literal.
///
/// Same conservation law, same encoding, one extra parameter.
pub fn build_split_state_from(
    prev_txid: &str,
    prev_vout: u32,
    prev_out_value: u64,
    children: &[(String, u64)],
    network: &str,
    csv_d: u16,
    fee_rate: f64,
) -> Result<TierTx, MercuryError> {
    if children.is_empty() {
        return Err(MercuryError::FeeTooHigh);
    }
    let txid = Txid::from_str(prev_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    let available =
        tier_out_total(prev_out_value, children.len(), fee_rate).ok_or(MercuryError::FeeTooHigh)?;
    let total: u64 = children.iter().map(|(_, v)| *v).sum();
    if total != available {
        // Σout must equal Σin − fee_committed exactly (PROTOCOL.md §5.4).
        return Err(MercuryError::FeeTooHigh);
    }
    let mut output = Vec::with_capacity(children.len() + 1);
    for (address, value) in children {
        output.push(TxOut { value: *value, script_pubkey: spk_from_address(address, network)? });
    }
    output.push(TxOut { value: P2A_VALUE, script_pubkey: p2a_script() });
    let tx = Transaction {
        version: 3,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            // The split state spends the funding tier's PAYLOAD output — not a positional `0`. Child
            // `j` then lands at `payload_vout + j` (see the returned [`TierTx::payload_vout`]).
            previous_output: OutPoint { txid, vout: prev_vout },
            script_sig: ScriptBuf::new(),
            sequence: csv_blocks(csv_d),
            witness: Witness::default(),
        }],
        output,
    };
    encode(&tx, UNCOLORED_PAYLOAD_VOUT)
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
    encode(
        &build_tier_tx(txid, UNCOLORED_PAYLOAD_VOUT, TRIGGER_SEQUENCE, spk, out_value),
        UNCOLORED_PAYLOAD_VOUT,
    )
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

    /// A guaranteed-valid regtest P2TR address, derived rather than hardcoded (a bad bech32m literal
    /// would make these tests fail inside spk_from_address and prove nothing).
    fn test_addr() -> String {
        let xonly = bitcoin::secp256k1::XOnlyPublicKey::from_slice(
            &hex::decode("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798").unwrap(),
        )
        .unwrap();
        Address::p2tr(&bitcoin::secp256k1::Secp256k1::new(), xonly, None, bitcoin::Network::Regtest)
            .to_string()
    }

    #[test]
    fn split_state_conserves_value_and_scales_the_fee() {
        // A 3-output split state must: spend X.out[0] under the CSV, pay the exact children, anchor
        // with P2A last, stay v3, and pay a fee scaled to its REAL size (not the 2-output base).
        let x = "0000000000000000000000000000000000000000000000000000000000000001";
        let a = test_addr();
        let a = a.as_str();
        let (x_val, rate) = (200_000u64, 2.0);
        let avail = tier_out_total(x_val, 3, rate).unwrap();
        // Fee scales: 3 payload outputs cost 2 extra P2TR outputs vs the base tier.
        assert_eq!(
            committed_fee_for_outputs(3, rate),
            ((TIER_VBYTES + 2 * P2TR_OUT_VBYTES) as f64 * rate).ceil() as u64
        );
        assert!(committed_fee_for_outputs(3, rate) > committed_fee(rate), "fee must scale with outputs");
        assert_eq!(committed_fee_for_outputs(1, rate), committed_fee(rate), "1 payload == the base tier");

        let kids = vec![(a.to_string(), 1_000u64), (a.to_string(), 2_000u64), (a.to_string(), avail - 3_000)];
        let sp = build_split_state(x, x_val, &kids, "regtest", 18, rate).unwrap();
        let tx: Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&sp.tx_hex).unwrap()).unwrap();
        assert_eq!(tx.version, 3);
        assert_eq!(tx.input.len(), 1);
        assert_eq!(tx.input[0].previous_output.vout, 0, "SP spends X.out[0] — NOT the funding F [B1]");
        assert_eq!(tx.input[0].sequence, csv_blocks(18));
        assert_eq!(tx.output.len(), 4, "3 children + P2A");
        assert_eq!(tx.output[3].value, P2A_VALUE);
        assert_eq!(tx.output[3].script_pubkey, p2a_script(), "P2A anchors last");
        let paid: u64 = tx.output[..3].iter().map(|o| o.value).sum();
        assert_eq!(paid, avail, "Σout == Σin − fee_committed (no mint, no burn)");
        assert_eq!(paid + P2A_VALUE + committed_fee_for_outputs(3, rate), x_val);
    }

    /// **[CATS] The spine's split state roots at `SP_i.out[K]`, not `out[0]`.**
    ///
    /// The sender's tip is the LAST payload output of the previous batch, so every batch after the
    /// first spends a vout `>= 1`. Reusing the vout-0 builder there would not fail loudly: it would
    /// produce a transaction naming a PAYEE's slot as its input, and the taproot SIGHASH_ALL co-sign
    /// taken over it would commit to that same wrong outpoint — a valid signature over a transaction
    /// that can never be broadcast, discovered only after `set_spend_budget` had terminalized the tip.
    #[test]
    fn split_state_from_roots_at_the_named_outpoint() {
        let x = "0000000000000000000000000000000000000000000000000000000000000001";
        let a = test_addr();
        let (x_val, rate) = (200_000u64, 2.0);
        let avail = tier_out_total(x_val, 2, rate).unwrap();
        let kids = vec![(a.clone(), 1_000u64), (a.clone(), avail - 1_000)];

        for vout in [0u32, 1, 7] {
            let sp = build_split_state_from(x, vout, x_val, &kids, "regtest", 0, rate).unwrap();
            let tx: Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&sp.tx_hex).unwrap()).unwrap();
            assert_eq!(tx.input[0].previous_output.vout, vout, "the input must be the one asked for");
            assert_eq!(tx.input[0].sequence, csv_blocks(0), "spine tiers carry SPINE_CSV");
            let paid: u64 = tx.output[..2].iter().map(|o| o.value).sum();
            assert_eq!(paid, avail, "the conservation law is unchanged by the outpoint");
        }

        // The old entry point is exactly the vout-0 case, byte for byte — so nothing that already
        // spends `X_m.out[0]` or `ext_child.out[0]` changes shape, txid, or signature.
        assert_eq!(
            build_split_state(x, x_val, &kids, "regtest", 0, rate).unwrap().tx_hex,
            build_split_state_from(x, UNCOLORED_PAYLOAD_VOUT, x_val, &kids, "regtest", 0, rate)
                .unwrap()
                .tx_hex
        );
    }

    #[test]
    fn split_state_rejects_value_mismatch() {
        let x = "0000000000000000000000000000000000000000000000000000000000000001";
        let a = test_addr();
        let a = a.as_str();
        let avail = tier_out_total(200_000, 2, 2.0).unwrap();
        // Minting: Σ children exceeds what the parent carries.
        assert!(build_split_state(x, 200_000, &[(a.into(), avail), (a.into(), 1)], "regtest", 18, 2.0).is_err());
        // Burning: Σ children falls short (value would silently vanish to fee).
        assert!(build_split_state(x, 200_000, &[(a.into(), 10), (a.into(), 10)], "regtest", 18, 2.0).is_err());
        // Exact: accepted.
        assert!(build_split_state(x, 200_000, &[(a.into(), 10), (a.into(), avail - 10)], "regtest", 18, 2.0).is_ok());
        // No children at all.
        assert!(build_split_state(x, 200_000, &[], "regtest", 18, 2.0).is_err());
    }

    #[test]
    fn child_builders_root_at_the_assigned_split_output() {
        // A split child's ladder hangs off SP.out[j] for an ARBITRARY j — not out[0]. The child's first
        // extension must spend exactly (sp_txid, j); its state then spends the extension's out[0].
        let sp = "0000000000000000000000000000000000000000000000000000000000000abc";
        let a = test_addr();
        let (val, rate) = (150_000u64, 2.0);

        for j in [0u32, 1, 2, 7] {
            let ext = build_extension_from(sp, j, val, &a, "regtest", 12, rate).unwrap();
            let etx: Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&ext.tx_hex).unwrap()).unwrap();
            assert_eq!(etx.version, 3);
            assert_eq!(etx.input[0].previous_output.vout, j, "child extension roots at SP.out[j]");
            assert_eq!(etx.input[0].previous_output.txid.to_string(), sp);
            assert_eq!(etx.input[0].sequence, csv_blocks(12));
            assert_eq!(etx.output[1].value, P2A_VALUE, "P2A anchor present");

            // The child state spends the extension's out[0].
            let st = build_state_from(&ext.txid, 0, ext.out_value, &a, "regtest", 24, rate).unwrap();
            let stx: Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&st.tx_hex).unwrap()).unwrap();
            assert_eq!(stx.input[0].previous_output.txid.to_string(), ext.txid);
            assert_eq!(stx.input[0].previous_output.vout, 0);
            assert_eq!(stx.input[0].sequence, csv_blocks(24));
        }
    }

    #[test]
    fn encode_reads_the_payload_output_and_fails_closed_out_of_range() {
        // `encode` must read the PAYLOAD output, not `output[0]`. With the uncoloured shape the two
        // coincide; the point of the accessor is that they need not.
        let txid = Txid::from_str(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let tx = build_tier_tx(txid, UNCOLORED_PAYLOAD_VOUT, csv_blocks(24), p2a_script(), 50_000);
        let at0 = encode(&tx, 0).expect("payload 0 exists");
        assert_eq!(at0.out_value, 50_000);
        assert_eq!(at0.payload_vout, 0);
        // Index 1 is the P2A anchor — reading it as "the payload" yields the anchor value, which is
        // exactly the class of silent mis-read the accessor exists to make explicit.
        assert_eq!(encode(&tx, 1).unwrap().out_value, P2A_VALUE);
        // Out of range → fail CLOSED, never a panic and never a wrong output.
        assert!(matches!(encode(&tx, 2), Err(MercuryError::TransactionReconstructionError)));
        assert!(matches!(encode(&tx, 99), Err(MercuryError::TransactionReconstructionError)));
    }

    #[test]
    fn tier_builders_chain_through_the_payload_vout_accessor() {
        // Every builder must root its input at the PARENT's payload vout and report its OWN payload
        // vout, so a later colouring change is one constant rather than a dozen literals.
        let a = test_addr();
        let (f_value, rate) = (200_000u64, 2.0);
        let f = "0000000000000000000000000000000000000000000000000000000000000001";
        let t = build_trigger(f, 0, f_value, &a, "regtest", rate).unwrap();
        assert_eq!(t.payload_vout, UNCOLORED_PAYLOAD_VOUT);
        let x = build_extension(&t.txid, t.out_value, &a, "regtest", 12, rate).unwrap();
        assert_eq!(x.payload_vout, UNCOLORED_PAYLOAD_VOUT);
        let xtx: Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&x.tx_hex).unwrap()).unwrap();
        assert_eq!(
            xtx.input[0].previous_output.vout, t.payload_vout,
            "the extension spends the TRIGGER's payload output"
        );
        let s = build_state(&x.txid, x.out_value, &a, "regtest", 24, rate).unwrap();
        let stx: Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&s.tx_hex).unwrap()).unwrap();
        assert_eq!(
            stx.input[0].previous_output.vout, x.payload_vout,
            "the state spends the EXTENSION's payload output"
        );
        let de = build_detrigger(&t.txid, t.out_value, &a, "regtest", rate).unwrap();
        let dtx: Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&de.tx_hex).unwrap()).unwrap();
        assert_eq!(dtx.input[0].previous_output.vout, t.payload_vout, "de-trigger too");
        // The split state roots at the extension's payload vout and hosts child j at payload_vout+j.
        let avail = tier_out_total(x.out_value, 2, rate).unwrap();
        let sp = build_split_state(
            &x.txid,
            x.out_value,
            &[(a.clone(), 1_000), (a.clone(), avail - 1_000)],
            "regtest",
            18,
            rate,
        )
        .unwrap();
        let sptx: Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&sp.tx_hex).unwrap()).unwrap();
        assert_eq!(sptx.input[0].previous_output.vout, x.payload_vout, "SP spends X's payload output");
        assert_eq!(sp.payload_vout, UNCOLORED_PAYLOAD_VOUT);
        assert_eq!(sptx.output[sp.payload_vout as usize].value, 1_000, "child 0 at payload_vout");
        assert_eq!(sptx.output[sp.payload_vout as usize + 1].value, avail - 1_000, "child 1 at +1");
    }

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
        // 100_000 sats, 2 sat/vB → fee 250 (125 vB, [D4]-corrected — was 248 on the 124-vB
        // SIGHASH_DEFAULT model), anchor 240 → 99_510 forward.
        assert_eq!(committed_fee(2.0), 250);
        assert_eq!(tier_out_value(100_000, 2.0), Some(100_000 - 250 - 240));
        // Too small → terminal dust case.
        assert_eq!(tier_out_value(300, 2.0), None);
    }

    /// **[D4] The model and the transaction must agree — proved by MEASURING.**
    ///
    /// The committed fee exists for exactly one property: a pre-signed tier relays and confirms
    /// STANDALONE, with no P2A child attached. That property is `committed_fee / real_vsize >= rate`,
    /// so it is worth nothing unless `real_vsize` is the size the transaction actually has once
    /// signed. This test therefore builds the real tier shape, hands it to the **production**
    /// finaliser ([`crate::transaction::new_backup_transaction`] — the same function that finalises
    /// every co-signed tier), measures `Transaction::vsize()`, and requires the model to match it
    /// exactly, at every arity a split state can reach.
    ///
    /// Counterfactual: restore `TIER_VBYTES = 124` and this fails at n=1 with `124 != 125` — the old
    /// constant sized a 124-vB transaction that measures 125 vB and would have relayed at
    /// 1.984 sat/vB.
    #[test]
    fn the_uncoloured_fee_matches_a_measured_signed_tier() {
        /// One TES-R taproot key-spend signature: 64 Schnorr bytes + the explicit `SIGHASH_ALL` byte.
        /// Not 64 — that width is `SIGHASH_DEFAULT` only, and assuming it is [D4].
        const TAPROOT_SIGHASH_ALL_WITNESS_BYTES: usize = 65;
        let rate = 2.0;
        let a = test_addr();
        let f = "0000000000000000000000000000000000000000000000000000000000000001";

        for n_payload in 1..=4usize {
            // The real shape, from the real builders: n=1 is a plain tier; n>1 is a split state.
            let unsigned_hex = if n_payload == 1 {
                build_trigger(f, 0, 1_000_000, &a, "regtest", rate).unwrap().tx_hex
            } else {
                let avail = tier_out_total(1_000_000, n_payload, rate).unwrap();
                let mut kids: Vec<(String, u64)> =
                    (1..n_payload).map(|i| (a.clone(), 10_000 * i as u64)).collect();
                let rest: u64 = kids.iter().map(|(_, v)| *v).sum();
                kids.push((a.clone(), avail - rest));
                build_split_state(f, 1_000_000, &kids, "regtest", 18, rate).unwrap().tx_hex
            };

            // MEASURE: through the production finaliser, not a hand-rolled witness. Schnorr
            // signatures are fixed-width, so any 64 bytes parse; only the SERIALISED width matters to
            // vsize, and that is decided by the finaliser's `hash_ty`, not by these bytes.
            let signed_hex =
                crate::transaction::new_backup_transaction(unsigned_hex, "01".repeat(64))
                    .expect("the production finaliser must accept a tier-shaped transaction");
            let signed: Transaction =
                bitcoin::consensus::encode::deserialize(&hex::decode(&signed_hex).unwrap()).unwrap();
            assert_eq!(
                signed.input[0].witness.iter().next().unwrap().len(),
                TAPROOT_SIGHASH_ALL_WITNESS_BYTES,
                "TES-R signs SIGHASH_ALL, so the witness item is 64 sig bytes + 1 sighash byte"
            );
            let measured = signed.vsize() as u64;

            // The model must EQUAL the transaction — not bound it.
            let modelled = TIER_VBYTES + (n_payload as u64 - 1) * P2TR_OUT_VBYTES;
            assert_eq!(
                modelled, measured,
                "n={n_payload}: the vsize model must equal the finalised transaction"
            );
            // The property the fee is FOR: standalone relay at the target rate, no anchor.
            let fee = committed_fee_for_outputs(n_payload, rate);
            assert!(
                fee >= (measured as f64 * rate).ceil() as u64,
                "n={n_payload}: {fee} sat over {measured} vB = {:.3} sat/vB < {rate}",
                fee as f64 / measured as f64
            );
            // At an integral rate the fee is exact — no silent over-payment either.
            assert_eq!(fee, measured * 2, "n={n_payload}: an uncoloured tier must pay EXACTLY {rate}");
        }
    }

    /// **The CTES-R surcharge identity, pinned at its uncoloured end.**
    ///
    /// A coloured tier is an uncoloured tier plus ONE `opret` output — nothing else. So
    /// `COLORED_TIER_VBYTES − TIER_VBYTES` must be exactly [`P2TR_OUT_VBYTES`] (43), and the fee
    /// surcharge exactly `43 · rate`. `SDK_E2E=74` asserts this end to end on a live coloured ladder;
    /// this is the constant-level half, so a future move of either constant alone fails here first.
    ///
    /// `mercuryrustlib` cannot be depended on from `mercurylib` (it depends on us), so the coloured
    /// number is restated as a literal — with the guard that it is the ONLY literal, and the identity
    /// is what is checked.
    #[test]
    fn the_coloured_surcharge_is_exactly_one_opret_output() {
        // `mercuryrustlib::rgb::COLORED_TIER_VBYTES`, MEASURED on a production-finalised coloured
        // tier by `rgb::ctesr_tests::the_coloured_fee_matches_a_measured_signed_tier`.
        const COLORED_TIER_VBYTES: u64 = 168;
        assert_eq!(
            COLORED_TIER_VBYTES - TIER_VBYTES,
            P2TR_OUT_VBYTES,
            "the coloured tier is the uncoloured tier plus exactly one opret output — if this fails, \
             one of the two vsize models moved without the other (that is [D4], and it is what made \
             SDK_E2E=74 red)"
        );
        for rate in [1.0f64, 2.0, 5.0, 10.0] {
            let colored = (COLORED_TIER_VBYTES as f64 * rate).ceil() as u64;
            assert_eq!(
                colored - committed_fee(rate),
                (P2TR_OUT_VBYTES as f64 * rate).ceil() as u64,
                "rate {rate}: the coloured surcharge must be exactly the opret's 43 vB"
            );
        }
        // The `SDK_E2E=74` assertion verbatim, at the committed default rate.
        assert_eq!(committed_fee(2.0), 250);
        assert_eq!((COLORED_TIER_VBYTES as f64 * 2.0).ceil() as u64 - committed_fee(2.0), 43 * 2);
        // And the coloured fee is the uncoloured fee for one MORE output — exactly, no +1 vB slack.
        for n in 1..=4usize {
            let colored_n =
                ((COLORED_TIER_VBYTES + (n as u64 - 1) * P2TR_OUT_VBYTES) as f64 * 2.0).ceil() as u64;
            assert_eq!(
                colored_n,
                committed_fee_for_outputs(n + 1, 2.0),
                "n={n}: colored_committed_fee(n) == committed_fee_for_outputs(n + 1)"
            );
        }
    }

    /// Every floor that is DERIVED from the tier vsize moves with it — no floor may keep a literal.
    #[test]
    fn derived_floors_track_the_corrected_tier_vbytes() {
        let (rate, dust) = (2.0f64, 330u64);
        // A child of an in-ladder split funds TWO rungs (extension + state) and must still clear dust.
        assert_eq!(min_child_value(rate, dust), 2 * (250 + P2A_VALUE) + dust);
        assert_eq!(min_child_value(rate, dust), 1_310, "was 1_306 on the 124-vB model");
        // Derived, never a literal: re-deriving from the constant must reproduce it.
        assert_eq!(
            min_child_value(rate, dust),
            2 * ((TIER_VBYTES as f64 * rate).ceil() as u64 + P2A_VALUE) + dust
        );
        // A tier's forward value drops by the same 2 sat per rung; three rungs cost 6 more sat.
        let plain_rung = committed_fee(rate) + P2A_VALUE;
        assert_eq!(tier_out_value(1_000_000, rate), Some(1_000_000 - plain_rung));
        assert_eq!(tier_out_total(1_000_000, 1, rate), tier_out_value(1_000_000, rate));
        // [CATS/V5] …and the SPINE TIP funds exactly ONE rung, so it is one rung cheaper. Both
        // numbers are derived from `TIER_VBYTES`, not written down.
        assert_eq!(min_spine_tip_value(rate, dust), plain_rung + dust);
        assert_eq!(min_spine_tip_value(rate, dust), 820);
        assert_eq!(
            min_child_value(rate, dust) - min_spine_tip_value(rate, dust),
            plain_rung,
            "the tip's saving is exactly the extension rung it does not build"
        );
    }

    /// **[V5] The two floors are NOT interchangeable, and the cheap one is the dangerous one.**
    ///
    /// `min_spine_tip_value` is 490 sat below `min_child_value` at the default rate. Applied to a
    /// PAYEE's piece it admits a value that cannot fund the piece's second rung — and the failure
    /// lands inside `establish_child`, after `set_spend_budget` has already terminalized the parent.
    /// So the ordering is asserted here rather than left as a comment: any future edit that lets the
    /// tip floor rise to or above the child floor (or the child floor fall to the tip's) breaks a
    /// test instead of a coin.
    #[test]
    fn the_spine_tip_floor_is_strictly_below_the_child_floor_at_every_shipped_rate() {
        for rate in [1.0f64, 2.0, 5.0, 10.0, 50.0] {
            let dust = 330u64;
            assert!(
                min_spine_tip_value(rate, dust) < min_child_value(rate, dust),
                "rate {rate}: a one-rung floor must be cheaper than a two-rung floor"
            );
            // …and it is still above bare dust: a tip that cannot pay for its own cap is not a coin.
            assert!(min_spine_tip_value(rate, dust) > dust);
        }
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
    fn spk_resolves_plain_and_mercury_addresses() {
        // Plain bech32m P2TR → itself (34-byte OP_1 <32>).
        let plain = spk_from_address(
            "bcrt1p83afnxgnczlsqvd20swjlnr3kcm7hvz9p338dgueetjz2tx6vvjs05rsfy",
            "regtest",
        )
        .expect("plain address resolves");
        assert_eq!(plain.as_bytes().len(), 34);
        assert_eq!(plain.as_bytes()[0], 0x51);
        assert_eq!(plain.as_bytes()[1], 0x20);

        // Mercury transfer address → the recipient's DERIVED P2TR (Model A payee), matching
        // create_tx_out exactly. A real regtest transfer address (from sdk01).
        let mercury = "tml1qqp65hkjf3rq03fypj26nuvr6j8hr5gjktqdwkvkkamkl7kj5pa0ffgz7ckag5keskjjjdc9l6s9n9sj5ntrrx20umsuh5manvcc0gczlh5qgquuch";
        let via_helper = spk_from_address(mercury, "regtest").expect("mercury address resolves");
        let (_, upk, _) = crate::decode_transfer_address(mercury).unwrap();
        let expected = Address::p2tr(
            &secp256k1_zkp::Secp256k1::new(),
            upk.x_only_public_key().0,
            None,
            bitcoin::Network::Regtest,
        )
        .script_pubkey();
        assert_eq!(via_helper, expected, "mercury address resolves to P2TR(recipient_user_pubkey)");
        assert_eq!(via_helper.as_bytes().len(), 34, "and it is a P2TR output");
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
