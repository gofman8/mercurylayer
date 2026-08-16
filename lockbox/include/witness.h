// WITNESS BINDING — the SE learns what it is signing, without unblinding its own signature.
//
// THE PROBLEM
// -----------
// `/get_partial_signature` receives a 133-byte session commitment and nothing else. The SE cannot
// distinguish a legitimate tier co-signature from one that moves the coin to an attacker, so every
// enforcement rule in SPEC.md §5.4 — the payout predicate, the owner latch, the leaf registry — has
// nothing to stand on. It can count signatures; it cannot police what they authorise.
//
// THE MECHANISM
// -------------
// The client additionally discloses the unsigned transaction and the public session inputs. The SE:
//
//   1. parses the transaction and recomputes the BIP-341 sighash itself (`tx::`),
//   2. rebuilds the blinded MuSig2 session from that sighash with
//      `secp256k1_blinded_musig_nonce_process_without_keyaggcoeff`,
//   3. byte-compares the result against the 133 bytes it was asked to sign.
//
// A disclosure that does not reproduce the session is refused. This is not a trust upgrade: the SE
// verifies rather than believes, and the comparison is over bytes the client already committed to.
//
// WHAT IT BINDS, FOR FREE
// -----------------------
// BIP-341 commits amounts, scriptPubKeys, outpoints, version, locktime and sequences into the
// sighash. So a disclosure that lies about ANY of them yields a different sighash, a different
// session, and a refusal — the SE does not need to check them individually, and cannot be fooled by
// a field it forgot to look at. That is why the amounts a payout predicate compares are trustworthy
// even though the SE has no chain access: the value is not asserted, it is committed.
//
// WHAT IT DOES NOT BIND
// ---------------------
// Anything the sighash does not cover: which output index carries the payload, what the SE believes
// a P2TR script means, the coin's history. Those remain the SE's own structural assumptions and are
// checked separately. The binding says "these bytes are that transaction" — nothing about whether
// that transaction is one the protocol should allow.
//
// FAILURE POSTURE
// ---------------
// Refusal is free: the check runs before the secnonce is consumed, so a rejected disclosure leaves
// the coin exactly as it was. Absence of a disclosure is NOT a refusal — witness binding is opt-in
// per request while the clients are migrated, and the routes that require it enforce that separately.
// A malformed disclosure IS a refusal: "I could not parse it" must never mean "it is fine".

#ifndef LOCKBOX_WITNESS_H
#define LOCKBOX_WITNESS_H

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "tx.h"

namespace witness {

/// Everything the SE needs to rebuild the session, and nothing secret. Each field is public data the
/// client already holds; none of it lets the SE (or an observer of this request) forge a signature.
struct Disclosure {
    std::string unsigned_tx_hex;                  // the transaction being signed
    uint32_t input_index = 0;                     // which input this signature is for
    std::vector<uint64_t> prevout_values;         // one per input, in input order
    std::vector<std::string> prevout_spks_hex;    // one per input, in input order
    std::string agg_pubkey_hex;                   // 33-byte compressed OUTPUT key (post-tweak)
    std::string agg_nonce_hex;                    // 66-byte aggregate nonce
    std::string blinding_factor_hex;              // 32 bytes
    std::string out_tweak_hex;                    // 32 bytes
    unsigned char hash_type = 0x01;               // SIGHASH_ALL — what the tiers are signed at
};

/// Parse a disclosure out of the request body. Returns `nullopt` if any field is missing or
/// malformed — deliberately strict, because a half-understood disclosure is the one case where
/// "close enough" would let a wrong transaction through.
std::optional<Disclosure> parse_disclosure(const std::string& json_body);

/// The outcome, distinguished so callers can respond correctly and so logs say something useful.
enum class BindResult {
    Match,          // the disclosure reproduces the session: proceed
    Mismatch,       // it does not: refuse, and do NOT consume the secnonce
    Malformed,      // could not parse or hash the disclosure: refuse
    Unsupported,    // a shape this SE does not understand: refuse rather than guess
};

/// Rebuild the session from `d` and compare it against `serialized_session` (the 133 bytes the
/// client asked the SE to sign).
///
/// `out_sighash`, when non-null, receives the recomputed 32-byte sighash on a Match — callers that
/// need to reason about the transaction (the leaf registry, the payout predicate) use the SE's own
/// computation, never the client's claim.
///
/// `out_detail` receives a short human-readable reason on any non-Match, for the response body.
BindResult bind(const Disclosure& d,
                const std::vector<unsigned char>& serialized_session,
                std::vector<unsigned char>* out_sighash,
                std::string* out_detail);

/// Was the transaction's output at `vout` a P2TR paying `xonly`? Used by rules that must read a key
/// out of the money rather than take it from the caller — the owner latch and the payout predicate
/// both do this, and both would be forgeable if they trusted a declared key instead.
bool output_pays_xonly(const std::string& unsigned_tx_hex,
                       uint32_t vout,
                       const std::vector<unsigned char>& xonly);

/// The payload output of a tier, selected STRUCTURALLY.
///
/// # Why not `vout[0]`, and why not a supplied index
///
/// REQ-61(a): the client's `payload_vout` is ATTACKER-SUPPLIED in every conveyed bundle — the client
/// code says so itself. A latch key or an exit key read at an index the attacker chooses is a key
/// the attacker chooses. The spec's earlier `vout[0]` was wrong in the other direction: the anchor
/// is not always second.
///
/// So the SE derives it: the payload is **the unique output whose scriptPubKey is P2TR**. On an
/// uncoloured tier the only other output is the P2A anchor, whose script is `OP_1 <2-byte>` and so
/// is not P2TR, leaving exactly one candidate. A transaction without exactly one P2TR output yields
/// `nullopt`, and the caller MUST refuse rather than pick — a tie broken by position is the same
/// defect wearing a different hat.
struct PayloadOutput {
    uint32_t vout;
    std::vector<unsigned char> xonly;  // 32 bytes
    uint64_t value;
};
std::optional<PayloadOutput> payload_output(const tx::Transaction& t);

/// Convenience overload for a hex-encoded transaction; `nullopt` also when it does not parse.
std::optional<PayloadOutput> payload_output(const std::string& unsigned_tx_hex);

}  // namespace witness

#endif  // LOCKBOX_WITNESS_H
