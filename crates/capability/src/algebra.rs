//! The capability algebra: intersection with most-restrictive constraints
//! (DOMAIN.md §6.3).
//!
//! Composition never widens. [`narrow`] intersects a running grant set with one
//! layer's candidate set: a candidate is accepted only when some held grant's resource
//! contains it, its constraints are combined most-restrictively with every held grant
//! that contains it, and a candidate nothing contains is ignored and reported as a
//! [`WideningRejection`] for the caller to record.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::grant::{Constraints, Grant};
use crate::layer::Layer;

/// Why a layer's candidate grant was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WideningReason {
    /// The candidate is not contained by any grant held by the layer above.
    NotNarrowerThanAnyInput,
    /// A user rule tried to widen an approval that policy does not allow.
    ApprovalWideningNotPermitted,
    /// A user rule tried `always` on a tier-4 effect, which never accepts it.
    TierFourAlwaysRejected,
    /// A delegated child holds a grant the parent does not hold.
    GrantNotHeldByParent,
}

/// A rejected widening attempt, to be recorded as `capability.widening_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WideningRejection {
    /// The layer that attempted the widening.
    pub layer: Layer,
    /// The grant the layer attempted to add or widen.
    pub grant: Grant,
    /// Why it was rejected.
    pub reason: WideningReason,
}

impl WideningRejection {
    /// Build a rejection.
    #[must_use]
    pub fn new(layer: Layer, grant: Grant, reason: WideningReason) -> Self {
        Self {
            layer,
            grant,
            reason,
        }
    }
}

impl fmt::Display for WideningRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "layer {} attempted {} ({:?})",
            self.layer, self.grant, self.reason
        )
    }
}

/// Intersect `held` with one layer's candidate grants (DOMAIN.md §6.3).
///
/// Returns the narrowed grant set plus one [`WideningRejection`] per candidate the
/// layer attempted to add. A held grant that no accepted candidate covers is removed:
/// a layer may always remove authority.
#[must_use]
pub fn narrow(
    held: &[Grant],
    candidates: &[Grant],
    layer: Layer,
) -> (Vec<Grant>, Vec<WideningRejection>) {
    let mut grants: Vec<Grant> = Vec::new();
    let mut rejections: Vec<WideningRejection> = Vec::new();

    for candidate in candidates {
        let covering: Vec<&Grant> = held
            .iter()
            .filter(|grant| {
                grant.effect_class == candidate.effect_class
                    && grant.resource.covers(&candidate.resource)
            })
            .collect();
        if covering.is_empty() {
            rejections.push(WideningRejection::new(
                layer,
                candidate.clone(),
                WideningReason::NotNarrowerThanAnyInput,
            ));
            continue;
        }
        let mut constraints = candidate.constraints.clone();
        for grant in covering {
            constraints = Constraints::combine(&constraints, &grant.constraints);
        }
        push_most_restrictive(
            &mut grants,
            Grant {
                effect_class: candidate.effect_class.clone(),
                resource: candidate.resource.clone(),
                constraints,
            },
        );
    }

    (grants, rejections)
}

fn push_most_restrictive(grants: &mut Vec<Grant>, grant: Grant) {
    if let Some(existing) = grants.iter_mut().find(|existing| {
        existing.effect_class == grant.effect_class && existing.resource == grant.resource
    }) {
        existing.constraints = Constraints::combine(&existing.constraints, &grant.constraints);
        return;
    }
    grants.push(grant);
}
