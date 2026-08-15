// Minimal Bitcoin transaction parsing and BIP-341 key-path sighash, for SE-side WITNESS BINDING.
//
// WHY THIS EXISTS
// ---------------
// The SE is a blind signer: `/get_partial_signature` receives a session commitment and nothing else,
// so it cannot tell a legitimate co-signature from one that moves the money somewhere else. Every
// enforcement rule in SPEC.md §5.4 needs the SE to know WHAT it is signing.
//
// Witness binding closes that: the client discloses the unsigned transaction, the SE recomputes the
// BIP-341 sighash from those bytes, rebuilds the blinded MuSig2 session with
// `secp256k1_blinded_musig_nonce_process_without_keyaggcoeff`, and byte-compares it against the
// session it was asked to sign. A disclosure that does not produce the same session is refused.
//
// The binding is SELF-CHECKING for everything the sighash commits to: amounts, scriptPubKeys,
// outpoints, version, locktime and sequences all feed the hash, so a lying disclosure yields a
// different sighash, a different session, and a refusal. It is NOT self-checking for this code's own
// structural assumptions (which output index is the payload, what a P2TR script looks like), which is
// why those are asserted separately and why `deny_sighash_differential` compares this implementation
// against the Rust builders byte-for-byte over every tier shape before anything depends on it.
//
// SCOPE, deliberately narrow
// --------------------------
// Only what the tiers actually use: segwit-marker transactions, P2TR key-path spends, and
// SIGHASH_ALL (0x01) -- which is what `lib/src/transaction.rs` signs the tiers with
// (`TapSighashType::All`), NOT the SIGHASH_DEFAULT (0x00) that a fresh reading of BIP-341 suggests.
// The two differ by one byte in the message and therefore produce completely different hashes; a
// binding built against the wrong one refuses every honest co-signature. Anything else is REJECTED
// rather than approximated — an SE that guesses at
// a script type it does not understand is worse than one that refuses. There is no signature
// verification and no script execution here: this parses bytes and hashes them.

#ifndef LOCKBOX_TX_H
#define LOCKBOX_TX_H

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace tx {

struct OutPoint {
    unsigned char txid[32];  // internal byte order, as it appears on the wire
    uint32_t vout;
};

struct TxIn {
    OutPoint prevout;
    std::vector<unsigned char> script_sig;  // MUST be empty for the shapes we accept
    uint32_t sequence;
};

struct TxOut {
    uint64_t value;
    std::vector<unsigned char> script_pubkey;
};

struct Transaction {
    uint32_t version;
    std::vector<TxIn> vin;
    std::vector<TxOut> vout;
    uint32_t lock_time;
};

/// The prevouts a key-path sighash commits to. BIP-341 hashes EVERY input's amount and
/// scriptPubKey, not just the one being signed, so a partial list cannot produce a correct hash.
struct Prevout {
    uint64_t value;
    std::vector<unsigned char> script_pubkey;
};

/// Parse a serialised transaction. Accepts both the legacy and segwit encodings; witness data is
/// parsed and discarded, since the sighash does not commit to it.
///
/// Returns `std::nullopt` on ANY malformation — truncation, a length that overflows, a count that
/// exceeds what the buffer can hold. Every failure is a refusal; none is a partial parse.
std::optional<Transaction> parse(const std::vector<unsigned char>& raw);

/// Hex convenience wrapper. Rejects odd-length and non-hex input.
std::optional<Transaction> parse_hex(const std::string& hex);

/// BIP-341 key-path sighash for `input_index`.
///
/// `hash_type` is the SIGHASH byte. The tiers use **`0x01` (SIGHASH_ALL)** — see the note above; it
/// is a required argument rather than a default so no caller can inherit the wrong one silently.
///
/// `prevouts` must have exactly one entry per input, in input order. Returns `std::nullopt` if that
/// does not hold, if `input_index` is out of range, or if any input carries a scriptSig (which would
/// mean this is not the segwit spend we think it is).
///
/// The result is the 32 bytes that go in `msg32` of the blinded `nonce_process`.
std::optional<std::vector<unsigned char>> taproot_key_path_sighash(
    const Transaction& t,
    const std::vector<Prevout>& prevouts,
    size_t input_index,
    unsigned char hash_type);

/// The SIGHASH byte the TES-R tiers are signed with (`TapSighashType::All` in the Rust builders).
constexpr unsigned char SIGHASH_ALL = 0x01;

/// BIP-340 tagged hash: SHA256(SHA256(tag) || SHA256(tag) || data). Exposed because the differential
/// test pins it directly against known vectors — a wrong tagged hash is the failure that would make
/// every sighash wrong in the same way, and therefore invisible to a self-consistency check.
std::vector<unsigned char> tagged_hash(const std::string& tag,
                                       const std::vector<unsigned char>& data);

/// Is this scriptPubKey a P2TR output (`OP_1 <32-byte>`)? The SE reads a leaf's exit key out of one
/// of these, so anything else must be refused rather than interpreted.
bool is_p2tr(const std::vector<unsigned char>& spk);

/// The 32-byte x-only key inside a P2TR scriptPubKey, or `nullopt` if it is not one.
std::optional<std::vector<unsigned char>> p2tr_xonly_key(const std::vector<unsigned char>& spk);

}  // namespace tx

#endif  // LOCKBOX_TX_H
