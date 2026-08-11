#[test]
fn live_se_attestation_verifies_against_the_chain_anchored_key() {
    // Captured from the RUNNING lockbox (/signature_count/<sid>?nonce=...) on 2026-08-11.
    // The pubkey below is the coin's own server_pubkey read from the enclave DB, i.e. the value the
    // receiver binds to the on-chain tx0 output — so this exercises the real trust path.
    let sid = "5eebee706045497daf9b8972c9aa4857";
    let nonce = "abababababababababababababababababababababababababababababababab";
    let sig = "0xb6e3c5cae313aab33b4f5668d08a5011254be73ec7951eaf2226610b083062ef4a58e18c385dc7ac9c312fd9064d8c870732837b30673ad17a335c923bed9525";
    let pk  = "0x9f6ea2593922431bf4528e714547ca968a406e8582908b473911178a8eb69d55";
    let anchored = "039f6ea2593922431bf4528e714547ca968a406e8582908b473911178a8eb69d55";
    assert!(mercurylib::transfer::receiver::verify_sig_count_attestation(
        sid, 2, nonce, sig, pk, anchored).is_ok(),
        "the LIVE SE attestation must verify against the coin's chain-anchored key");
    // And an under-reported count must not.
    assert!(mercurylib::transfer::receiver::verify_sig_count_attestation(
        sid, 1, nonce, sig, pk, anchored).is_err());
}
