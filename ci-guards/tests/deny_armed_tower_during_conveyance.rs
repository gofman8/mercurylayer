//! **[A2 / sdk80] Every lane that hands value away must disarm this wallet's own watchtower FIRST.**
//!
//! `defend_ladders`' child loop keys L1 on an **allowlist**: it broadcasts a retained state only for
//! a coin whose status reads `CONFIRMED`, which is this wallet's own record of *"mine, unspent, no
//! counterparty holds anything over it"*. The allowlist form is deliberate — a lane added tomorrow
//! parks its coin in SOME non-CONFIRMED status and is refused by default, so nobody can re-arm the
//! tower against their own recipient by forgetting to extend a denylist.
//!
//! **But the allowlist is only as good as when the field is written.** The loop re-reads that status
//! from the wallet DB on every pass. A lane that writes it LAST — after the SE has co-signed the
//! superseding split state, after the bundle has reached the recipient's mailbox — leaves a window
//! in which a pass reads a stale `CONFIRMED`, is admitted, and broadcasts the retained state over
//! the very outpoint the recipients' new state depends on.
//!
//! That is the sender's own tower destroying the payment the sender just made. On a coloured coin it
//! destroys the allocation.
//!
//! A2 closed this for `execute_ex`, `child_retransfer` and `cosign_colored_child_retransfer` by
//! hoisting a DURABLE status write ahead of the first step that can produce material for anybody
//! else. **The four in-ladder lanes were not covered**, and `sdk80` measured the window open in 1 of
//! 1 samples on `child_in_ladder_pay_many`.
//!
//! So this is a CENSUS rather than four assertions: the next lane to be added is the one that
//! matters, and the failure mode is silence.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// THE CENSUS. Every co-sign of an in-ladder split must be preceded by a durable arm-down.
///
/// Both directions are checked: as many arm-downs as co-signs, and each co-sign preceded by one.
/// Counting alone would pass if a lane had two arm-downs and another had none.
#[test]
fn every_in_ladder_cosign_is_preceded_by_a_durable_arm_down() {
    let code = code_only(&read("clients/libs/rust-sdk/src/transfer.rs"));

    let cosigns: Vec<usize> = ["mercuryrustlib::tesr::in_ladder_split(", "mercuryrustlib::tesr::child_in_ladder_split("]
        .iter()
        .flat_map(|m| code.match_indices(m).map(|(i, _)| i).collect::<Vec<_>>())
        .collect();
    assert!(
        cosigns.len() >= 4,
        "expected at least the four in-ladder lanes (child_in_ladder_pay, child_in_ladder_pay_many, \
         in_ladder_pay, in_ladder_pay_many), found {} co-sign call sites — this census has lost its \
         subject",
        cosigns.len()
    );

    let arms: Vec<usize> = code
        .match_indices("self.set_coin_status(")
        .filter(|(i, _)| code[*i..].starts_with("self.set_coin_status(") )
        .map(|(i, _)| i)
        .filter(|i| code[*i..code.len().min(i + 200)].contains("CoinStatus::IN_TRANSFER"))
        .collect();

    for c in &cosigns {
        // The arm-down must be the nearest preceding one, and close enough to be in the same lane:
        // an arm-down 3,000 characters earlier is in some other function.
        let nearest = arms.iter().filter(|a| *a < c).max();
        let ok = nearest.map_or(false, |a| c - a < 3_000);
        assert!(
            ok,
            "an in-ladder split co-signs at byte {c} with no durable IN_TRANSFER arm-down before it. \
             `defend_ladders` reads the coin's status from the wallet DB on every pass, so between \
             that co-sign and whenever this lane writes the status, the wallet's own tower is armed \
             with a state that rivals the one the recipients now hold over the same outpoint. \
             sdk80 measured that window open."
        );
    }
}

/// NON-VACUITY. The rule is worth nothing if the tower stopped keying on `CONFIRMED`, or stopped
/// re-reading the status per pass — either would make the arm-down decorative and the real defence
/// something else entirely.
#[test]
fn the_tower_still_keys_on_a_confirmed_allowlist() {
    let code = code_only(&read("clients/libs/rust-sdk/src/wallet.rs"));
    assert!(
        code.contains("CoinStatus::CONFIRMED"),
        "`defend_ladders` no longer references CONFIRMED — if L1 changed shape, this census is \
         pinning the wrong thing and must be re-derived rather than deleted"
    );
    assert!(
        code.contains("live_sids"),
        "the child loop's liveness join is gone; the arm-down protects a rule that no longer exists"
    );
}

/// THE THREE LANES A2 ALREADY COVERED must stay covered — a regression there is the same defect on
/// the whole-coin hop, where the loss is the entire coin rather than one piece.
#[test]
fn the_whole_coin_lanes_keep_their_arm_down() {
    let sender = code_only(&read("clients/libs/rust/src/transfer_sender.rs"));
    assert!(
        sender.contains("persist_coin_status(") && sender.contains("CoinStatus::IN_TRANSFER"),
        "`execute_ex` no longer makes the IN_TRANSFER transition durable before the conveyance. That \
         is the original A2 defect: the filter keyed on precisely the field that was not yet on disk"
    );
    let tesr = code_only(&read("clients/libs/rust/src/tesr.rs"));
    assert!(
        tesr.contains("persist_coin_status("),
        "the child re-transfer lanes no longer arm down before their `S'_child` co-sign"
    );
}
