//! Universal Effect Ledger: reservation, dispatch, settlement and reconciliation
//! (RUN-007, DOMAIN.md §7.2).
//!
//! Canonical owner (DOSSIER.md §5, §17): `crates/server/src/effects`. This module is the
//! single record of consequential actions and the single place that decides a retry. It
//! is bound to the effect-class taxonomy in `config/effects.yaml`, reserves an
//! `EffectRecord` with a derived idempotency key, dispatches it under a token, settles
//! it once, reconciles unknown outcomes according to the class strategy and refuses
//! every blind retry. Every transition is one event-emitting PostgreSQL transaction; the
//! run's durable protocol state is updated in that same transaction so recovery
//! reconciles instead of retrying.
#![forbid(unsafe_code)]

/// Repository path of this module's canonical owner, used by the workspace conformance
/// check to prove one owner maps to exactly one module.
pub const EFFECTS_OWNER: &str = "crates/server/src/effects";

/// The event family this module writes (DOMAIN.md §9.2).
pub const EFFECTS_OWNER_EVENTS: &[&str] = &["effect.*"];

pub mod error;
pub mod ledger;
pub mod model;
pub mod taxonomy;

pub use error::EffectError;
pub use ledger::EffectLedger;
pub use model::{
    EffectAuthorization, EffectOutcome, EffectRecord, EffectResource, EffectStatus, EffectTarget,
    NewEffect, OutcomeKind, ReconciliationEvidence, ReconciliationRecord, ReconciliationStrategy,
    RetryAuthorization, TargetKind, ToolCallRef,
};
pub use taxonomy::{EffectTaxonomy, EffectTaxonomyEntry, DOMAIN_EFFECT_CLASSES};
