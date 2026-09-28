//! The proposal-trust source the served runtime composes (INT-012, DOMAIN.md §12 rule 2).
//!
//! A tool call's `derived_from_trust` is the most-untrusted label among the context segments
//! of its causal window: every segment the proposing turn's model calls were shown, as INT-005
//! recorded them in `context_projections`. A turn with no recorded projection, and any segment
//! whose label is missing or unknown, fails closed to [`FAIL_CLOSED_TRUST`] — a labelling gap
//! can escalate a call, never widen what it may do.

use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::PgPool;

use super::escalation::{derived_from_trust, TrustLabellingSource, TrustLevel, FAIL_CLOSED_TRUST};
use crate::runtime::context_bridge::store::turn_segment_labels;
use crate::runtime::state_machine::RuntimeIdentity;
use crate::runtime::turn_loop::ProposalTrustSource;

/// Labels a proposal from the ContextProjections its turn recorded.
#[derive(Clone)]
pub struct StoredProposalTrust {
    pool: PgPool,
    tenant_id: String,
}

impl StoredProposalTrust {
    /// A source over the pool for the identity's tenant.
    #[must_use]
    pub fn new(pool: PgPool, identity: &RuntimeIdentity) -> Self {
        Self {
            pool,
            tenant_id: identity.tenant_id.clone(),
        }
    }
}

/// Segment labels as recorded; an unknown spelling is unlabelled.
struct RecordedLabels(HashMap<String, Option<TrustLevel>>);

impl TrustLabellingSource for RecordedLabels {
    fn trust_of(&self, segment_ref: &str) -> Option<TrustLevel> {
        self.0.get(segment_ref).copied().flatten()
    }
}

/// The derived trust of a causal window given as `(segment, recorded label)` pairs.
///
/// The same segment recorded twice (two model calls of one turn) keeps its least-trusted
/// label, and an empty window fails closed.
#[must_use]
pub fn derive_from_labels(labels: &[(String, Option<String>)]) -> TrustLevel {
    let mut recorded: HashMap<String, Option<TrustLevel>> = HashMap::new();
    let mut order = Vec::new();
    for (segment, label) in labels {
        let level = label
            .as_deref()
            .and_then(|name| TrustLevel::parse(name).ok());
        match recorded.get_mut(segment) {
            Some(existing) => {
                *existing = match (*existing, level) {
                    (Some(left), Some(right)) => Some(left.max(right)),
                    _ => None,
                };
            }
            None => {
                recorded.insert(segment.clone(), level);
                order.push(segment.clone());
            }
        }
    }
    if order.is_empty() {
        return FAIL_CLOSED_TRUST;
    }
    derived_from_trust(&RecordedLabels(recorded), &order)
}

#[async_trait]
impl ProposalTrustSource for StoredProposalTrust {
    async fn proposal_trust(&self, run_id: &str, turn_id: &str) -> Option<TrustLevel> {
        // An unreadable record proves nothing: the caller fails closed on `None`.
        let labels = turn_segment_labels(&self.pool, &self.tenant_id, run_id, turn_id)
            .await
            .ok()?;
        if labels.is_empty() {
            return None;
        }
        Some(derive_from_labels(&labels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(segment: &str, level: Option<&str>) -> (String, Option<String>) {
        (segment.to_string(), level.map(str::to_string))
    }

    #[test]
    fn the_window_derives_its_most_untrusted_label() {
        assert_eq!(
            derive_from_labels(&[
                label("system", Some("TRUSTED_SYSTEM")),
                label("msg", Some("TRUSTED_USER")),
            ]),
            TrustLevel::TrustedUser
        );
        assert_eq!(
            derive_from_labels(&[
                label("msg", Some("TRUSTED_USER")),
                label("page", Some("UNTRUSTED_EXTERNAL")),
            ]),
            TrustLevel::UntrustedExternal
        );
    }

    #[test]
    fn an_empty_window_or_an_unknown_label_fails_closed() {
        assert_eq!(derive_from_labels(&[]), FAIL_CLOSED_TRUST);
        assert_eq!(
            derive_from_labels(&[
                label("msg", Some("TRUSTED_USER")),
                label("odd", Some("trusted")),
            ]),
            FAIL_CLOSED_TRUST
        );
        assert_eq!(derive_from_labels(&[label("msg", None)]), FAIL_CLOSED_TRUST);
    }

    #[test]
    fn a_segment_seen_twice_keeps_its_least_trusted_label() {
        assert_eq!(
            derive_from_labels(&[
                label("tool", Some("AGENT_GENERATED")),
                label("tool", Some("UNTRUSTED_EXTERNAL")),
            ]),
            TrustLevel::UntrustedExternal
        );
    }
}
