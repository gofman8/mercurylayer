#pragma once

#include <cstdint>
#include <string>
#include <vector>

/// BIP-340 verification for the SE.
///
/// # Why this file exists at all
///
/// Until now the lockbox had NEVER verified a signature. `secp256k1_schnorrsig_verify` — and every
/// other verify — had zero hits anywhere under `lockbox/`; the SE only ever signed, and all of its
/// routes were unauthenticated. Two requirements need that to change:
///
///   * **REQ-61 (the owner latch)** — co-signatures under a latched sid must carry a fresh BIP-340
///     signature by the key read out of the money itself.
///   * **REQ-54 R2 (`/release`)** — a leaf's holder consents to being discharged by signing
///     `tagged("utexo/leaf_release/v1", sid ‖ nonce32)`.
///
/// Neither is a wiring change. Both are new capability, and a verifier that is silently wrong is
/// worse than none: one that accepts everything looks identical to a working one until someone
/// forges, and one that computes the wrong tagged message refuses every honest request while
/// looking like a control that works.
///
/// # The tag is the load-bearing part
///
/// A BIP-340 tagged hash is `SHA256(SHA256(tag) ‖ SHA256(tag) ‖ msg)` — the doubled tag hash is what
/// makes a signature valid under one tag useless under another. Getting it wrong re-enables
/// cross-protocol replay silently, because it disagrees with every client identically. `tx::tagged_hash`
/// is pinned against libsecp256k1's own BIP-340 challenge by
/// `ci-guards/tests/emit_bip340_vectors.rs`, which re-derives the verification equation by hand and
/// fails if the tag stops binding.
namespace auth {

/// The domain tag for R2. Defined once so the route and the tests cannot drift apart.
inline constexpr const char* RELEASE_TAG = "utexo/leaf_release/v1";

/// Verify a BIP-340 signature over an arbitrary 32-byte message.
///
/// `xonly` MUST be 32 bytes, `sig` 64, `msg32` 32. Any other length is a refusal, not an assertion:
/// these come off the wire.
bool verify_bip340(const std::vector<unsigned char>& xonly,
                   const std::vector<unsigned char>& msg32,
                   const std::vector<unsigned char>& sig);

/// The exact message R2 signs: `tagged(RELEASE_TAG, sid ‖ nonce32)`.
///
/// `sid` is hashed as its ASCII characters, matching the client, because a statechain id travels as
/// a hex STRING everywhere else in this system. Hashing the decoded bytes on one side and the string
/// on the other is the kind of mismatch that is invisible in review and fatal in production.
std::vector<unsigned char> release_message(const std::string& sid,
                                           const std::vector<unsigned char>& nonce32);

/// Verify an R2 release: rebuilds the tagged message and checks the signature under `xonly`.
bool verify_release(const std::vector<unsigned char>& xonly,
                    const std::string& sid,
                    const std::vector<unsigned char>& nonce32,
                    const std::vector<unsigned char>& sig);

// ── [REQ-61] LATCH ENFORCEMENT ───────────────────────────────────────────────────────────────────

/// The domain tag for a latch authorisation. Distinct from `RELEASE_TAG` so a release can never be
/// replayed as a co-signature authorisation, or the reverse — that is the entire purpose of a tag.
inline constexpr const char* LATCH_TAG = "utexo/leaf_latch/v1";

/// The message an owner signs to authorise ONE co-signature: `tagged(LATCH_TAG, sid ‖ session)`.
///
/// **The session is in the message on purpose.** A signature that authorised "any co-signature under
/// this sid" would be a bearer token: capture it once and replay it against a different transaction
/// forever. Binding the 133-byte session makes the authorisation specific to the exact signing round
/// it was issued for, and the session is already what witness binding ties to a transaction — so
/// owner consent, the session, and the transaction are one chain rather than three assertions.
std::vector<unsigned char> latch_message(const std::string& sid,
                                         const std::vector<unsigned char>& session);

/// Verify an owner's authorisation for this signing round under the coin's latch key.
bool verify_latch(const std::vector<unsigned char>& latch_key,
                  const std::string& sid,
                  const std::vector<unsigned char>& session,
                  const std::vector<unsigned char>& sig);

}  // namespace auth
