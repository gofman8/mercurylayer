//! **[#145] A coin may not be offered to a payment unless this wallet holds material to EXIT it.**
//!
//! Every filter in `payment_coins` asks whether a coin is ELIGIBLE — confirmed, not a duplicate, not
//! an RGB carrier, worth more than its own renewal fee. None of them asks the separate question of
//! whether the wallet can actually *exit or convey* it:
//!
//! * a laddered coin's exit material is its bundle (`tesr-` root, `ctesr-` child, `spinetip-` tip);
//! * an un-laddered coin's is its flat backup chain;
//! * a coin with **neither** is a slot the SE knows about, that this wallet holds a key for, and
//!   that it can neither exit unilaterally nor hand on — most often a derived child slot whose split
//!   failed after the slot was minted.
//!
//! That third shape was offered to selection, and the failure surfaced at the FAR END of the
//! payment: `transfer_sender` reached for the flat backup rows, found none, and refused. `chaos22`'s
//! oracle could only class it as an unclassified breach — the wallet reported balance, the planner
//! promised it, and the send died. The remedy is upstream: the coin is never offered, so a different
//! coin funds the payment and nobody meets the refusal.
//!
//! # Why the shape of the check matters more than the check
//!
//! `parent_shape` reaches `Unladdered` through THREE CONSECUTIVE ABSENCES — no tip row, no child
//! row, no root row. This repo has already been bitten once by treating that as a positive answer
//! (the spine tip fell through all three and was routed as un-laddered, at the wrong floor, to the
//! [B1]-unsafe plain split). "Un-laddered" is a claim about a coin that still has a flat backup
//! chain; a coin with no chain either is not un-laddered, it is un-exitable, and the two must not
//! share a route.

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

/// THE FUNNEL. `payment_coins` is the synchronous half of the [B2] "one coin set" property; the
/// exit-material filter is the half that needs the database. If a call site takes the synchronous
/// half alone, it has the un-filtered set — and [B2]'s whole point is that `quote_transfer` and
/// `transfer` cannot look at different wallets.
#[test]
fn payment_coins_has_exactly_one_caller() {
    let code = code_only(&read("clients/libs/rust-sdk/src/transfer.rs"));
    // Word-boundary matching: `spendable_payment_coins(` CONTAINS `payment_coins(`, and counting the
    // wrapper's own definition and calls as violations would make this guard unsatisfiable.
    let calls: Vec<usize> = code
        .match_indices("payment_coins(")
        .map(|(i, _)| i)
        .filter(|i| {
            let before = &code[i.saturating_sub(1)..*i];
            before != "_" // not the tail of `spendable_payment_coins(`
        })
        .filter(|i| !code[i.saturating_sub(3)..*i].ends_with("fn ")) // not the definition
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "`payment_coins` has {} call sites; exactly one is allowed (inside \
         `spendable_payment_coins`). A caller taking the synchronous half alone gets coins with no \
         exit material — slots this wallet can neither exit nor convey — and the failure surfaces at \
         the far end of the payment as an unclassified breach rather than as a refusal to plan.",
        calls.len()
    );
}

/// THE PROOF EXISTS, and fails closed. `try_get_backup_txs` is the absence-vs-failure split: reading
/// a FAILED read as "no material" would silently retire a good coin from the spendable balance —
/// the same silent-degradation shape pointed the other way.
#[test]
fn the_exit_material_proof_reads_absence_not_failure() {
    let code = code_only(&read("clients/libs/rust-sdk/src/transfer.rs"));
    let at = code
        .find("pub(crate) async fn has_exit_material(")
        .expect("`has_exit_material` is gone — selection has lost its capability filter");
    let body = &code[at..code.len().min(at + 1_400)];
    assert!(
        body.contains("try_get_backup_txs("),
        "`has_exit_material` no longer uses `try_get_backup_txs`. `get_backup_txs` is a `fetch_one`, \
         so a MISSING row and a FAILED read are the same value there — and reading a failed read as \
         'no material' retires a perfectly good coin from the wallet's balance:\n\n{body}"
    );
    assert!(
        body.contains("ParentShape::Unladdered"),
        "`has_exit_material` no longer short-circuits on the laddered shapes; a bundle IS exit \
         material and re-deriving that probe here would be a second definition to drift:\n\n{body}"
    );
}

/// THE UN-LADDERED ROUTES. Both places that act on `ParentShape::Unladdered` by routing a coin to a
/// PLAIN split must prove the material first — that route ends at the flat sender, which needs the
/// backup chain a materialless slot does not have.
#[test]
fn every_plain_split_route_proves_its_material_first() {
    let code = code_only(&read("clients/libs/rust-sdk/src/transfer.rs"));
    let proofs = code.matches("self.has_exit_material(").count();
    assert!(
        proofs >= 3,
        "found {proofs} `has_exit_material` call sites; at least three are required — the shared \
         selection filter, the batch lane's candidate loop (`transfer_many`), and \
         `ensure_exact_coin`'s. The latter two build their OWN candidate lists and would otherwise \
         re-introduce the defect one lane over."
    );
}

/// **THE REFUSAL MUST BE CALLER-INDEPENDENT.** This is the half of #145 that was NOT a selection
/// bug, and it is the more dangerous half.
///
/// The spine-tip refusal lived in exactly one caller — `UtexoWallet::transfer`'s handover loop.
/// `transfer_sender::execute` is public, `chaos22`'s `respend` calls it directly, and a direct
/// caller has no dispatch: the tip walked into the flat lane, whose classifier LICENSES it
/// (`FundingNotOnChain` — a tip's funding output is un-broadcast, exactly like a `ctesr-` child's).
/// What stopped the conveyance was an ABSENCE, the missing backup rows.
///
/// Refusal-by-absence is one guard away from a money loss: a flat conveyance of a tip hands the
/// recipient a backup chain over an outpoint that does not exist on chain and never will — a coin
/// with no exit, with no error on either side.
#[test]
fn the_flat_sender_refuses_a_spine_tip_itself() {
    let code = code_only(&read("clients/libs/rust/src/transfer_sender.rs"));
    let at = code
        .find("async fn execute_ex(")
        .expect("`execute_ex` is gone — this guard has lost its subject");
    let body = &code[at..];
    let refusal = body
        .find("load_spine_tip(")
        .expect(
            "`execute_ex` no longer refuses a spine tip itself. The refusal must NOT live only in \
             `UtexoWallet::transfer`'s dispatch: `execute` is public and a direct caller reaches \
             the flat lane, which LICENSES an un-broadcast funding output and would convey the tip \
             on a backup chain the recipient can never exit through.",
        );
    // …and it must come BEFORE the flat backup chain is built, which is where the absence lives.
    let rows = body
        .find("create_backup_transactions(")
        .expect("`execute_ex` no longer builds the flat backup chain — re-derive this guard");
    assert!(
        refusal < rows,
        "the spine-tip refusal runs AFTER `create_backup_transactions`. Then the tip is stopped by \
         the ABSENCE of backup rows rather than by a rule — refusal by accident, which reads to \
         `chaos22`'s oracle as an unclassified breach and is one missing guard from a silent money \
         loss."
    );
}

/// THE REMEDY IS NAMED, and named CORRECTLY. A stuck coin has a fee problem that combining rescues;
/// a materialless coin is missing the material itself and combining does nothing. Reporting them as
/// one count tells a user to run an operation that cannot work.
#[test]
fn the_two_exclusions_keep_two_remedies() {
    let sdk = read("clients/libs/rust-sdk/src/transfer.rs");
    assert!(
        sdk.contains("not rescuable by combining"),
        "the quote no longer distinguishes a materialless coin from a stuck one. Combining rescues \
         a coin whose value is below its renewal fee; it cannot conjure a backup chain."
    );
    let types = read("clients/libs/rust-sdk/src/types.rs");
    assert!(
        types.contains("pub no_exit_material_coins: Vec<String>"),
        "`TransferQuote` no longer reports the excluded coins. Silently withholding balance from \
         `fundable` with no field saying why is the shape that made this a chaos-oracle breach \
         rather than a message."
    );
}
