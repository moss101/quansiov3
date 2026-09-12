//! quansio CLI over the public API v1.
//!
//! Canonical owner (DOSSIER.md §17): `crates/cli`.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/cli";
