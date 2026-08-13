//! **[#152] A token balance may not depend on the SHAPE an allocation arrived in.**
//!
//! `get_asset_balance` is chain-anchored: it settles an allocation when its witness is mined. Every
//! allocation in this design is deliberately UN-BROADCAST, so what the RGB engine can settle depends
//! on how the allocation reached the wallet — and it did. A wallet receiving the same asset twice
//! summed correctly when the second arrived as a whole-child forward and reported only the FIRST
//! when it arrived as a piece carved from a spine tip. Both children adopted, both carrying their
//! consignment, one balance.
//!
//! That is not a rounding difference; it is a balance whose value depends on a routing detail the
//! holder did not choose and cannot see.
//!
//! The fix is not to patch the engine's view but to stop asking it about material it structurally
//! cannot see. The wallet's OWN adopted records — root carriers (`tesr-`), children (`ctesr-`) and
//! spine tips (`spinetip-`) — each carry the consignment-derived amount the receiver validated at
//! claim. That is the authority a receiver's safety already rests on, so it is what the balance
//! reports.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

fn code_only(src: &str) -> String {
    src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// THE LEDGER EXISTS and covers all THREE shapes. Missing one is a silently-low balance for whoever
/// happens to hold that shape — which is exactly the defect, one shape over.
#[test]
fn the_ledger_balance_covers_every_shape_an_allocation_can_live_in() {
    let code = code_only(&read("clients/libs/rust-sdk/src/tokens.rs"));
    let at = code
        .find("pub async fn ledger_token_balances(")
        .expect("`ledger_token_balances` is gone — the balance is chain-anchored again");
    let body = &code[at..code.len().min(at + 2_500)];
    for (probe, shape) in [
        ("load_spine_tip(", "a spine TIP — the shape the original defect lost"),
        ("load_child(", "an adopted CHILD"),
        ("tesr::load(", "a root CARRIER"),
    ] {
        assert!(
            body.contains(probe),
            "the ledger no longer sums {shape}. A missing shape is a silently-low balance for \
             whoever holds it, which is the defect this function exists to close:\n\n{body}"
        );
    }
}

/// THE CONSUMER. A ledger nothing reads is a ledger that proves nothing.
#[test]
fn the_public_balance_consumes_the_ledger() {
    let code = code_only(&read("clients/libs/rust-sdk/src/tokens.rs"));
    let at = code.find("pub async fn get_token_balances(").expect("`get_token_balances` is gone");
    let body = &code[at..code.len().min(at + 2_000)];
    assert!(
        body.contains("ledger_token_balances()"),
        "`get_token_balances` no longer consults the ledger, so it is back to reporting only what \
         the chain-anchored engine can settle:\n\n{body}"
    );
    // Combined with `max`, deliberately: on-chain material the ledger does not track (an exited
    // allocation now living on a plain UTXO) must still be reported.
    assert!(
        body.contains(".max(settled)"),
        "the ledger no longer combines with the engine's settled figure via `max`. Replacing it \
         outright would drop an EXITED allocation, which lives on a plain UTXO and is exactly what \
         the engine tracks and the ledger does not:\n\n{body}"
    );
}

/// NON-VACUITY: the amounts must come from the bundle's `rgb` half — the CONSIGNMENT-derived figure
/// the receiver validated — and never from a sender-declared field.
#[test]
fn the_ledger_reads_the_consignment_derived_amount() {
    let code = code_only(&read("clients/libs/rust-sdk/src/tokens.rs"));
    let at = code.find("pub async fn ledger_token_balances(").unwrap();
    let body = &code[at..code.len().min(at + 2_500)];
    assert!(
        body.contains("r.amount") && body.contains("r.contract_id"),
        "the ledger no longer reads the bundle's `rgb` half. Any other source for the amount is a \
         number the sender chose:\n\n{body}"
    );
    // …and only over CONFIRMED, non-duplicate coins, or a spent or duplicated row inflates it.
    assert!(
        body.contains("CoinStatus::CONFIRMED") && body.contains("duplicate_index == 0"),
        "the ledger no longer restricts to confirmed, non-duplicate coins — a spent or duplicated \
         row would inflate the balance:\n\n{body}"
    );
}
