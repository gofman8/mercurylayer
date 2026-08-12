//! **A coloured spine batch must not build PLAIN legs under its coloured `SP`.** [S4b]
//!
//! S4 built the coloured split state (`build_colored_spine_batch_sp`) and the entry point
//! (`spine_batch_split_colored`), and `spine_batch_split_ex` correctly refuses the two mismatched
//! lanes: an uncoloured `SP` over a coloured tip, and a coloured `SP` over a plain tip. The refusal
//! it did not have is the one for the case where both agree.
//!
//! Below `SP`, every leg is still constructed by `establish_child_journalled` /
//! `establish_spine_tip_journalled` — the PLAIN builders — and each persisted record is written with
//! `rgb: None`. That is an UNCOLOURED tier over `SP.out[j]`, which after the batch is where the
//! allocation lives. It is the identical construction the "coloured tip, uncoloured SP" arm exists
//! to prevent, one level further down, and its consequence is worse than a refusal: the allocation
//! is BURNED rather than declined.
//!
//! It was latent, not live — `spine_batch_split_colored` has no callers in the repo — which is
//! exactly when this is cheapest to close and easiest to forget. The refusal is a no-op for every
//! path that exists today.
//!
//! **This guard retires itself.** It asserts the refusal is present *while* no coloured leg builder
//! exists. Build one and this test tells you, by name, to remove the refusal along with it — so the
//! guard cannot become the thing that blocks the feature it is protecting.

use std::path::PathBuf;

fn tesr_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("clients/libs/rust/src/tesr.rs");
    std::fs::read_to_string(&p).expect("tesr.rs is readable")
}

/// Does a builder that constructs COLOURED legs for a spine batch exist yet?
///
/// The marker is a function whose name says so. Deliberately a name test and not a behaviour test:
/// the point is to notice when the capability arrives, and the person who adds it is the person who
/// should delete the refusal.
fn coloured_leg_builder_exists(src: &str) -> bool {
    ["fn establish_colored_child_journalled(", "fn establish_colored_spine_tip_journalled("]
        .iter()
        .any(|needle| src.contains(needle))
}

#[test]
fn the_coloured_batch_refuses_until_its_legs_are_actually_coloured() {
    let src = tesr_src();
    let body = {
        let at = src.find("\nasync fn spine_batch_split_ex(").expect("spine_batch_split_ex exists");
        &src[at..at + src[at..].find("\n/// ").unwrap_or(9000)]
    };

    if coloured_leg_builder_exists(&src) {
        // The capability landed. Now the refusal is the bug.
        assert!(
            !body.contains("the COLOURED spine batch is not implemented below `SP`"),
            "a coloured leg builder now exists, so the `(true, true)` refusal in \
             `spine_batch_split_ex` is stale and is blocking the feature it was protecting — delete \
             it in the same commit that lands the legs, and replace this guard with one that pins \
             the legs are genuinely coloured (an `rgb: Some(..)` on every persisted record)"
        );
        return;
    }

    assert!(
        body.contains("the COLOURED spine batch is not implemented below `SP`"),
        "`spine_batch_split_ex` accepts a coloured tip + coloured `SP` and then builds every leg \
         with the PLAIN `establish_child_journalled` / `establish_spine_tip_journalled`, persisting \
         each with `rgb: None`. An uncoloured tier over `SP.out[j]` — where the allocation is booked \
         after the batch — DESTROYS it. Until a coloured leg builder exists, this combination must \
         be REFUSED by name rather than silently burning the allocation."
    );
    // …and the refusal must be reachable: a coloured tip has to survive the two mismatch arms to
    // reach it, so those must still be the ones checking the OTHER two combinations.
    assert!(
        body.contains("(true, false)") && body.contains("(false, true)"),
        "the lane-agreement match no longer distinguishes the mismatched combinations, so the \
         `(true, true)` refusal may not be reachable"
    );
}
