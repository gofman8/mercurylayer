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
//! **The refusal is now PERMANENT, not a placeholder.** When this guard was written the coloured
//! legs did not exist anywhere and the refusal stood in for them. They exist now
//! (`build_colored_spine_batch` + `cosign_colored_spine_batch`), and they are a SEPARATE pair rather
//! than a coloured mode of this driver — because a coloured leg is a pre-built, pre-coloured
//! transaction that the co-sign signs, not something `establish_*_journalled` can mint from the SE.
//! So this refusal keeps its job for good: it is the sign at the fork saying the plain driver is the
//! wrong road, and the assertion below now also checks it NAMES the right one.

use std::path::PathBuf;

fn tesr_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("clients/libs/rust/src/tesr.rs");
    std::fs::read_to_string(&p).expect("tesr.rs is readable")
}

#[test]
fn the_plain_batch_driver_refuses_a_coloured_batch_and_names_the_right_one() {
    let src = tesr_src();
    let body = {
        let at = src.find("\nasync fn spine_batch_split_ex(").expect("spine_batch_split_ex exists");
        &src[at..at + src[at..].find("\n/// ").unwrap_or(9000)]
    };

    assert!(
        body.contains("a COLOURED spine batch must not be driven through the PLAIN batch driver"),
        "`spine_batch_split_ex` accepts a coloured tip + coloured `SP` and then builds every leg \
         with the PLAIN `establish_child_journalled` / `establish_spine_tip_journalled`, persisting \
         each with `rgb: None`. An uncoloured tier over `SP.out[j]` — where the allocation is booked \
         after the batch — DESTROYS it rather than refusing. This combination must be refused by \
         name."
    );
    // A refusal that does not say where to go instead is how someone concludes the feature is
    // missing and writes a second, worse version of it.
    assert!(
        body.contains("cosign_colored_spine_batch"),
        "the refusal must NAME the coloured pair, or the next reader concludes the coloured batch \
         does not exist — which is exactly what the original refusal (correctly, at the time) said, \
         and it is no longer true"
    );
    // The coloured pair really is there.
    assert!(
        src.contains("pub fn build_colored_spine_batch(")
            && src.contains("pub async fn cosign_colored_spine_batch("),
        "the refusal points at a coloured pair that does not exist"
    );
    // …and the refusal must be reachable: a coloured tip has to survive the two mismatch arms to
    // reach it, so those must still be the ones checking the OTHER two combinations.
    assert!(
        body.contains("(true, false)") && body.contains("(false, true)"),
        "the lane-agreement match no longer distinguishes the mismatched combinations, so the \
         `(true, true)` refusal may not be reachable"
    );
}
