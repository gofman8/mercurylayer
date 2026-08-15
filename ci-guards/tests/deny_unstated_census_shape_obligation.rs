//! **[D47] The shape rules that discharge the census's distinctness premise must SAY they do.**
//!
//! `se_num_sigs == flat_backups + tiers + superseded` is an exact equality, and it is only sound if
//! the counted categories are pairwise distinct — no object countable as both a flat backup and a
//! tier. That premise is discharged by SHAPE: a flat backup is nVersion 2 / nSequence 0 / height
//! `nLockTime` / one non-`OP_RETURN` output; a tier is nVersion 3 / `nLockTime` 0 / one 240-sat
//! anchor / CSV in band. Nothing satisfies both.
//!
//! Every one of those rules exists for a TRANSPORT reason — relay, the race, TRUC. So the failure
//! mode is not that someone attacks the census; it is that someone relaxes a shape rule for a
//! perfectly good relay-side reason, the categories stop being distinguishable, and the census
//! silently starts counting one thing as another.
//!
//! D47 discharges the premise by SAYING SO where the change would happen, rather than by adding a
//! runtime check that re-derives a structurally true property at every claim. This guard is what
//! makes "we said so" durable.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// THE STATEMENT EXISTS, and names all four premises.
#[test]
fn the_census_completeness_theorem_is_stated_with_its_four_premises() {
    let spec = read("docs/utexo/spec/SPEC.md");
    assert!(
        spec.contains("CENSUS COMPLETENESS"),
        "SPEC.md no longer states the census-completeness theorem. A11 published as a bare \
         assumption is the load-bearing sentence a reviewer stops at."
    );
    for premise in ["A3", "CO-1", "concurrency is 1 per key", "PAIRWISE DISTINCT"] {
        assert!(
            spec.contains(premise),
            "the census-completeness theorem no longer names the premise `{premise}`. A theorem \
             whose premises are not enumerated is an assumption with more words."
        );
    }
    // CO-1 must be named as the premise that is NOT discharged — the honest half.
    assert!(
        spec.contains("is the one that is NOT discharged"),
        "the theorem no longer says which premise is unmet. Listing four premises and quietly \
         discharging all four would be worse than publishing A11 bare."
    );
}

/// THE OBLIGATION IS ATTACHED TO THE SHAPES, which is the part that has to survive.
#[test]
fn the_shape_rules_are_stated_to_carry_a_census_obligation() {
    let spec = read("docs/utexo/spec/SPEC.md");
    assert!(
        spec.contains("CENSUS obligation and not only a relay/race one"),
        "the shape rules no longer state that they carry a CENSUS obligation. That sentence is the \
         entire mechanism of D47: without it, the next person to relax nVersion/nSequence/nLockTime \
         for a relay reason has no way to know they are touching the census."
    );
    // …and the discriminating fields are named, so the obligation is checkable rather than vague.
    for field in ["nVersion 2", "nVersion 3", "nSequence 0", "240-sat"] {
        assert!(
            spec.contains(field),
            "the shape obligation no longer names `{field}`. An obligation over unnamed fields \
             cannot be re-checked when one of them changes."
        );
    }
}

/// THE REJECTED ALTERNATIVE stays rejected, in writing. A runtime distinctness check is the obvious
/// thing to add later; the reason not to must outlive whoever decided it.
#[test]
fn the_runtime_check_stays_explicitly_rejected() {
    let spec = read("docs/utexo/spec/SPEC.md");
    assert!(
        spec.contains("runtime distinctness check is deliberately NOT specified"),
        "the rejection of a runtime distinctness check is gone from SPEC.md. It is the obvious \
         addition, and without the reason recorded it will be proposed again — paying at every \
         claim to re-derive what the shapes already guarantee."
    );
}
