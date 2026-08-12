//! **The two enforcers of the spend budget must be handed the SAME quantity.** [D8(i)]
//!
//! The budget is enforced twice, on purpose: the coordinator refuses to co-sign past it, and the
//! enclave holds and attests its own copy so a receiver's census rests on something other than the
//! word of the party they are being protected from. Two enforcers, one rule — and therefore one
//! number, or the redundancy becomes a way to disagree.
//!
//! They were given different numbers. `set_spend_budget` takes `remaining`, a RELATIVE count ("one
//! more co-signature from here"). `set_sig_budget` converts it to the ABSOLUTE
//! `count_finalized_signatures + remaining` and stores THAT, because the enclave's check is
//! `sig_count >= sig_budget` against its own LIFETIME count (`lockbox/src/server.cpp:145-151`). The
//! mirror POSTed `payload.0.remaining` — the relative value — so the enclave read it as "this coin
//! may never exceed 1 co-signature in its entire life".
//!
//! Every coin arriving at a split has more than that: a deposit that has been laddered already
//! co-signed its trigger, its extension and its state. Measured on the live regtest stack, one hop
//! in front of a split:
//!
//! | enforcer    | budget | count | verdict          |
//! |-------------|--------|-------|------------------|
//! | coordinator | 7      | 6     | would allow      |
//! | enclave     | **1**  | 6     | TERMINAL, refuse |
//!
//! Same 6. Two different budgets, from one call.
//!
//! And the failure is not a clean refusal. Both budgets are **monotone ratchets** and the
//! coordinator's write lands FIRST, so by the time the enclave refuses, the coin is already terminal
//! at the coordinator and pinned at 1 in the enclave — where the ratchet (`sig_budget IS NULL OR
//! sig_budget > $1`) means nothing can ever raise it. The split does not happen and the coin is
//! exit-only, permanently. That is the whole in-ladder split lane, for every coin, as soon as the
//! enclave-side budget shipped.
//!
//! This guard is a source pin because the defect lives in one argument of one HTTP call: there is no
//! value to assert on without a Rocket instance, a Postgres and an enclave. It pins the shape that
//! was wrong — never mirror the request field; always mirror what the database actually stored.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

#[test]
fn the_enclave_is_mirrored_the_stored_absolute_budget_not_the_relative_request() {
    let path = repo_root().join("server/src/endpoints/lightning_latch.rs");
    let src = std::fs::read_to_string(&path).expect("lightning_latch.rs is readable");

    // Scope to `set_spend_budget` — the only place the mirror happens, and the only place
    // `remaining` and the stored budget are both in scope to be confused for one another.
    let body = src
        .split_once("pub async fn set_spend_budget")
        .expect("set_spend_budget exists")
        .1;
    let body = body.split_once("\npub ").map(|(b, _)| b).unwrap_or(body);

    let mirror = body
        .find("/sig_budget")
        .map(|i| &body[i..])
        .expect("the enclave mirror POST exists — without it the enclave has no budget at all");
    // CODE ONLY. The comment explaining this very fix names `payload.0.remaining` a few lines above
    // the call, so a raw substring search matches the prose warning against the bug rather than the
    // bug — which is exactly how this assertion first failed against the corrected source. Strip
    // `//` lines, then bound the window to the call itself.
    let code: String = mirror
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let window = &code[..code.len().min(400)];

    assert!(
        !window.contains("payload.0.remaining"),
        "the enclave is being mirrored the RELATIVE `remaining` instead of the ABSOLUTE budget the \
         coordinator stored. The enclave compares it against its own LIFETIME sig_count, so this \
         reads as 'this coin may never exceed `remaining` co-signatures in its life' — and because \
         both budgets are monotone ratchets and the coordinator's write lands first, the coin ends \
         up terminal at the coordinator, pinned low at the enclave, and exit-only forever.\n\
         Mirror `budget` (the return value of `set_sig_budget`) instead.\n\nfound:\n{window}"
    );
    assert!(
        window.contains("\"sig_budget\": budget"),
        "expected the mirror to send the stored absolute budget as `\"sig_budget\": budget`; the \
         coordinator and the enclave must enforce the same number.\n\nfound:\n{window}"
    );
}

/// The premise the assertion above rests on: `set_sig_budget` really does return the ABSOLUTE
/// budget. If it were ever changed to return the relative `remaining`, mirroring `budget` would be
/// just as wrong while still satisfying the pin — so the pin needs this second half to mean anything.
#[test]
fn the_stored_budget_is_absolute() {
    let path = repo_root().join("server/src/database/deposit.rs");
    let src = std::fs::read_to_string(&path).expect("deposit.rs is readable");
    let body = src
        .split_once("pub async fn set_sig_budget")
        .expect("set_sig_budget exists")
        .1;
    let body = body.split_once("\npub ").map(|(b, _)| b).unwrap_or(body);
    assert!(
        body.contains("count + remaining"),
        "`set_sig_budget` no longer computes an ABSOLUTE budget as `count + remaining`. If it now \
         stores something relative, the enclave mirror must change with it — the two enforcers \
         compare against the same lifetime signature count and must hold the same number.\n\n{body}"
    );
}
