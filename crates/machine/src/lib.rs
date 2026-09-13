//! Machine control, worker gateway, secret broker and egress broker.
//!
//! Canonical owner (DOSSIER.md §17): `crates/machine`.
//!
//! `control` is the machine-control authority (EXEC-001): the execution-target lifecycle, the lease a
//! controller must hold to steer a target, and the fences — generation and lease expiry — that keep a
//! stale controller harmless. Nothing here decides what to run; that is the runtime's.
#![forbid(unsafe_code)]

pub mod control;

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/machine";
