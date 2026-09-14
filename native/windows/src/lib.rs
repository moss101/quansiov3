//! Windows native broker bridging UI Automation and the credential store to the Rust machine module.
//!
//! Canonical owner (DOSSIER.md §17): `native/windows`.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "native/windows";

pub mod computer_use;
