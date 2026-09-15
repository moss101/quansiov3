//! quansio-server: authoritative API, control, runtime, policy, effects,
//! scheduler and artifact composition.
//!
//! Canonical owner (DOSSIER.md §17): `crates/server`. Modules here retain strict
//! canonical ownership (DOSSIER.md §5) even though they ship as one deployment
//! unit; splitting them later requires an approved decision and must not change
//! ownership or contracts.
//!
//! `api` is the public v1 surface (APP-001): tenant scope, the §14 command catalog, the §10 read
//! projections and §15's error taxonomy, and `composition` is the root that builds the modules over one
//! pool for the `quansio-server` binary.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner.
pub const CANONICAL_OWNER: &str = "crates/server";

pub mod api;
pub mod artifacts;
pub mod audit;
pub mod composition;
pub mod control;
pub mod effects;
pub mod notify;
pub mod observability;
pub mod policy;
pub mod runtime;
pub mod scheduler;
pub mod usage;
