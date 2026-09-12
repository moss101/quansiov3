//! Delegation narrowing (DOMAIN.md §4.3, §6.3; RUN-002 seam).
//!
//! A child agent thread's authority must be a narrowing of its parent's. This is the
//! same predicate delegation, skills, tools and capability packs use: every candidate
//! grant must be at most as permissive as some grant the parent already holds.
//!
//! RUN-002 exposes a `DelegationNarrowingCheck` trait in
//! `crates/server/src/runtime/agents/store.rs`; the runtime owns that trait object, and
//! `crates/server` depends on this crate, not the reverse, so this crate provides the
//! check as a thin crate-level type the runtime adapts. Until the runtime wires it in,
//! RUN-002's `StructuralDelegationCheck` remains the fallback.

use quansio_core::CorrelationId;
use quansio_events::{Actor, EventBatch, EventDraft};

use crate::algebra::{WideningReason, WideningRejection};
use crate::error::CapabilityError;
use crate::grant::Grant;
use crate::layer::Layer;
use crate::persistence::{stage_projected_event, ProjectionRow};
use crate::projection::CapabilityProjection;

/// The narrowing violation in a candidate child grant set, if any.
#[must_use]
pub fn narrowing_violation(parent: &[Grant], child: &[Grant]) -> Option<WideningRejection> {
    child
        .iter()
        .find(|candidate| !parent.iter().any(|held| candidate.is_narrowing_of(held)))
        .map(|candidate| {
            WideningRejection::new(
                Layer::Delegation,
                candidate.clone(),
                WideningReason::GrantNotHeldByParent,
            )
        })
}

/// Whether a child projection is provably a narrowing of its parent's.
///
/// # Errors
/// Returns [`CapabilityError::WideningRejected`] naming the first child grant the parent
/// does not hold.
pub fn check_narrowing(
    parent: &CapabilityProjection,
    child: &CapabilityProjection,
) -> Result<(), CapabilityError> {
    check_narrowing_grants(&parent.grants, &child.grants)
}

/// Whether a candidate grant set is a narrowing of a held grant set.
///
/// # Errors
/// Returns [`CapabilityError::WideningRejected`] naming the first candidate the held set
/// does not contain.
pub fn check_narrowing_grants(parent: &[Grant], child: &[Grant]) -> Result<(), CapabilityError> {
    match narrowing_violation(parent, child) {
        None => Ok(()),
        Some(rejection) => Err(CapabilityError::WideningRejected(Box::new(rejection))),
    }
}

/// The crate-level delegation narrowing check for RUN-002's hook.
#[derive(Debug, Clone, Copy, Default)]
pub struct DelegationNarrowingCheck;

impl DelegationNarrowingCheck {
    /// Reject a child projection that is not a narrowing of its parent's.
    ///
    /// # Errors
    /// Returns [`CapabilityError::WideningRejected`] when the child widens authority.
    pub fn check(
        parent: &CapabilityProjection,
        child: &CapabilityProjection,
    ) -> Result<(), CapabilityError> {
        check_narrowing(parent, child)
    }

    /// Reject a candidate child grant set that is not a narrowing of the parent's.
    ///
    /// # Errors
    /// Returns [`CapabilityError::WideningRejected`] when the child widens authority.
    pub fn check_grants(parent: &[Grant], child: &[Grant]) -> Result<(), CapabilityError> {
        check_narrowing_grants(parent, child)
    }
}

/// Stage the child's `capability.projected` event only when it is a proven narrowing of
/// the parent's projection; a widening delegation writes nothing.
///
/// # Errors
/// Returns [`CapabilityError::WideningRejected`] and leaves `batch` untouched when the
/// child widens authority.
pub fn stage_delegated_projection(
    batch: &mut EventBatch,
    parent: &CapabilityProjection,
    child_row: &ProjectionRow,
    correlation_id: CorrelationId,
    actor: Actor,
) -> Result<EventDraft, CapabilityError> {
    let child = child_row.projection()?;
    check_narrowing(parent, &child)?;
    Ok(stage_projected_event(
        batch,
        child_row,
        correlation_id,
        actor,
    ))
}
