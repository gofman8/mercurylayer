//! **A derived-slot voucher may only become a deposit address through `create_child_slot_addr`.**
//!
//! The pool of SE-minted slot vouchers is durable, and its two halves have to move together: a
//! voucher that `deposit/init` consumed must LEAVE the pool, and a voucher whose init failed must
//! STAY (counted). `create_child_slot_addr` is where that pairing lives. Eleven call sites reached
//! past it to `deposit::get_deposit_bitcoin_address` directly, so every one of them spent a voucher
//! at the SE and left it on disk.
//!
//! # Why that is worse than it sounds
//!
//! `take_derived_tokens` mints only when `pool.len() < n`. A poisoned pool therefore looks FULL, so
//! nothing is minted and the dead ids are handed straight back — and they sit at the FRONT of the
//! pool, so they are handed out first, every time. Each one then fails `Token already spent` for
//! `SLOT_VOUCHER_FAILURE_LIMIT` attempts before the self-healing counter drops it. One RGB
//! token-piece payment leaves two behind.
//!
//! Measured on the live stack before the fix: `sdk16` died on the BTC transfer that follows an RGB
//! onboarding, re-using the exact voucher the 3,066-sat token piece had already spent
//! (`[TOKPROBE] … token=021c9546 amount=3066` then `token=021c9546 amount=15000`). `sdk37` and
//! `sdk39` failed the same way. `sdk21` is the one that shows the shape of the real-world cost: the
//! SSP **paid its Lightning invoice** and then claimed zero transfers, because the slot the swap
//! needed could not be created one hop back.
//!
//! This is the silent-degradation class again — the failure surfaces as a stale-looking
//! "already spent" from the coordinator, several operations away from the code that caused it.

use std::path::PathBuf;

fn sdk_src(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("clients/libs/rust-sdk/src")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The files that hold derived-slot flows. `wallet.rs` is included deliberately: it owns the pool
/// and `get_deposit_address`, the one legitimate ONBOARDING caller.
const FILES: [&str; 4] = ["transfer.rs", "tokens.rs", "refresh.rs", "wallet.rs"];

/// THE CENSUS. Every direct call to `deposit::get_deposit_bitcoin_address` outside the two
/// legitimate homes is a bypass.
#[test]
fn no_flow_turns_a_voucher_into_an_address_behind_the_pool() {
    // The legitimate direct callers, by the token they spend:
    //   * `create_child_slot_addr` (transfer.rs) — THE derived-slot door, which owns the pairing;
    //   * three ONBOARDING sites that draw on `take_token()`, never the voucher pool.
    const ONBOARDING_ALLOWANCE: usize = 3;

    let mut direct = 0usize;
    for f in FILES {
        direct += code_only(&sdk_src(f))
            .matches("mercuryrustlib::deposit::get_deposit_bitcoin_address(")
            .count();
    }
    assert_eq!(
        direct,
        ONBOARDING_ALLOWANCE + 1,
        "there are {direct} direct calls to `deposit::get_deposit_bitcoin_address` across {FILES:?}, \
         but only {} may exist: `create_child_slot_addr` plus {ONBOARDING_ALLOWANCE} onboarding \
         sites that draw on `take_token()`. A new one is almost certainly a DERIVED flow that will \
         spend its voucher at the SE and leave it in the durable pool — where the next \
         `take_derived_tokens` hands it straight back, because a poisoned pool looks full.",
        ONBOARDING_ALLOWANCE + 1
    );
}

/// NON-VACUITY. The census above is only meaningful while the derived flows really do go through the
/// door — a refactor that renamed the door would leave the count at 4 and mean nothing.
#[test]
fn every_derived_flow_goes_through_the_door() {
    let mut takes = 0usize;
    let mut through = 0usize;
    for f in FILES {
        let code = code_only(&sdk_src(f));
        takes += code.matches(".take_derived_tokens(").count();
        through += code.matches(".create_child_slot_addr(").count()
            + code.matches(".create_child_slot(").count();
    }
    assert!(
        takes >= 8,
        "only {takes} `take_derived_tokens` call sites found — this census has lost its subject"
    );
    assert!(
        through >= takes,
        "{takes} flows take derived vouchers but only {through} slot creations go through the door. \
         At least one flow is spending vouchers by some other route."
    );
}

/// THE PAIRING ITSELF, read off the code rather than off the doc comment.
#[test]
fn the_door_consumes_on_success_and_counts_on_failure() {
    let code = code_only(&sdk_src("transfer.rs"));
    let at = code
        .find("pub(crate) async fn create_child_slot_addr(")
        .expect("`create_child_slot_addr` is gone — the pairing has no home");
    let body = &code[at..at + code[at..].find("\n    pub(crate) async fn create_child_slot(").unwrap_or(3000)];

    assert!(
        body.contains("self.fail_slot_voucher(token_id)"),
        "the error arm does not COUNT the attempt. A voucher the SE already consumed would then be \
         retried forever and wedge every later batch behind a dead id:\n\n{body}"
    );
    assert!(
        body.contains("self.consume_slot_voucher(token_id)"),
        "the success arm does not REMOVE the spent voucher from the durable pool. That is the exact \
         defect this door exists to prevent:\n\n{body}"
    );
    // Order matters: consuming before the init would throw away a voucher whose init then failed,
    // which is the loss this pool exists to stop.
    let consume = body.find("self.consume_slot_voucher(token_id)").unwrap();
    let init = body
        .find("mercuryrustlib::deposit::get_deposit_bitcoin_address(")
        .expect("the door no longer calls `deposit/init`");
    assert!(
        init < consume,
        "the voucher is consumed BEFORE `deposit/init` is known to have succeeded — a failed init \
         would then throw away a voucher that is still spendable, charging the parent's lifetime \
         allowance for nothing"
    );
}
