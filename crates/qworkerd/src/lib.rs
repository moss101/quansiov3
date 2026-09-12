//! Worker daemon: tool host, terminal, checkpoints and managed browser (CDP).
//!
//! Canonical owner (DOSSIER.md §17): `crates/qworkerd`.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/qworkerd";
