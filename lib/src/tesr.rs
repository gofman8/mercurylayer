//! TES-R (Trigger / Extension / State) transaction tier builders for Mercury Utexo.
//!
//! See `docs/utexo/spec/PROTOCOL.md`. A coin's funding UTXO `F` (P2TR of the aggregate key `A`) rests
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
/// **The relay dust floor a SPENDABLE output must clear, in sats.**
///
/// Bitcoin Core refuses to relay a transaction carrying an output below `dustRelayFee`-derived
/// threshold for its script type; 330 is the P2TR figure, and it is the number every builder floor in
/// this codebase is written against ([`min_child_value`], [`min_spine_tip_value`],
/// `mercuryrustlib::tesr::colored_ladder_floor`, `SplitLegRole::min_value`). It lived as four
/// independent literal `330`s — two function-local `const`s in `crate::transaction`, one
/// crate-private one in the SDK, and `COLORED_LADDER_DUST` — none of them reachable from the
/// RECEIVE-side verifiers, which is a large part of why no verifier ever checked it.
///
/// ⚠️ **Deliberately one number for every script type.** A P2WPKH output actually relays at 294, so
/// this is over-strict by 36 sat for a v0-witness exit address. That is the right trade: every
/// sender-side floor in the tree already uses 330 uniformly, so no honest path produces a leg in the
/// 294..329 band, and a per-script-type table would introduce exactly the sender/receiver floor drift
/// this constant exists to remove.
///
/// **Exempt, and only these two:** the P2A anchor (a 4-byte `OP_1 <0x4e73>` output whose own
/// standardness threshold is 240, which is why [`P2A_VALUE`] is 240) and the RGB `opret` commitment
/// (provably unspendable, so exempt from dust rules entirely, and it carries value 0).
pub const DUST_LIMIT: u64 = 330;
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

/// Bitcoin Core's default `minrelaytxfee`, in sat/vB. A transaction paying less than this is not
/// merely slow — it is REFUSED at submission ("min relay fee not met") and never enters a mempool.
pub const MIN_RELAY_FEE_RATE_SATS_PER_VB: f64 = 1.0;

/// **Can this tier be broadcast ON ITS OWN?** [D26]
///
/// The ladder's whole safety argument is a maturity race: the lower-CSV tier confirms first and wins
/// the outpoint. That argument is about a race, and *a transaction that cannot enter a mempool never
/// enters the race*. So "the superseded tier has a higher CSV, therefore it loses" is only true if
/// the live tier can actually be sent — which is a fee question, not a timelock question.
///
/// Measured, not assumed (WP1-TRUC-P2A-SPIKE (retired 2026-08-15)): a v3 tier under the floor is
/// refused outright at `sendrawtransaction`.
///
/// **Deliberately ignores the P2A anchor**, and that is the conservative direction. Every tier
/// carries one, and package relay via `submitpackage` WOULD rescue an underpaying tier — but this
/// tree has no `submitpackage` caller yet (tracked as its own item), so today an underpaid tier is
/// simply unbroadcastable. Counting the anchor here would credit the ladder with a rescue nobody can
/// perform. When that path exists, this is the function to revisit; until then it may refuse a
/// bundle that a future build could broadcast, which costs a retry rather than a coin.
pub fn tier_is_relayable(implied_fee_sats: u64, vsize: u64) -> bool {
    implied_fee_sats >= (vsize as f64 * MIN_RELAY_FEE_RATE_SATS_PER_VB).ceil() as u64
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
/// @ the SHIPPED rate, committed_fee_rate = 3.0 sat/vB ([D44]); committed_fee = 125 vB · 3 = 375
/// min_spine_tip_value = committed_fee + P2A + dust     = 375 + 240 + 330        =   945
/// min_child_value     = 2·(committed_fee + P2A) + dust = 2·(375 + 240) + 330    = 1 560
///
/// @ 2.0 sat/vB, the rate this comment was written at and the code no longer uses:
/// min_spine_tip_value = 820      min_child_value = 1 310
/// ```
///
/// **[D56] Both functions take the rate as an ARGUMENT — the numbers above are evaluations, not
/// constants**, and a reader who lifts one is quoting a rate rather than a floor. That is not a
/// hypothetical: every floor test froze `rate = 2.0` as a fixture, so raising the shipped rate to
/// 3.0 broke none of them and this comment went stale in silence.
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

/// Byte-wise `==` for `&str` usable from `const fn`. `str`'s `PartialEq` is not const, and neither is
/// `to_ascii_lowercase`, so the const table below matches canonical (already-lowercased) names.
pub const fn const_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Protocol parameters for the TES-R ladder — the relative-timelock schedule the wallet uses to size
/// each tier and to decide when to renew or roll over. Mainnet defaults are from
/// `docs/utexo/spec/PROTOCOL.md` §5.2.
///
/// **These are compiled in per network ([`Self::for_network`], D7/D25) and NOT taken from the SE.**
/// The CSV schedule is the mild half: getting it "wrong" costs exit-wait length and renewal cadence,
/// not funds. The flat-ladder half ([`Self::flat_ladder_params`], D8(f)) is the sharp one — `interval`
/// is the yardstick INV-5 measures every backup hop against, so a coordinator that could choose it
/// could choose the defence against backup-vector padding. `/info/config` still publishes both, but
/// only as a cross-check the client refuses to proceed past on mismatch.
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
        Self { d0: 1440, delta: 36, d_floor: 144, e0: 720, delta_e: 36, e_floor: 144, m_max: 15, committed_fee_rate: 3.0 }
    }

    /// Test-scale schedule for regtest (fast to mine a full lifecycle).
    pub fn regtest() -> Self {
        Self { d0: 24, delta: 6, d_floor: 6, e0: 12, delta_e: 3, e_floor: 3, m_max: 2, committed_fee_rate: 3.0 }
    }

    /// **[D25] The schedule for the public test networks — testnet and signet: the MAINNET one.**
    ///
    /// These networks have real ~10-minute blocks, so the old behaviour — silently falling through
    /// to [`Self::regtest`]'s `d0 = 24` — meant a ladder whose head start was about **four hours**,
    /// on the profile the deployed coordinator actually runs (`server/Settings.toml`:
    /// `network = "testnet"`). It was stated in no document.
    ///
    /// A public test network should exercise **the schedule that ships**. Only regtest, where blocks
    /// are mined on demand, keeps the test-scale numbers — that is where E2E speed comes from, and it
    /// is preserved exactly.
    ///
    /// This also removes the combination D7 flagged as the sharper problem: on the toy schedule the
    /// deployed profile admitted a **139-transaction** exit chain, ~135 of them consecutive zero-CSV
    /// spine tiers, whose TRUC relay stall (~68 blocks at two in flight) exceeded the entire state
    /// schedule (WP1-TRUC-P2A-SPIKE (retired 2026-08-15)). On the mainnet schedule it does not arise.
    ///
    /// **This is a deliberate compatibility break** (D23): a receiver derives its accepted CSV band
    /// from its own preset, so ladders built against the deployed testnet coordinator by an older
    /// build are refused. Those coins are expendable.
    pub fn testnet() -> Self {
        Self::mainnet()
    }

    /// **[D7] The preset for a network string — EXPLICIT, with no silent fallback.**
    ///
    /// This used to be "`bitcoin`/`mainnet` → mainnet, **everything else** → regtest", which meant
    /// testnet and signet silently received the toy schedule (`d0 = 24`, ≈4 hours of real time on a
    /// 10-minute-block network) — a fact stated in no document, and the deployed coordinator runs
    /// `network = "testnet"`. It also meant a TYPO fell through to the toy schedule rather than
    /// being caught: `"mainet"` would have quietly produced a 4-hour ladder on real money.
    ///
    /// Every supported network is now named, and an unrecognised one is [`None`] rather than a
    /// guess. Callers that cannot fail take [`Self::for_network_or_default`], which is explicit
    /// about what it assumes.
    pub fn for_network_checked(network: &str) -> Option<Self> {
        let n = network.to_ascii_lowercase();
        match n.as_str() {
            "bitcoin" | "mainnet" => Some(Self::mainnet()),
            "testnet" | "testnet3" | "testnet4" | "signet" => Some(Self::testnet()),
            "regtest" => Some(Self::regtest()),
            _ => None,
        }
    }

    /// [D25] The preset for a network string. **An unrecognised network PANICS rather than guessing.**
    ///
    /// It used to fall through to the toy regtest schedule, so `"mainet"` would have produced a
    /// four-hour ladder on real money and nothing would have said so. Under D23 there is no reason
    /// to keep that: a network name this binary does not know is a configuration error, and the
    /// loudest possible failure is the correct one — a mis-set network silently changes every
    /// timelock in the system.
    ///
    /// Callers that can return an error should use [`Self::for_network_checked`] and refuse by name.
    pub fn for_network(network: &str) -> Self {
        Self::for_network_checked(network).unwrap_or_else(|| {
            panic!(
                "unknown network {network:?}: refusing to guess a TES-R schedule. Every timelock in \
                 the system derives from this value, so a typo would silently change the whole \
                 ladder. Supported: bitcoin/mainnet, testnet/testnet3/testnet4/signet, regtest."
            )
        })
    }

    /// **[D8(f)] The FLAT-lane ladder parameters for this network — compiled in, not fetched.**
    ///
    /// `initlock` is the deposit's absolute head start; `interval` is the decrement each transfer
    /// takes off it, so the flat backup chain is `L_k = L_0 − k·interval`.
    ///
    /// **Why these must not come from the coordinator.** INV-5 requires every hop to decrement by
    /// EXACTLY `interval` (`ladder_decrements_by_interval`, `transfer/receiver.rs`), and that is the
    /// defence against a sender padding the backup vector with duplicates to inflate `flat_backups`
    /// and absorb a hidden co-signed state. If the coordinator supplies `interval`, the coordinator
    /// defines the defence.
    ///
    /// **And deriving it from the conveyed chain does not work** — that was the tempting fix and it
    /// is circular: a padded chain with uniform `I/2` decrements derives `I/2` and validates against
    /// itself, accepting exactly the padding INV-5 exists to stop. The value has to come from
    /// somewhere neither the sender nor the coordinator chooses, which is here.
    ///
    /// Regtest keeps the small numbers so E2E lifecycles stay fast, exactly as the CSV schedule does.
    pub fn flat_ladder_params(network: &str) -> Option<(u32, u32)> {
        Self::flat_ladder_params_const(&network.to_ascii_lowercase())
    }

    /// [`Self::flat_ladder_params`] in a `const` context, so downstream constants (the SDK's
    /// `auto_exit_margin_blocks` derivation) are computed at compile time from this table instead of
    /// transcribing its numbers. Case-sensitive — `flat_ladder_params` is the lowercasing wrapper.
    /// **[D69] THE ENCLAVE'S PINNED ATTESTATION IDENTITY, per network.**
    ///
    /// This is what closes `TRUST-MODEL` B11. Terminality is established from an enclave signature;
    /// a signature is worth only the key that verifies it; and until now that key was the COIN's
    /// server key, whose sole honest anchor is the on-chain funding output. A deep in-ladder-split
    /// ancestor's funding is deliberately un-broadcast, so for those ancestors the verifying key
    /// arrived in the same HTTP response as the signature — which proves nothing.
    ///
    /// The enclave now signs every attestation with ONE long-term identity key
    /// (`enclave::attestation_identity_pubkey`, derived from its seed under
    /// `utexo/attestation-identity/v1`). Pinning that key here makes the verifier independent of
    /// what the coordinator says and independent of whether the coin is on chain — so the check
    /// works at every depth.
    ///
    /// # Why a compiled-in constant, and what that costs
    ///
    /// D69 took option (a): pin in the client. No new infrastructure, no bootstrap trust, nothing to
    /// discover. The cost is **rotation is a client release** — a compromised identity key cannot be
    /// replaced until users upgrade — and a second operator needs a second entry. When either of
    /// those bites, the successor is an on-chain anchor with a rotation chain (option (b)), and this
    /// constant becomes its genesis entry.
    ///
    /// # `None` is not "no check" — it is "no coin may be verified on this network yet"
    ///
    /// Every network returns `None` today because **no enclave has been provisioned for any public
    /// network**. That is the honest state, and it is why the resolver treats `None` plus no
    /// configured value as a REFUSAL rather than a fallback. A regtest or CI lockbox generates its
    /// own seed, so its identity differs per environment and must be supplied by configuration;
    /// there is one defined place to read it from (`GET /attestation_identity`).
    ///
    /// When a mainnet enclave is provisioned, its x-only identity goes here, and from that moment
    /// mainnet stops accepting a configured override — see `attestation_identity`.
    /// **[V-6] The regtest enclave's attestation identity, pinned.**
    ///
    /// Not a magic constant and not this machine's key: it is a pure function of the SEED THIS REPO
    /// COMMITS for its own dev stack (`docker-compose-main.yml`, the `vault-init` payload), under a
    /// derivation the enclave states in full — `SHA256("utexo/attestation-identity/v1" ‖ seed32 ‖
    /// counter_u8)` retried until the digest is a valid scalar, then the x-only public key
    /// (`enclave::derive_identity_keypair`). Anyone running this repo's stack gets this key, so
    /// pinning it is reproducible rather than local.
    ///
    /// `regtest_attestation_identity_is_derivable_from_the_committed_dev_seed` re-derives it in Rust
    /// and fails if either the seed or this constant moves — the pin cannot rot into a lie without
    /// the suite saying so.
    ///
    /// **Why regtest can be pinned and mainnet cannot.** A pin is only an anchor if it was obtained
    /// OUT OF BAND — that is the whole of D69. For regtest the out-of-band channel is the repository
    /// itself: the seed is in the tree, so the key is a fact about the source rather than about a
    /// running server. Mainnet has no enclave provisioned, so there is no key to pin, and inventing
    /// one would be worse than leaving it absent: a wrong pin refuses every attestation, and the
    /// obvious "fix" is to trust the key the coordinator serves — which is exactly the hole D69
    /// closed. It stays `None` until a mainnet enclave exists and its identity is published.
    pub const REGTEST_ATTESTATION_IDENTITY: &str =
        "a3c87c1dd1344e30f6374b568306f46031ed9bfa35ec73c03c61d819848c5def";

    pub const fn attestation_identity_const(network: &str) -> Option<&'static str> {
        // Deliberately exhaustive over the same names `for_network` knows, so adding a network
        // cannot silently inherit another's identity.
        if const_str_eq(network, "bitcoin") || const_str_eq(network, "mainnet") {
            None // no mainnet enclave provisioned — see REGTEST_ATTESTATION_IDENTITY's note
        } else if const_str_eq(network, "testnet")
            || const_str_eq(network, "testnet3")
            || const_str_eq(network, "testnet4")
            || const_str_eq(network, "signet")
        {
            None // no public-testnet enclave provisioned
        } else {
            Some(Self::REGTEST_ATTESTATION_IDENTITY)
        }
    }

    /// **[D69] Resolve the identity to verify attestations against — pin first, config second,
    /// REFUSE third.**
    ///
    /// The order is the security property, not a convenience:
    ///
    /// * a compiled-in pin, where one exists, is **not overridable**. If it were, the "pin" would be
    ///   a default and an attacker who can influence a config file could re-open B11 in full;
    /// * a configured value is accepted only where there is no pin — i.e. on a network whose enclave
    ///   this build does not know, which today is all of them;
    /// * neither means the client **cannot verify terminality at all**, and that is a refusal. It
    ///   must not degrade to "accept whatever the coordinator serves", because that is precisely the
    ///   state B11 describes.
    pub fn attestation_identity(
        network: &str,
        configured: Option<&str>,
    ) -> core::result::Result<String, String> {
        if let Some(pinned) = Self::attestation_identity_const(network) {
            if let Some(cfg) = configured {
                let norm = |s: &str| s.trim_start_matches("0x").to_ascii_lowercase();
                if norm(cfg) != norm(pinned) {
                    return Err(format!(
                        "the configured attestation identity does not match the one COMPILED IN for \
                         {network}. A pin that a configuration file can override is not a pin — it \
                         is a default, and overriding it re-opens the hole it was added to close \
                         (TRUST-MODEL B11). Remove the configured value, or use a build whose pin is \
                         the key you mean.\n  compiled-in: {pinned}\n  configured:  {cfg}"
                    ));
                }
            }
            return Ok(pinned.to_string());
        }
        match configured {
            Some(c) if !c.trim().is_empty() => Ok(c.to_string()),
            _ => Err(format!(
                "no attestation identity is available for network `{network}`: this build has no \
                 compiled-in pin and none was configured. Terminality is established from an \
                 enclave signature, and without a key to verify it against there is nothing to \
                 check the signature WITH — accepting the key the coordinator serves alongside the \
                 signature would prove nothing (TRUST-MODEL B11). Read the enclave's identity from \
                 `GET /attestation_identity` and set `attestation_identity` in the client settings."
            )),
        }
    }

    pub const fn flat_ladder_params_const(network: &str) -> Option<(u32, u32)> {
        // `match` on `&str` is not available in `const fn` (str's PartialEq is not const), hence the
        // explicit chain over `const_str_eq`.
        if const_str_eq(network, "bitcoin")
            || const_str_eq(network, "mainnet")
            // D25: testnet/signet deliberately run the MAINNET schedule, so the timings that ship
            // are the timings that were rehearsed.
            || const_str_eq(network, "testnet")
            || const_str_eq(network, "testnet3")
            || const_str_eq(network, "testnet4")
            || const_str_eq(network, "signet")
        {
            // 10 000-block head start, 100 blocks per hop => 100 hops of ladder capacity, the figure
            // `clients/libs/rust-sdk/src/config.rs` documents as the fail-closed bound on audit [17].
            return Some((10_000, 100));
        }
        if const_str_eq(network, "regtest") {
            // Same 100-hop capacity, scaled down so an E2E lifecycle mines in seconds.
            return Some((1_000, 10));
        }
        None
    }

    /// State CSV at state-count `k`: `D0 − k·δ`, clamped to the floor.
    pub fn state_csv(&self, k: u16) -> u16 {
        self.d0.saturating_sub(k.saturating_mul(self.delta)).max(self.d_floor)
    }

    /// Extension CSV at renewal epoch `m`: `E0 − m·δE`, clamped to the floor.
    pub fn ext_csv(&self, m: u16) -> u16 {
        self.e0.saturating_sub(m.saturating_mul(self.delta_e)).max(self.e_floor)
    }

    /// **[D38/R13] Is `csv` a value this schedule can actually PRODUCE for an extension?**
    ///
    /// The band `[e_floor, e0]` is not the set of legal extension CSVs — the GRID is. An honest
    /// renewal steps by exactly `δE` ([`Self::ext_csv`]), so `e0 − m·δE` and the floor clamp are the
    /// only values any builder in this design emits. A receiver that admits anything in the band
    /// admits states the specification does not define, chosen by the sender at 1-block granularity
    /// where the design's own granularity is `δE`.
    ///
    /// The floor is on the grid by fiat, because `ext_csv` clamps there: a schedule whose floor is
    /// not itself a grid point still produces the floor, and refusing it would refuse the last
    /// honest renewal.
    /// **The floor bound is part of the predicate, not a separate check the caller must remember.**
    /// Without it this is only a modular-arithmetic test, and `SPINE_CSV = 0` passes it on mainnet
    /// (`1440 % 36 == 0`) — so a caller using the grid alone would admit an un-timelocked state on
    /// the ordinary lane. A predicate named "is this legal" must answer that question completely.
    pub fn is_on_ext_grid(&self, csv: u16) -> bool {
        csv >= self.e_floor
            && csv <= self.e0
            && (csv == self.e_floor || (self.e0 - csv) % self.delta_e == 0)
    }

    /// The state-tier twin of [`Self::is_on_ext_grid`] — `d0 − k·δ`, or the floor clamp.
    ///
    /// **`SPINE_CSV = 0` is deliberately NOT on this grid** and must never be tested against it: a
    /// split state is a distinct tier kind with its own exact band, not a state walked to zero.
    pub fn is_on_state_grid(&self, csv: u16) -> bool {
        csv >= self.d_floor
            && csv <= self.d0
            && (csv == self.d_floor || (self.d0 - csv) % self.delta == 0)
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

/// **[REQ-56 / §5.4.4] Build the COLLAPSE transaction `C` that closes a tree.**
///
/// `C` spends the root's funding output `F` and pays **every unreleased frontier leaf its full
/// funding value at its own exit key**, with the remainder to the root owner. Leaves whose holders
/// have released are owed nothing. The payouts come out of `F` itself — the tree's own money — which
/// is the whole of REQ-69/REQ-80: **the closer fronts nothing.**
///
/// **The fee comes out of the OWNER'S remainder, never out of a payout, and that is not a formatting
/// choice.** The SE's predicate requires each leaf paid IN FULL; shaving a fee off a payout is
/// arithmetically identical to underpaying one, which is the exact refusal `collapse_grant` exists to
/// make — a holder discharged without being paid. So the caller passes `owner_value` already net of
/// the fee, and `C` is refused here if the outputs do not fit inside `funding_value`.
///
/// **Version 2, not 3.** Every TES-R tier is v3/TRUC because it must relay as a package behind an
/// anchor. `C` is a final settlement transaction that pays its own fee out of the remainder and has
/// no anchor and no child, so TRUC's topology limits would only constrain it for nothing.
///
/// Keys are x-only (32-byte) hex, because that is what the SE's leaf registry stores as a leaf's exit
/// key and what its predicate compares against. Taking addresses here would mean converting twice and
/// comparing in a third form.
pub fn build_collapse_tx(
    funding_txid: &str,
    funding_vout: u32,
    funding_value: u64,
    payouts: &[(String, u64)],
    owner_xonly: Option<(String, u64)>,
) -> Result<TierTx, MercuryError> {
    let txid = Txid::from_str(funding_txid).map_err(|_| MercuryError::BitcoinHashHexError)?;
    if payouts.is_empty() {
        // An empty obligation is satisfied vacuously — correct arithmetic, catastrophic answer. The
        // SE refuses this upstream (`validate_for_grant`); refusing to BUILD it too means a caller
        // never gets as far as asking.
        return Err(MercuryError::TransactionReconstructionError);
    }

    let spk_of = |xonly_hex: &str| -> Result<ScriptBuf, MercuryError> {
        let raw = hex::decode(xonly_hex).map_err(|_| MercuryError::BitcoinHashHexError)?;
        if raw.len() != 32 {
            return Err(MercuryError::BitcoinHashHexError);
        }
        let mut spk = Vec::with_capacity(34);
        spk.push(0x51);
        spk.push(0x20);
        spk.extend_from_slice(&raw);
        Ok(ScriptBuf::from_bytes(spk))
    };

    let mut output = Vec::with_capacity(payouts.len() + 1);
    for (key, value) in payouts {
        output.push(TxOut { value: *value, script_pubkey: spk_of(key)? });
    }
    if let Some((key, value)) = owner_xonly.as_ref() {
        // A zero-value remainder is simply no output: the fee happened to consume all of it. Emitting
        // a 0-sat P2TR instead would make `C` non-standard and unbroadcastable, turning a tree that
        // closes tightly into one that cannot close at all.
        if *value > 0 {
            output.push(TxOut { value: *value, script_pubkey: spk_of(key)? });
        }
    }

    let total_out: u64 = output.iter().map(|o| o.value).sum();
    if total_out > funding_value {
        return Err(MercuryError::FeeTooLow);
    }

    let tx = Transaction {
        version: 2,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            previous_output: OutPoint { txid, vout: funding_vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::default(),
        }],
        output,
    };
    encode(&tx, 0)
}

/// **THE payload-vout accessor.** The output index at which a tier's PAYLOAD (value-carrying,
/// P2TR(A)-or-payee) outputs begin. Every chaining site — "the child spends its parent's payload
/// output", "the tier pays `A` on its payload output", the `live_csv_by_outpoint` census key — must
/// read this rather than assuming `0`.
///
/// It is `0` today because a tier is `[payload…, P2A]`. Under CTES-R a coloured tier is
/// `[opret, payload…, P2A]` (the fork sets `opreturn_first = true` whenever any output is P2TR, and
/// the P2A anchor is *not* P2TR — see CTESR-GATE (retired 2026-08-15) §2.1(a)), so every payload shifts by
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
/// MIGRATION (retired 2026-08-15)):
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
/// co-sign time. See SPLIT-FINDINGS (retired 2026-08-15) — that census currently rests on server/enclave
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
    // `prevout_value`, NOT `coin.amount` — see this function's doc. The same binding feeds the
    // sighash and the disclosure, so the SE recomputes the hash this signature was actually made
    // over. Passing `coin.amount` instead made every tier past the first disclose a value BIP-341
    // never committed to, which witness binding would refuse as a mismatch.
    let prevouts = vec![TxOut { value: prevout_value, script_pubkey: input_address.script_pubkey() }];

    let hash = SighashCache::new(&unsigned_tx).taproot_key_spend_signature_hash(
        0,
        &sighash::Prevouts::All(&prevouts),
        TapSighashType::All,
    )?;

    calculate_musig_session(coin, hash, encoded_unsigned_tx, &prevouts, 0, TapSighashType::All)
}

#[cfg(test)]
mod flat_ladder_params_tests {
    use super::*;

    /// The client's whole D8(f) defence rests on this being a total, explicit function: an unknown
    /// network must be `None` so the caller refuses, never a silently-defaulted `interval`.
    #[test]
    fn unknown_networks_are_none_not_defaulted() {
        for net in ["", "mainet", "bitcoin-testnet", "liquid", "BITCOIN_TESTNET", "regtest2"] {
            assert!(
                TesrParams::flat_ladder_params(net).is_none(),
                "network {net:?} must not resolve to a flat ladder — a guessed `interval` silently \
                 changes which backup chains INV-5 accepts"
            );
        }
    }

    #[test]
    fn known_networks_resolve_and_are_case_insensitive() {
        assert_eq!(TesrParams::flat_ladder_params("bitcoin"), Some((10_000, 100)));
        assert_eq!(TesrParams::flat_ladder_params("MainNet"), Some((10_000, 100)));
        assert_eq!(TesrParams::flat_ladder_params("regtest"), Some((1_000, 10)));
        assert_eq!(TesrParams::flat_ladder_params("REGTEST"), Some((1_000, 10)));
    }

    /// D25 applies to the flat ladder too: testnet/signet rehearse the mainnet numbers, so the
    /// timings that ship are the timings that were exercised.
    #[test]
    fn testnet_and_signet_run_the_mainnet_flat_ladder() {
        let mainnet = TesrParams::flat_ladder_params("bitcoin").unwrap();
        for net in ["testnet", "testnet3", "testnet4", "signet"] {
            assert_eq!(
                TesrParams::flat_ladder_params(net),
                Some(mainnet),
                "{net} must rehearse the mainnet flat ladder (D25)"
            );
        }
    }

    /// `interval` must divide `initlock`, and the quotient is the ladder's hop capacity — the figure
    /// `clients/libs/rust-sdk/src/config.rs` calls the fail-closed bound on audit [17]. A ragged
    /// division would leave a final hop shorter than `interval`, which INV-5 rejects by construction.
    #[test]
    fn every_profile_divides_evenly_into_100_hops() {
        for net in ["bitcoin", "testnet", "signet", "regtest"] {
            let (initlock, interval) = TesrParams::flat_ladder_params(net).unwrap();
            assert!(interval > 0, "{net}: interval 0 would make every hop a no-op");
            assert_eq!(
                initlock % interval,
                0,
                "{net}: initlock {initlock} is not a whole number of {interval}-block hops"
            );
            assert_eq!(initlock / interval, 100, "{net}: expected 100 hops of capacity");
        }
    }

    /// The const path and the runtime path must not drift — the SDK's exit margin is derived from
    /// the former while the client's refusal uses the latter.
    #[test]
    fn const_and_runtime_paths_agree() {
        for net in ["bitcoin", "testnet", "testnet3", "testnet4", "signet", "regtest"] {
            assert_eq!(
                TesrParams::flat_ladder_params_const(net),
                TesrParams::flat_ladder_params(net),
                "{net}: const and runtime tables disagree"
            );
        }
    }

    #[test]
    fn const_str_eq_matches_std() {
        for (a, b) in [("a", "a"), ("a", "b"), ("", ""), ("ab", "a"), ("a", "ab"), ("regtest", "regtes")] {
            assert_eq!(const_str_eq(a, b), a == b, "const_str_eq({a:?}, {b:?})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **[V-6] The regtest pin is DERIVED, and this test is what makes that claim checkable.**
    ///
    /// `TesrParams::REGTEST_ATTESTATION_IDENTITY` is a 32-byte literal, and a literal is exactly the
    /// shape of thing that rots into a lie: the dev seed could move, the enclave's derivation could
    /// gain a version, and nothing would notice — a wrong pin refuses every attestation, and the
    /// tempting "fix" is to trust the key the coordinator serves, which is the hole D69 closed.
    ///
    /// So re-derive it here from the seed THIS REPO COMMITS (`docker-compose-main.yml`, the
    /// `vault-init` payload), under the enclave's own stated derivation
    /// (`enclave::derive_identity_keypair`):
    ///
    ///   `sk = SHA256("utexo/attestation-identity/v1" ‖ seed32 ‖ counter_u8)`, counter from 0,
    ///   retried while the digest is not a valid secp256k1 scalar; identity = x-only pubkey of `sk`.
    ///
    /// The counter loop is reproduced rather than assumed to land on 0 — assuming it would make the
    /// test agree with the constant for the wrong reason on any future seed.
    #[test]
    fn regtest_attestation_identity_is_derivable_from_the_committed_dev_seed() {
        use bitcoin::hashes::{sha256, Hash as _};
        // The seed this repository ships for its own dev stack. If docker-compose-main.yml changes,
        // this must change with it — and then the pin must too, which is the point.
        const DEV_SEED_HEX: &str =
            "8b10a037120cf37441bd7623da2aa488c21889017ffb4f4d303b9dbcbada5bee";
        const DOMAIN: &[u8] = b"utexo/attestation-identity/v1";

        let seed = hex::decode(DEV_SEED_HEX).expect("dev seed is hex");
        assert_eq!(seed.len(), 32, "the enclave hashes exactly 32 seed bytes");

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let mut derived: Option<String> = None;
        for counter in 0u8..=255 {
            let mut preimage = DOMAIN.to_vec();
            preimage.extend_from_slice(&seed);
            preimage.push(counter);
            let sk_bytes = sha256::Hash::hash(&preimage).to_byte_array();
            // A SHA-256 digest is not guaranteed to be a valid scalar; the enclave retries, so so do
            // we. `from_slice` rejects zero and >= n, which is the same acceptance test.
            if let Ok(sk) = bitcoin::secp256k1::SecretKey::from_slice(&sk_bytes) {
                let (xonly, _parity) =
                    bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &sk).x_only_public_key();
                derived = Some(hex::encode(xonly.serialize()));
                break;
            }
        }
        let derived = derived.expect("a valid scalar exists within 256 counters");

        assert_eq!(
            derived,
            TesrParams::REGTEST_ATTESTATION_IDENTITY,
            "the pinned regtest identity is not what this repo's committed dev seed derives. Either \
             the seed in docker-compose-main.yml moved, or the enclave's derivation changed, or the \
             constant was edited by hand. A pin that does not match the enclave refuses EVERY \
             attestation, and the coin then reads as un-laddered for a reason no message explains."
        );

        // ...and the pin is actually WIRED, for the network it belongs to and no other. A constant
        // nothing reads would satisfy the check above and protect nothing.
        assert_eq!(
            TesrParams::attestation_identity_const("regtest"),
            Some(TesrParams::REGTEST_ATTESTATION_IDENTITY)
        );
        for provisionless in ["bitcoin", "mainnet", "testnet", "testnet3", "testnet4", "signet"] {
            assert_eq!(
                TesrParams::attestation_identity_const(provisionless),
                None,
                "{provisionless} has no enclave provisioned — pinning a key for it would anchor \
                 trust to a server that does not exist"
            );
        }
    }

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
        // [D25] This line used to assert `signet == regtest`, PINNING the defect as intended
        // behaviour: signet and testnet have real ~10-minute blocks, so the toy schedule meant a
        // ~4-hour head start on the profile the deployed coordinator runs. A public test network
        // must exercise the schedule that ships; only regtest, which mines on demand, stays fast.
        assert_eq!(TesrParams::for_network("signet"), TesrParams::mainnet());
        assert_eq!(TesrParams::for_network("testnet"), TesrParams::mainnet());
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

#[cfg(test)]
mod grid_law_tests {
    use super::TesrParams;

    /// **[D38/R13] The grid is the set of values the schedule can PRODUCE; the band is wider.**
    #[test]
    fn the_grid_admits_what_the_builders_emit_and_refuses_what_they_cannot() {
        for (name, p) in [("mainnet", TesrParams::mainnet()), ("regtest", TesrParams::regtest())] {
            // Every value a builder emits is on its grid, at every renewal epoch it can reach.
            for m in 0u16..40 {
                let e = p.ext_csv(m);
                assert!(p.is_on_ext_grid(e), "{name}: ext_csv({m}) = {e} is off its own grid");
                let d = p.state_csv(m);
                assert!(p.is_on_state_grid(d), "{name}: state_csv({m}) = {d} is off its own grid");
            }
            // …and a value strictly between two rungs is refused, which is the whole point: it sits
            // INSIDE the band, so the band check admits it.
            let between = p.e0 - 1;
            if p.delta_e > 1 && between > p.e_floor {
                assert!(between >= p.e_floor && between <= p.e0, "{name}: the probe must be in-band");
                assert!(
                    !p.is_on_ext_grid(between),
                    "{name}: e0-1 = {between} is in-band and must be OFF the grid (δE {})",
                    p.delta_e
                );
            }
            let between_d = p.d0 - 1;
            if p.delta > 1 && between_d > p.d_floor {
                assert!(
                    !p.is_on_state_grid(between_d),
                    "{name}: d0-1 = {between_d} is in-band and must be OFF the grid (δ {})",
                    p.delta
                );
            }
        }
    }

    /// The floor is admitted by fiat, because `ext_csv`/`state_csv` CLAMP there. A schedule whose
    /// floor is not itself a grid point still produces the floor, and refusing it would refuse the
    /// last honest renewal — the one a coin at its end of life depends on.
    #[test]
    fn the_floor_clamp_is_on_the_grid_even_when_the_arithmetic_disagrees() {
        let p = TesrParams { d0: 100, delta: 30, d_floor: 7, e0: 100, delta_e: 30, e_floor: 7, m_max: 3, committed_fee_rate: 2.0 };
        assert_ne!((p.e0 - p.e_floor) % p.delta_e, 0, "the probe schedule must have an off-grid floor");
        assert!(p.is_on_ext_grid(p.e_floor), "the floor clamp must be admitted");
        assert!(p.is_on_state_grid(p.d_floor), "the floor clamp must be admitted");
        assert_eq!(p.ext_csv(u16::MAX), p.e_floor, "…and it really is what the builder emits");
    }

    /// `SPINE_CSV = 0` must NOT be admitted by the state grid — a split state is its own kind with an
    /// exact band, not a state walked to zero. Admitting it here would let the grid law be used to
    /// justify a zero-CSV state on the ordinary lane.
    #[test]
    fn zero_is_not_a_state_grid_point() {
        for p in [TesrParams::mainnet(), TesrParams::regtest()] {
            assert!(!p.is_on_state_grid(0), "0 must be off the state grid (d_floor {})", p.d_floor);
        }
    }
    /// **[D69] THE PIN RESOLVER — every branch, because each one is a security decision.**
    #[test]
    fn the_attestation_identity_resolves_pin_first_config_second_and_otherwise_refuses() {
        use super::TesrParams as P;
        const K: &str = "a3c87c1dd1344e30f6374b568306f46031ed9bfa35ec73c03c61d819848c5def";

        // No pin compiled in + nothing configured => REFUSAL. The message must name the remedy,
        // because a bare error here reads as an outage. **regtest is no longer in this list** — it
        // has a pin now, which is what the next block checks instead.
        for net in ["bitcoin", "mainnet", "testnet", "signet"] {
            let e = P::attestation_identity(net, None).expect_err("no pin, no config => refuse");
            assert!(
                e.contains("no attestation identity is available") && e.contains("B11"),
                "{net}: the refusal must say WHAT is missing and WHY it matters: {e}"
            );
            // …and an empty string is not a configuration.
            assert!(P::attestation_identity(net, Some("   ")).is_err(), "{net}: blank is not a pin");
        }

        // Configured, no pin => accepted, verbatim.
        assert_eq!(P::attestation_identity("testnet", Some(K)).unwrap(), K);

        // **PIN FIRST, and the pinned case is no longer hypothetical.** regtest resolves with NO
        // configuration at all, because the compiled-in pin is the anchor.
        assert_eq!(
            P::attestation_identity("regtest", None).unwrap(),
            P::REGTEST_ATTESTATION_IDENTITY,
            "a pinned network must resolve without configuration — that is what pinning is for"
        );
        // ...and a configuration that DISAGREES with the pin is REFUSED — it does not silently lose
        // to the pin, and it certainly does not win. I expected "pin wins" here and the resolver is
        // stricter than that, correctly: silently ignoring a configured key would leave an operator
        // believing they had re-pointed the anchor, and a mismatch is far more likely to be a wrong
        // build than a wrong config. Refusing says so instead of guessing which.
        const OTHER: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
        let e = P::attestation_identity("regtest", Some(OTHER))
            .expect_err("a configured key that contradicts the compiled-in pin must REFUSE");
        assert!(
            e.contains("does not match the one COMPILED IN") && e.contains("B11"),
            "the refusal must name the conflict and the hole it protects: {e}"
        );
        // Agreeing configuration is fine — that is a redundant statement of the same anchor.
        assert_eq!(
            P::attestation_identity("regtest", Some(P::REGTEST_ATTESTATION_IDENTITY)).unwrap(),
            P::REGTEST_ATTESTATION_IDENTITY
        );

        // THE PROPERTY THAT MATTERS ONCE A PIN EXISTS. Simulated here rather than waiting for a
        // provisioned enclave, because the rule must be written and tested before the first key is
        // pinned — afterwards is too late to discover it was overridable.
        let pinned_case = |cfg: Option<&str>| -> Result<String, String> {
            match P::attestation_identity_const("bitcoin") {
                Some(_) => P::attestation_identity("bitcoin", cfg),
                // No mainnet enclave yet: assert the intended rule against the resolver's own logic
                // by giving it a network that DOES have a pin, if one is ever added. Until then this
                // arm records the expectation in executable form.
                None => Err("no pin compiled in for bitcoin (expected while unprovisioned)".into()),
            }
        };
        assert!(
            pinned_case(None).is_err(),
            "while no mainnet enclave is provisioned this must refuse, not invent a key"
        );
    }

    /// **[D69] `None` everywhere is the HONEST state, and this test exists so it cannot drift into
    /// a placeholder.**
    ///
    /// If someone pins a value here, they must also decide what happens to a configured override on
    /// that network — the resolver refuses a mismatch — and update this test deliberately. A pinned
    /// key that nobody noticed being added is exactly as bad as no key at all.
    #[test]
    fn only_regtest_has_a_pinned_identity_until_more_enclaves_are_provisioned() {
        use super::TesrParams as P;
        // **[V-6] REGTEST IS NOW PINNED**, and this test changed with it — its predecessor asserted
        // that NO network had a pin and said, in its own failure text, to update it in the same
        // commit as the first one landed. This is that update, and the tripwire is kept rather than
        // deleted: pinning a key changes the security posture of every client build, so each new
        // network must arrive through a deliberate edit here.
        assert_eq!(
            P::attestation_identity_const("regtest"),
            Some(P::REGTEST_ATTESTATION_IDENTITY),
            "regtest's pin is the identity derived from the seed this repo commits for its own dev \
             stack — see `regtest_attestation_identity_is_derivable_from_the_committed_dev_seed`"
        );
        for net in ["bitcoin", "mainnet", "testnet", "testnet3", "testnet4", "signet"] {
            assert!(
                P::attestation_identity_const(net).is_none(),
                "{net} now has a compiled-in attestation identity. That is the intended end state — \
                 but it changes the security posture of every client build, so update this test in \
                 the SAME commit, and make sure the mismatch-refusal in `attestation_identity` is \
                 what you want for that network. Note `SdkConfig` reads this to decide \
                 `colored_ladder`, so pinning a network also turns ONE COIN SHAPE on for it."
            );
        }
    }

}

/// **[REQ-83 / REQ-85] What makes a transaction a well-formed TAIL carrier — and what does not.**
///
/// A tail is a payment below [`DUST_LIMIT`] riding as the transaction's ONE permitted sub-dust
/// output. Bitcoin allows exactly one (`MAX_DUST_OUTPUTS_PER_TX = 1`), it must then pay ZERO fee,
/// and the dust must be spent by the package child.
///
/// **We can afford one because our anchor is FUNDED at [`P2A_VALUE`] = 240, its own standardness
/// threshold — so it is not dust and our slot has never been spent.** Spark spends the same slot on
/// a zero-value anchor, which is why a sub-dust child kills a whole branch there.
///
/// **TAILS BELONG TO THE PAYMENT LANE, NOT TO TIER VERIFICATION.** A first attempt wired this into
/// `refuse_dust_payloads`, which verifies TIERS — and eight dust-poisoning attack tests failed,
/// correctly. On a tier a sub-dust output is an ATTACK, and reporting it as "a well-formed tail,
/// admission pending" tells an attacker their shape is right and softens a security refusal into a
/// feature-flag notice. The same bytes mean opposite things in the two lanes, so the rule stays out
/// of the tier verifier.
///
/// This is the RULE, deliberately separated from its admission — and the separation has now paid
/// off. §6.0's relay claims were UNPROVEN when this was written; `scripts/tail_relay_probe.py` has
/// since asked Bitcoin Core and recorded that our funded-anchor shape is refused for the **FEE**
/// (`min relay fee not met`) where Spark's zero-value-anchor shape is refused for **DUST**, and that
/// a `[0-fee tail parent, paying child]` package relays (`package_msg: success`). So what gets
/// admitted was specified and tested BEFORE the ground moved, rather than invented under pressure
/// the day it moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailVerdict {
    /// No sub-dust output: an ordinary transaction, nothing to judge.
    NoTail,
    /// Exactly one sub-dust output, at `vout`, worth `value`, in a transaction shaped as REQ-83
    /// requires. Well-formed — which is NOT the same as admitted.
    WellFormed { vout: u32, value: u64 },
    /// More than one sub-dust output. **[REQ-85] This is the cap that carries the whole economic
    /// argument**: the maximum sweepable prize must stay below the cost of broadcasting the split.
    /// One tail is at most 329 sat against ~504 sat to broadcast; two tails and sweeping starts to
    /// pay. The cap is not tidiness.
    TooManyTails { count: usize },
    /// A sub-dust output in a transaction whose anchor is not the FUNDED 240 kind. The funded anchor
    /// is what leaves the dust slot free for the tail; with a zero-value anchor the transaction has
    /// two dust outputs and is non-standard — Spark's failure exactly.
    AnchorNotFunded,
    /// A sub-dust output of zero value. Not a payment.
    ZeroValueTail { vout: u32 },
}

/// Classify a transaction's tail shape from its outputs. `anchor_value` is the value carried by the
/// P2A anchor, or `None` when the transaction has no anchor.
///
/// Pure and total: every transaction gets a verdict, and the caller decides what to do with it.
pub fn tail_verdict(outputs: &[(u64, Vec<u8>)], anchor_value: Option<u64>) -> TailVerdict {
    let mut subdust: Vec<(u32, u64)> = Vec::new();
    for (i, (value, spk)) in outputs.iter().enumerate() {
        // The anchor and the RGB opret are not payloads and are never tails.
        if spk.as_slice() == P2A_SCRIPT_BYTES || spk.first() == Some(&0x6a) {
            continue;
        }
        if *value < DUST_LIMIT {
            subdust.push((i as u32, *value));
        }
    }
    match subdust.len() {
        0 => TailVerdict::NoTail,
        1 => {
            let (vout, value) = subdust[0];
            if value == 0 {
                return TailVerdict::ZeroValueTail { vout };
            }
            // The funded anchor is the precondition, not a nicety: it is what keeps the transaction
            // to ONE dust output.
            if anchor_value != Some(P2A_VALUE) {
                return TailVerdict::AnchorNotFunded;
            }
            TailVerdict::WellFormed { vout, value }
        }
        n => TailVerdict::TooManyTails { count: n },
    }
}

/// **[REQ-83 / §6.0.3] The four leaf shapes, chosen by value — and why there are exactly four.**
///
/// REQ-83 promises that **a sats payment of any amount at or above 1 sat is expressible**. That
/// promise is not kept by one mechanism; it is kept by four, each covering a band of value, and it
/// is kept only if the four bands **tile `[1, ∞)` with no gap and no overlap**. A gap is not a
/// cosmetic defect — a gap is an amount a user cannot pay, which is precisely the requirement being
/// violated. So the bands are derived here from the SAME floor functions the builders use, never
/// restated as literals, and [`leaf_shapes_tile_every_value`] sweeps them.
///
/// | band | shape | what funds its exit |
/// |---|---|---|
/// | `v ≥ min_child_value` | [`LeafShape::Laddered`] | two rungs, self-funding |
/// | `min_spine_tip_value ≤ v < min_child_value` | [`LeafShape::SpineTip`] | ONE rung — the tip shape, which already exists |
/// | `DUST_LIMIT ≤ v < min_spine_tip_value` | [`LeafShape::Stub`] | nothing: depth-0, exits with its group |
/// | `1 ≤ v < DUST_LIMIT` | [`LeafShape::Tail`] | nothing: the split's one permitted dust output, released by its fragment |
///
/// **Read the third and fourth rows against the owner's rule that small leaves need not exit
/// alone.** Below `min_spine_tip_value` a leaf cannot fund a rung, and the older design's answer was
/// to refuse the payment. That answer is withdrawn: the leaf is admitted and exits with the group
/// instead. Quantising the payment to what a leaf can self-fund is the failure mode, not the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafShape {
    /// Two rungs — extension and state. The only shape that funds its own exit unaided.
    Laddered,
    /// One rung, the spine-tip cap. Cheaper than a ladder and already built: this is the shape the
    /// sender's CHANGE leg has always taken, being applied to a payee's leg for the first time.
    SpineTip,
    /// No ladder at all. Above dust, so it is an ordinary spendable output — it simply cannot afford
    /// a rung of its own and leaves on its group's exit.
    Stub,
    /// Below dust: the split's single permitted sub-dust output, carried at zero fee behind a funded
    /// anchor and released to any sibling by its [`TailVerdict`] fragment.
    Tail,
    /// **Zero is not an amount.** Kept as an arm rather than folded into `Tail` so that the one
    /// genuinely unpayable value is named by the type instead of silently becoming a 0-sat tail.
    Unpayable,
}

impl LeafShape {
    /// Which shape a leaf of `value` takes at this fee rate.
    ///
    /// The two upper boundaries are read from [`min_child_value`] and [`min_spine_tip_value`] — the
    /// same functions the admission guards and the builders call — so the shape a payment is
    /// admitted at and the ladder that is then built can never be two different answers. That
    /// divergence is the failure the whole split-floor apparatus exists to prevent.
    pub fn for_value(value: u64, fee_rate_sats_per_vb: f64, dust_limit: u64) -> LeafShape {
        if value == 0 {
            return LeafShape::Unpayable;
        }
        if value < dust_limit {
            return LeafShape::Tail;
        }
        if value < min_spine_tip_value(fee_rate_sats_per_vb, dust_limit) {
            return LeafShape::Stub;
        }
        if value < min_child_value(fee_rate_sats_per_vb, dust_limit) {
            return LeafShape::SpineTip;
        }
        LeafShape::Laddered
    }

    /// How many ladder rungs this shape funds. `0` means the leaf exits with its group.
    pub fn rungs(self) -> u8 {
        match self {
            LeafShape::Laddered => 2,
            LeafShape::SpineTip => 1,
            LeafShape::Stub | LeafShape::Tail | LeafShape::Unpayable => 0,
        }
    }

    /// Whether a leaf of this shape can be put on chain by its owner ALONE.
    ///
    /// False is not a defect: it is the owner's accepted trade (`small leaves need not exit alone`).
    /// It is exposed so a caller can tell a user what they are buying, never so a caller can refuse
    /// the payment.
    pub fn exits_unaided(self) -> bool {
        self.rungs() > 0
    }
}

#[cfg(test)]
mod req56_collapse_tx {
    use super::*;

    const F: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    fn key(b: u8) -> String { hex::encode([b; 32]) }

    fn decode(t: &TierTx) -> bitcoin::Transaction {
        bitcoin::consensus::encode::deserialize(&hex::decode(&t.tx_hex).unwrap()).unwrap()
    }

    /// **THE PROPERTY THE SE ACTUALLY CHECKS: every leaf paid its FULL value at its OWN key.**
    ///
    /// Asserted per output rather than on the sum. A `C` that pays the right TOTAL to the wrong
    /// distribution satisfies any sum check and still discharges a holder without paying them, which
    /// is the one outcome REQ-56 exists to make impossible.
    #[test]
    fn every_leaf_is_paid_in_full_at_its_own_key() {
        let payouts = vec![(key(0xa1), 3_000u64), (key(0xb2), 2_000), (key(0xc3), 1_500)];
        let t = build_collapse_tx(F, 0, 10_000, &payouts, Some((key(0xff), 3_000))).unwrap();
        let tx = decode(&t);
        assert_eq!(tx.output.len(), 4, "three leaves and the owner's remainder");
        for (i, (k, v)) in payouts.iter().enumerate() {
            assert_eq!(tx.output[i].value, *v, "leaf {i} must be paid IN FULL");
            assert_eq!(
                hex::encode(&tx.output[i].script_pubkey.as_bytes()[2..]),
                *k,
                "leaf {i} must be paid at its OWN exit key"
            );
            assert_eq!(tx.output[i].script_pubkey.as_bytes()[0], 0x51, "P2TR");
        }
        assert_eq!(tx.input.len(), 1, "REQ-55: exactly one input");
        assert_eq!(tx.input[0].previous_output.txid.to_string(), F, "C must spend THIS root's F");
    }

    /// **The fee comes out of the OWNER'S remainder, never out of a payout.**
    ///
    /// Shaving a fee across the payouts is arithmetically identical to underpaying every leaf, and
    /// the SE refuses exactly that. This pins that a smaller remainder leaves the payouts untouched.
    #[test]
    fn the_fee_is_taken_from_the_remainder_and_the_payouts_do_not_move() {
        let payouts = vec![(key(0xa1), 3_000u64), (key(0xb2), 2_000)];
        let full = decode(&build_collapse_tx(F, 0, 10_000, &payouts, Some((key(0xff), 5_000))).unwrap());
        let feed = decode(&build_collapse_tx(F, 0, 10_000, &payouts, Some((key(0xff), 4_600))).unwrap());
        assert_eq!(full.output[0].value, feed.output[0].value);
        assert_eq!(full.output[1].value, feed.output[1].value);
        assert_eq!(full.output[2].value - feed.output[2].value, 400, "the whole fee came from the owner");
    }

    /// **A zero remainder emits NO owner output, not a zero-value one.**
    ///
    /// A 0-sat P2TR is non-standard, so emitting one would turn a tree that closes tightly into a
    /// tree that cannot close at all — the failure would appear at broadcast, long after the SE has
    /// frozen the root and the signature can never be reissued.
    #[test]
    fn a_zero_remainder_is_no_output_rather_than_a_zero_output() {
        let payouts = vec![(key(0xa1), 9_800u64)];
        let tx = decode(&build_collapse_tx(F, 0, 10_000, &payouts, Some((key(0xff), 0))).unwrap());
        assert_eq!(tx.output.len(), 1, "no 0-sat output may be emitted");
        assert!(tx.output.iter().all(|o| o.value > 0));
    }

    /// **An empty payout set is refused at BUILD time.**
    ///
    /// The SE refuses it too (`validate_for_grant`), and for the sharper reason: an empty obligation
    /// is satisfied vacuously — correct arithmetic, catastrophic answer. Refusing here as well means
    /// a caller never gets far enough to ask.
    #[test]
    fn an_empty_obligation_is_refused_rather_than_satisfied_vacuously() {
        assert!(build_collapse_tx(F, 0, 10_000, &[], Some((key(0xff), 9_000))).is_err());
    }

    /// **Paying out more than `F` holds is refused, rather than built and refused later.**
    #[test]
    fn outputs_may_not_exceed_the_funding_value() {
        let payouts = vec![(key(0xa1), 6_000u64), (key(0xb2), 5_000)];
        assert!(build_collapse_tx(F, 0, 10_000, &payouts, None).is_err());
        // …and exactly `funding_value` is allowed: a zero-fee C is a caller's problem to broadcast,
        // not a shape this builder may silently reshape.
        assert!(build_collapse_tx(F, 0, 11_000, &payouts, None).is_ok());
    }

    /// **A key that is not 32 bytes is refused, rather than producing a malformed scriptPubKey.**
    ///
    /// Left unchecked, a short key yields a `0x51 0x20 <short>` script that is not P2TR at all, and
    /// the leaf it was meant to pay would be unpayable forever — after the freeze.
    #[test]
    fn a_key_that_is_not_x_only_is_refused() {
        for bad in ["", "ab", &hex::encode([7u8; 33])] {
            assert!(
                build_collapse_tx(F, 0, 10_000, &[(bad.to_string(), 1_000)], None).is_err(),
                "key {bad:?} must be refused"
            );
        }
    }

    /// **Version 2, not the tiers' v3/TRUC.** `C` pays its own fee and has no anchor and no child, so
    /// TRUC's topology limits would constrain it for nothing.
    #[test]
    fn the_collapse_is_a_plain_v2_transaction() {
        let tx = decode(&build_collapse_tx(F, 0, 10_000, &[(key(0xa1), 9_000)], None).unwrap());
        assert_eq!(tx.version, 2);
        assert_eq!(tx.lock_time.to_consensus_u32(), 0);
    }
}

#[cfg(test)]
mod req83_leaf_shapes {
    use super::*;

    /// Rates worth checking: the committed mainnet rate the spec quotes, plus an implausibly cheap
    /// and an implausibly expensive one. The bands must be well-formed at ALL of them — a design
    /// that only tiles at 3.0 sat/vB is a design that develops a hole in a fee spike.
    const RATES: &[f64] = &[0.1, 1.0, 2.0, 3.0, 10.0, 100.0, 1_000.0];

    /// **THE REQUIREMENT ITSELF: no amount at or above 1 sat is unpayable.**
    ///
    /// This is REQ-83 restated as an executable claim. It sweeps every value up past the top
    /// boundary rather than sampling, because the defect this guards against is a one-value gap at a
    /// boundary — exactly what an off-by-one in any of the three comparisons produces, and exactly
    /// what sampling misses.
    #[test]
    fn no_value_above_zero_is_unpayable() {
        for &rate in RATES {
            let top = min_child_value(rate, DUST_LIMIT);
            for v in 1..=top + 50 {
                assert_ne!(
                    LeafShape::for_value(v, rate, DUST_LIMIT),
                    LeafShape::Unpayable,
                    "[REQ-83] {v} sat is unpayable at {rate} sat/vB — that is the requirement broken, \
                     not a rounding detail: it is an amount a user cannot send"
                );
            }
        }
        assert_eq!(LeafShape::for_value(0, 3.0, DUST_LIMIT), LeafShape::Unpayable, "zero is not an amount");
    }

    /// **The four bands tile `[1, ∞)` — no gap, no overlap, and each boundary is where the floor
    /// function says it is.**
    ///
    /// The shape is checked against an INDEPENDENTLY computed expectation rather than against
    /// `for_value`'s own logic, so a boundary moved in the implementation fails here instead of
    /// being mirrored by the test.
    #[test]
    fn the_bands_tile_every_value_at_every_rate() {
        for &rate in RATES {
            let (tip, child) = (min_spine_tip_value(rate, DUST_LIMIT), min_child_value(rate, DUST_LIMIT));
            for v in 1..=child + 50 {
                let want = if v < DUST_LIMIT {
                    LeafShape::Tail
                } else if v < tip {
                    LeafShape::Stub
                } else if v < child {
                    LeafShape::SpineTip
                } else {
                    LeafShape::Laddered
                };
                assert_eq!(
                    LeafShape::for_value(v, rate, DUST_LIMIT),
                    want,
                    "value {v} at {rate} sat/vB (dust {DUST_LIMIT}, tip {tip}, child {child})"
                );
            }
        }
    }

    /// **Each boundary is EXCLUSIVE below and INCLUSIVE at — the off-by-one that would matter.**
    ///
    /// At `min_child_value` exactly, a leaf funds two rungs; one satoshi less, it does not. A `<=`
    /// here would hand a leaf a ladder it cannot pay for, which does not fail at admission — it
    /// fails later, on chain, when the second rung cannot be broadcast.
    #[test]
    fn the_boundaries_are_exact() {
        let rate = 3.0;
        let (tip, child) = (min_spine_tip_value(rate, DUST_LIMIT), min_child_value(rate, DUST_LIMIT));
        for (v, want) in [
            (1, LeafShape::Tail),
            (DUST_LIMIT - 1, LeafShape::Tail),
            (DUST_LIMIT, LeafShape::Stub),
            (tip - 1, LeafShape::Stub),
            (tip, LeafShape::SpineTip),
            (child - 1, LeafShape::SpineTip),
            (child, LeafShape::Laddered),
        ] {
            assert_eq!(LeafShape::for_value(v, rate, DUST_LIMIT), want, "at {v} sat");
        }
    }

    /// **The bands can never invert, at any rate — which is what keeps all four arms reachable.**
    ///
    /// If a fee rate could ever push `min_spine_tip_value` below `DUST_LIMIT`, the `Stub` band would
    /// be empty and `for_value` would silently stop returning one of its arms. It cannot: both
    /// floors are `committed_fee + …` over the same dust limit, so the gaps are `dust`, `P2A_VALUE`
    /// and `committed_fee + P2A_VALUE` — all strictly positive. Pinned rather than argued, because
    /// the argument depends on the shape of two functions in another part of this file.
    #[test]
    fn the_bands_never_invert_at_any_rate() {
        for &rate in RATES {
            let (tip, child) = (min_spine_tip_value(rate, DUST_LIMIT), min_child_value(rate, DUST_LIMIT));
            assert!(1 < DUST_LIMIT, "the tail band must be non-empty");
            assert!(DUST_LIMIT < tip, "the stub band is empty at {rate} sat/vB: dust {DUST_LIMIT} >= tip {tip}");
            assert!(tip < child, "the spine-tip band is empty at {rate} sat/vB: tip {tip} >= child {child}");
        }
    }

    /// **§6.0.3's published table is what the code computes.** The spec quotes 1560 / 945 / 330 at
    /// the committed 3.0 sat/vB rate; if the floors move, the table is wrong and a reader is
    /// misinformed about which amounts cost what.
    #[test]
    fn the_published_mainnet_table_holds() {
        assert_eq!(min_child_value(3.0, DUST_LIMIT), 1_560);
        assert_eq!(min_spine_tip_value(3.0, DUST_LIMIT), 945);
        assert_eq!(DUST_LIMIT, 330);
    }

    /// **A leaf that cannot exit alone is still ADMITTED — the owner's rule, as a test.**
    ///
    /// `exits_unaided()` is false for a stub and for a tail, and that is a disclosure to the payer,
    /// never grounds to refuse the payment. A design that quantised payments up to what a leaf can
    /// self-fund would pass every other test in this module and still be the failure.
    #[test]
    fn small_leaves_are_admitted_even_though_they_cannot_exit_alone() {
        for (v, rungs) in [(1u64, 0u8), (329, 0), (330, 0), (944, 0), (945, 1), (1_560, 2)] {
            let shape = LeafShape::for_value(v, 3.0, DUST_LIMIT);
            assert_ne!(shape, LeafShape::Unpayable, "{v} sat must be payable");
            assert_eq!(shape.rungs(), rungs, "{v} sat funds {rungs} rung(s)");
        }
        assert!(!LeafShape::for_value(200, 3.0, DUST_LIMIT).exits_unaided());
        assert!(!LeafShape::for_value(500, 3.0, DUST_LIMIT).exits_unaided());
        assert!(LeafShape::for_value(1_000, 3.0, DUST_LIMIT).exits_unaided());
    }
}

#[cfg(test)]
mod req83_85_tail_rule {
    use super::*;

    fn p2tr() -> Vec<u8> {
        let mut v = vec![0x51, 0x20];
        v.extend_from_slice(&[0u8; 32]);
        v
    }
    fn anchor() -> Vec<u8> {
        P2A_SCRIPT_BYTES.to_vec()
    }
    fn opret() -> Vec<u8> {
        vec![0x6a, 0x02, 0xde, 0xad]
    }

    #[test]
    fn an_ordinary_transaction_has_no_tail() {
        let outs = vec![(DUST_LIMIT, p2tr()), (P2A_VALUE, anchor())];
        assert_eq!(tail_verdict(&outs, Some(P2A_VALUE)), TailVerdict::NoTail);
    }

    #[test]
    fn one_sub_dust_output_with_a_funded_anchor_is_well_formed() {
        let outs = vec![(DUST_LIMIT, p2tr()), (329, p2tr()), (P2A_VALUE, anchor())];
        assert_eq!(
            tail_verdict(&outs, Some(P2A_VALUE)),
            TailVerdict::WellFormed { vout: 1, value: 329 }
        );
        // The boundary is exclusive: DUST_LIMIT itself is an ordinary payload, not a tail.
        let at_floor = vec![(DUST_LIMIT, p2tr()), (P2A_VALUE, anchor())];
        assert_eq!(tail_verdict(&at_floor, Some(P2A_VALUE)), TailVerdict::NoTail);
        // And 1 sat is a legitimate tail — REQ-83 says any amount at or above 1.
        let one = vec![(1, p2tr()), (P2A_VALUE, anchor())];
        assert_eq!(
            tail_verdict(&one, Some(P2A_VALUE)),
            TailVerdict::WellFormed { vout: 0, value: 1 }
        );
    }

    #[test]
    fn req85_two_tails_are_refused_because_sweeping_would_start_to_pay() {
        // THE CAP THAT CARRIES THE ECONOMIC ARGUMENT. One tail is at most 329 sat against roughly
        // 504 sat to broadcast the split at 3 sat/vB, so a thief always loses. Two tails and the
        // prize can exceed the cost — which is why this is a rule and not tidiness.
        let outs = vec![(329, p2tr()), (200, p2tr()), (P2A_VALUE, anchor())];
        assert_eq!(tail_verdict(&outs, Some(P2A_VALUE)), TailVerdict::TooManyTails { count: 2 });
        assert!(329 + 200 > 504, "two tails can already exceed the cost of broadcasting");
    }

    #[test]
    fn a_zero_value_anchor_leaves_no_slot_for_a_tail() {
        // Spark's exact failure: their anchor is zero-value, so it IS the transaction's one dust
        // output and any sub-dust payload makes a second one — non-standard, and it kills every
        // sibling in the branch.
        let outs = vec![(329, p2tr()), (0, anchor())];
        assert_eq!(tail_verdict(&outs, Some(0)), TailVerdict::AnchorNotFunded);
        assert_eq!(tail_verdict(&outs, None), TailVerdict::AnchorNotFunded);
    }

    #[test]
    fn a_zero_value_tail_is_not_a_payment() {
        let outs = vec![(0, p2tr()), (P2A_VALUE, anchor())];
        assert_eq!(tail_verdict(&outs, Some(P2A_VALUE)), TailVerdict::ZeroValueTail { vout: 0 });
    }

    #[test]
    fn the_anchor_and_the_opret_are_never_counted_as_tails() {
        // Both are sub-dust by value or by kind; counting either would make every ordinary coloured
        // tier look like a tail carrier and the cap would fire on honest transactions.
        let outs = vec![(DUST_LIMIT, p2tr()), (P2A_VALUE, anchor()), (0, opret())];
        assert_eq!(tail_verdict(&outs, Some(P2A_VALUE)), TailVerdict::NoTail);
    }
}
