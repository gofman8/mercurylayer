//! [D7] Every network's schedule is NAMED, and an unrecognised one is not a guess.
//!
//! `TesrParams::for_network` used to map `bitcoin`/`mainnet` to the mainnet schedule and
//! **everything else** to regtest's toy one. That silently gave testnet and signet `d0 = 24` — about
//! four hours on a 10-minute-block network — a fact stated in no document, on the profile the
//! deployed coordinator actually runs (`server/Settings.toml`: `network = "testnet"`). It also meant
//! a typo fell through to the toy schedule instead of being caught.
use mercurylib::tesr::TesrParams;

#[test]
fn every_supported_network_is_named_explicitly() {
    for n in ["bitcoin", "mainnet", "BITCOIN", "MainNet"] {
        let p = TesrParams::for_network_checked(n).expect("mainnet must be recognised");
        assert_eq!(p.d0, 1440, "{n} must get the mainnet schedule");
        assert_eq!(p.m_max, 15);
    }
    for n in ["testnet", "testnet3", "testnet4", "signet"] {
        let p = TesrParams::for_network_checked(n).expect("must be recognised");
        assert_eq!(p.d0, 1440, "[D25] {n} has real ~10-minute blocks and must run the MAINNET schedule");
        assert_eq!(p.m_max, 15);
    }
    let r = TesrParams::for_network_checked("regtest").expect("regtest must be recognised");
    assert_eq!(r.d0, 24);
}

#[test]
fn an_unrecognised_network_is_none_not_a_guess() {
    // The whole point: a typo must be catchable. Before this, "mainet" silently produced a ~4-hour
    // ladder on real money.
    for bad in ["mainet", "bitcion", "", "liquid", "mutinynet"] {
        assert!(TesrParams::for_network_checked(bad).is_none(),
            "{bad:?} must NOT resolve to a schedule — an unrecognised network is not a guess");
    }
}

#[test]
fn the_infallible_form_resolves_known_networks() {
    assert_eq!(TesrParams::for_network("mainnet").d0, 1440);
    assert_eq!(TesrParams::for_network("testnet").d0, 1440);
    assert_eq!(TesrParams::for_network("regtest").d0, 24);
}

#[test]
#[should_panic(expected = "refusing to guess a TES-R schedule")]
fn an_unknown_network_panics_rather_than_guessing() {
    // [D25] It used to fall through to the TOY schedule, so "mainet" would have produced a ~4-hour
    // ladder on real money in silence. Every timelock derives from this value, so a mis-set network
    // must fail as loudly as possible.
    let _ = TesrParams::for_network("mainet");
}

#[test]
fn only_regtest_keeps_the_test_scale_schedule() {
    // [D25] Regtest mines on demand, so this is where E2E speed comes from and it is preserved
    // EXACTLY. Every other network runs what ships.
    assert_eq!(TesrParams::regtest().d0, 24, "E2E speed depends on this staying small");
    assert_eq!(TesrParams::testnet(), TesrParams::mainnet(),
        "public test networks must exercise the schedule that ships");
    assert_ne!(TesrParams::testnet().d0, TesrParams::regtest().d0);
}
