//! Machine control, worker gateway, egress broker and secret broker.
//!
//! Canonical owner (DOSSIER.md §17): `crates/machine`.
//!
//! `control` is the machine-control authority (EXEC-001): the execution-target lifecycle, the lease a
//! controller must hold to steer a target, and the fences — generation and lease expiry — that keep a
//! stale controller harmless. `gateway` is the typed envelope that reaches a worker and the rules that
//! gate it (EXEC-002). `egress` is the broker that decides which destinations an execution target may
//! reach, and denies by default (EXEC-008). `secrets` is the secret broker: opaque handles and the
//! one boundary where material is resolved (EXEC-007). Nothing here decides what to run; that is the
//! runtime's.
#![forbid(unsafe_code)]

pub mod control;
pub mod egress;
pub mod gateway;
pub mod secrets;

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/machine";
