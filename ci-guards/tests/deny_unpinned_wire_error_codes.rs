//! **[D38/D18] The wire error vocabulary is the interface, and every profile must carry all of it.**
//!
//! `TransferReceiverError`'s variants are compared as string literals in four client profiles across
//! five sites — `clients/libs/nodejs`, `clients/libs/web`, and both generated Kotlin trees — and
//! three other specs' conformance tables point at `ERR-n` entries that index into them. Prose is not
//! an interface; this enum is.
//!
//! Two things were wrong.
//!
//! **The wire values were a by-product of the Rust identifiers.** With no `serde(rename)`, an
//! identifier refactor — something nobody would think to call a protocol change — silently moves the
//! interface out from under four consumers. The renames now say the wire value explicitly, which
//! looks redundant and is exactly the point: the two are separable, so changing one becomes
//! deliberate.
//!
//! **The Kotlin bindings carried only two of three variants.** `TransferCancelledError` was absent,
//! so a Kotlin client meeting the shipped 410 could not deserialize the body at all — and that
//! variant exists precisely so a cancelled payment does not look like an idle mailbox. A conformance
//! failure, not a binding detail.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// The three variants, by their WIRE spelling. Adding one here without adding it to every profile
/// below is what this guard exists to make impossible.
const WIRE_CODES: [&str; 3] = [
    "StatecoinBatchLockedError",
    "ExpiredBatchTimeError",
    "TransferCancelledError",
];

/// Every variant carries an explicit rename equal to its own identifier, so the wire value survives
/// a Rust rename.
#[test]
fn every_variant_pins_its_wire_value() {
    let src = read("lib/src/transfer/receiver.rs");
    let at = src
        .find("pub enum TransferReceiverError {")
        .expect("`TransferReceiverError` is gone");
    let body = &src[at..at + src[at..].find("\n}").expect("the enum ends") + 2];
    for code in WIRE_CODES {
        assert!(
            body.contains(&format!("#[serde(rename = \"{code}\")]")),
            "`{code}` has no explicit serde(rename). Without it the wire value is a by-product of \
             the Rust identifier, and a refactor nobody calls a protocol change moves the interface \
             for four client profiles:\n\n{body}"
        );
    }
    // Non-vacuity: the enum really does have exactly these variants, so a NEW one cannot be added
    // unpinned and unnoticed.
    let renames = body.matches("#[serde(rename = ").count();
    assert_eq!(
        renames,
        WIRE_CODES.len(),
        "the enum has {renames} pinned variants but this guard knows {}. A new error code must be \
         added to WIRE_CODES here AND to every client profile below.",
        WIRE_CODES.len()
    );
}

/// EVERY PROFILE CARRIES EVERY CODE. This is the assertion that was false: the Kotlin trees had two
/// of three, so the shipped 410 was undeserializable there.
#[test]
fn every_client_profile_carries_every_code() {
    const PROFILES: [&str; 2] = [
        "clients/apps/kotlin/src/main/kotlin/mercurylib.kt",
        "lib/out-kotlin/com/mercurylayer/mercurylib.kt",
    ];
    for p in PROFILES {
        let src = read(p);
        for code in WIRE_CODES {
            assert!(
                src.contains(&format!("@SerialName(\"{code}\")")),
                "{p} does not carry `{code}`. A client meeting that response cannot deserialize the \
                 body at all — and `TransferCancelledError` in particular exists so a cancelled \
                 payment does not present as an idle mailbox, which is the silent-degradation shape \
                 a recipient must never be shown."
            );
        }
    }
}

/// The JS profiles compare the codes as literals. They need not carry all three (they branch only on
/// the batch-lock pair), but whatever they DO compare must be a real wire value — a typo there is a
/// branch that never fires.
#[test]
fn the_js_profiles_compare_real_wire_values() {
    for p in ["clients/libs/nodejs/transfer_receive.js", "clients/libs/web/transfer_receive.js"] {
        let src = read(p);
        for code in WIRE_CODES {
            // If the file mentions a code at all, it must be spelled exactly.
            let loose = code.trim_end_matches("Error");
            if src.contains(loose) {
                assert!(
                    src.contains(code),
                    "{p} references `{loose}` but not the exact wire value `{code}` — a mis-spelled \
                     literal is a branch that never fires"
                );
            }
        }
    }
}
