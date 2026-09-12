//! Tool contract, Tool Registry and dispatch descriptors shared by the runtime and qworkerd.
//!
//! Canonical owner (DOSSIER.md §17): `crates/tools`. This crate is pure: it holds the
//! Tool declarations (DOMAIN.md §7.5), validates proposed arguments strictly, derives the
//! effect class, resource selector, parameter digest and idempotency key for a call, and
//! filters the registry by a capability projection. It never dispatches, never opens a
//! transaction and never decides policy — the trusted runtime does those.
#![forbid(unsafe_code)]

pub mod call;
pub mod canonical;
pub mod declaration;
pub mod error;
pub mod registry;
pub mod schema;

pub use call::{plan_call, CallContext, ToolCallPlan};
pub use canonical::canonical_json;
pub use declaration::{
    EffectClassRule, EvidenceCapture, IdempotencyRule, NamedDerivation, RawEvidenceCapture,
    RawIdempotencyRule, RawResourceRule, RawToolDeclaration, ResourceRule, SourceTrust,
    ToolDeclaration, ToolHost,
};
pub use error::ToolError;
pub use registry::{RawFamilyReservation, ToolRegistry};
pub use schema::{check_vocabulary, first_violation, validate_instance, SchemaViolation};

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/tools";
