//! **The cryptographic prerequisite for SE-side witness binding must stay present.**
//!
//! # What this pins, and why it is the first thing the round needs
//!
//! `SPEC.md` §5.4's enforcement rests on the SE being able to *reconstruct* the signing session from
//! a disclosed transaction and byte-compare it against the session it was asked to sign. Without
//! that, the SE is a pure blind signer and cannot police anything about a collapse.
//!
//! That reconstruction needs one symbol the upstream library does not have: a **blinded**
//! `nonce_process` matching the blinded partial-sign the lockbox already calls. It exists in the
//! pinned fork:
//!
//! ```text
//! int secp256k1_blinded_musig_nonce_process_without_keyaggcoeff(
//!     ctx, session, aggnonce, msg32, aggregate_pubkey, adaptor, blinding_factor, tweak32)
//! ```
//!
//! `msg32` is where the BIP-341 sighash of the disclosed transaction goes. So the disclosure must
//! carry exactly: the aggregate nonce, the aggregate pubkey, the blinding factor, the output tweak,
//! and enough of the transaction to recompute the sighash.
//!
//! # What this guard can and cannot do
//!
//! It pins the two facts that are checkable from this repository: the fork the lockbox builds
//! against, and that the lockbox's signing path uses the blinded API family rather than the plain
//! one. It CANNOT confirm the symbol is exported — that requires the fetched fork, which exists only
//! inside the built container. Verified there directly:
//!
//! ```text
//! docker exec <lockbox> grep -n nonce_process \
//!   /app/build/secp256k1-zkp/src/secp256k1_zkp_external/include/secp256k1_musig.h
//! ```
//!
//! If the tag below moves, that verification must be re-run before relying on witness binding — and
//! this guard failing is what forces the question.

use std::fs;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// The fork and tag the enclave builds against. Moving either invalidates the verification above.
#[test]
fn the_lockbox_still_builds_against_the_blinded_musig_fork() {
    let cmake = read("lockbox/cmake/secp256k1_zkp.cmake");
    assert!(
        cmake.contains("blinded-musig-scheme"),
        "the lockbox no longer pins the `blinded-musig-scheme` tag. That fork is the only source of \
         `secp256k1_blinded_musig_nonce_process_without_keyaggcoeff`, which is what lets the SE \
         reconstruct a signing session from a disclosed transaction. Without it the SE cannot \
         witness-bind anything, and every enforcement rule in SPEC.md §5.4 is unbuildable.\n\n\
         If this move is deliberate, confirm the replacement exports a blinded `nonce_process` \
         before relying on §5.4."
    );
}

/// The signing path must use the BLINDED family. The plain `musig_nonce_process` has no blinding
/// factor, so a session built with it cannot be reconstructed from a disclosure.
#[test]
fn the_signing_path_uses_the_blinded_api_family() {
    let enclave = read("lockbox/src/enclave.cpp");
    assert!(
        enclave.contains("secp256k1_blinded_musig_partial_sign"),
        "the enclave's signing path is not on the blinded MuSig2 API. The blinded partial-sign and \
         the blinded `nonce_process` are a matched pair: witness binding reconstructs the session \
         with the latter and compares it against what the former was asked to sign. Mixing families \
         makes that comparison meaningless."
    );
}

/// NON-VACUITY: the strings this guard keys on must actually be the ones in the tree, or it is
/// asserting against nothing — the failure mode every source-scanning test in this repo has hit.
#[test]
fn the_guard_reads_files_that_exist_and_are_not_empty() {
    for f in ["lockbox/cmake/secp256k1_zkp.cmake", "lockbox/src/enclave.cpp"] {
        let s = read(f);
        assert!(s.len() > 200, "{f} is suspiciously small ({} bytes) — is it still the real file?", s.len());
    }
}
