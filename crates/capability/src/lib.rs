//! Capability Projection: effective authority derivation and narrowing.
//!
//! Canonical owner (DOSSIER.md §17): `crates/capability`. This crate owns the
//! capability algebra of DOMAIN.md §6 — [`Grant`], the resource selector kinds, the
//! fixed projection layer order, the intersection/most-restrictive composition, the
//! [`CapabilityProjection`] and the authorized-decision rules — and nothing else. It is
//! pure logic plus the event-staging and row mapping that keep a persisted projection
//! and its `capability.projected` event in sync; it never opens a store of its own.
//!
//! Non-negotiable invariants enforced here (DOMAIN.md §6.3, §16):
//!
//! * a layer may only **remove or narrow**; a widening attempt is ignored and returned
//!   as a [`algebra::WideningRejection`] for the caller to record;
//! * a missing or unresolvable input **fails closed** with
//!   [`error::CapabilityError::InputsUnavailable`] and an empty projection;
//! * a **stale** projection (changed `inputs_digest` or past `expires_at`) cannot
//!   authorize dispatch and forces recomputation.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/capability";

pub mod algebra;
pub mod authorize;
pub mod delegation;
pub mod error;
pub mod grant;
pub mod layer;
pub mod persistence;
pub mod policy;
pub mod projection;
pub mod selector;

pub use algebra::{narrow, WideningReason, WideningRejection};
pub use authorize::{
    authorize, Authorization, AuthorizationReason, AuthorizationRequest, Decision,
};
pub use delegation::{
    check_narrowing, check_narrowing_grants, narrowing_violation, stage_delegated_projection,
    DelegationNarrowingCheck,
};
pub use error::{CapabilityError, InputUnavailableReason, StaleReason};
pub use grant::{Approval, BudgetRef, Constraints, EffectClass, Grant, Tier};
pub use layer::{inputs_digest, Layer, ProjectionInput};
pub use persistence::{
    stage_assembly_rejections, stage_projected_event, stage_widening_rejected_event,
    ProjectionArgument, ProjectionRow,
};
pub use policy::{PolicyDecision, PolicyDocument, PolicyRule, UserRule, UserRuleDecision};
pub use projection::{
    assemble, Assembly, CapabilityProjection, LayerInput, LayerPayload, ProjectionRequest,
    ProjectionSource, ProjectionSubject, SubjectKind,
};
pub use selector::ResourceSelector;
