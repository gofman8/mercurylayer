//! Repo invariants enforced as tests.
//!
//! This crate deliberately contains no code: the invariants live in `tests/`, each one driving a
//! standalone script under `scripts/ci/`. Keeping them dependency-free means they run in seconds
//! and cannot be broken by unrelated work-in-progress elsewhere in the workspace.
