//! Tool contract, Tool Registry and dispatch descriptors shared by the runtime and qworkerd.
//!
//! Canonical owner (DOSSIER.md §17): `crates/tools`.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/tools";
