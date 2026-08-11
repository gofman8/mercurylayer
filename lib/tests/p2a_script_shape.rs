#[test]
fn p2a_script_is_51024e73_and_240_sats() {
    let s = mercurylib::tesr::p2a_script();
    assert_eq!(hex::encode(s.as_bytes()), "51024e73",
        "the anchor must be exactly OP_1 <0x4e73> — this is the scriptPubKey Core special-cases");
    assert_eq!(mercurylib::tesr::P2A_VALUE, 240);
}
