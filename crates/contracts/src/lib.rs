//! Generated Rust bindings for Quansio contracts; generated from `schemas/`, never hand edited.
//!
//! Canonical owner (DOSSIER.md §17): `crates/contracts`. The derivation chain is
//! `DOMAIN.md -> schemas/catalog -> schemas/proto|openapi|json -> generated bindings`
//! (GOV-004). CI fails on any regeneration diff, so this tree must stay generated.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner.
pub const CANONICAL_OWNER: &str = "crates/contracts";

pub mod generated;

/// Contract revision of the generated bindings; bumped only by additive changes.
pub const CONTRACT_VERSION: &str = "v1";
