//! quansio-server: authoritative API, control, runtime, policy, effects,
//! scheduler and artifact composition.
//!
//! Canonical owner (DOSSIER.md §17): `crates/server`. Modules here retain strict
//! canonical ownership (DOSSIER.md §5) even though they ship as one deployment
//! unit; splitting them later requires an approved decision and must not change
//! ownership or contracts.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner.
pub const CANONICAL_OWNER: &str = "crates/server";

pub mod api;
pub mod artifacts;
pub mod audit;
pub mod control;
pub mod effects;
pub mod notify;
pub mod policy;
pub mod runtime;
pub mod scheduler;
pub mod usage;
