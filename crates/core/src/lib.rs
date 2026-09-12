//! IDs, generation counters, idempotency keys and typed errors shared across the Quansio runtime.
//!
//! Canonical owner (DOSSIER.md §17): `crates/core`.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/core";
