//! WorkGraph, AgentGraph and StateGraph stores plus GraphTransaction.
//!
//! Canonical owner (DOSSIER.md §17): `crates/graph`.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/graph";
