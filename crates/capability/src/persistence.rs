//! Projection persistence and event staging (DOMAIN.md §6.2, §7.2, §9.2).
//!
//! A projection that authorizes an effect must be reconstructable, so the persisted row
//! and the `capability.projected` event carry the same `inputs_digest`. This module owns
//! the exact `capability_projections` column mapping and the canonical event payloads.
//! The row is applied on the same `quansio-events` transaction that carries the event,
//! so the two can never diverge; the transaction itself belongs to the store-owning
//! caller, which binds [`ProjectionRow::arguments`] to [`ProjectionRow::INSERT_SQL`]
//! inside `EventStore::commit_mutation_tx`.

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, Digest};
use quansio_events::{Actor, EventBatch, EventDraft, EventType};

use crate::algebra::WideningRejection;
use crate::error::CapabilityError;
use crate::grant::Grant;
use crate::layer::ProjectionInput;
use crate::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};

/// Aggregate type of a `capability.*` event.
pub const CAPABILITY_AGGREGATE_TYPE: &str = "capability_projection";

/// `capability.projected` event type (DOMAIN.md §9.2).
pub const PROJECTED_EVENT_TYPE: &str = "capability.projected";

/// `capability.widening_rejected` event type (DOMAIN.md §9.2).
pub const WIDENING_REJECTED_EVENT_TYPE: &str = "capability.widening_rejected";

/// One bound value of a `capability_projections` row, in statement order.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionArgument {
    /// A required text column.
    Text(String),
    /// A nullable text column.
    OptionalText(Option<String>),
    /// A `jsonb` column.
    Json(serde_json::Value),
    /// A `timestamptz` column.
    Timestamptz(DateTime<Utc>),
    /// A nullable `timestamptz` column.
    OptionalTimestamptz(Option<DateTime<Utc>>),
}

/// One `capability_projections` row (DOMAIN.md §6.2, migration 0001).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRow {
    /// `cap_…` projection id.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace, when the subject is workspace-scoped.
    pub workspace_id: Option<String>,
    /// Subject kind.
    pub subject_kind: SubjectKind,
    /// Subject id.
    pub subject_id: String,
    /// Layer inputs, in fixed order.
    pub inputs: Vec<ProjectionInput>,
    /// Effective grants.
    pub grants: Vec<Grant>,
    /// SHA-256 over `inputs`.
    pub inputs_digest: Digest,
    /// When the projection was computed.
    pub computed_at: DateTime<Utc>,
    /// Earliest expiry across inputs and grants.
    pub expires_at: Option<DateTime<Utc>>,
}

impl ProjectionRow {
    /// The canonical insert statement, with `arguments()` bound positionally.
    pub const INSERT_SQL: &'static str = "INSERT INTO capability_projections \
         (id, tenant_id, workspace_id, subject_kind, subject_id, inputs, grants, \
          inputs_digest, computed_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)";

    /// The canonical read-back statement (`$1` = id, `$2` = tenant id).
    pub const SELECT_SQL: &'static str = "SELECT id, tenant_id, workspace_id, subject_kind, \
         subject_id, inputs, grants, inputs_digest, computed_at, expires_at \
         FROM capability_projections WHERE id = $1 AND tenant_id = $2";

    /// Build the row for a projection in a tenant.
    #[must_use]
    pub fn from_projection(
        projection: &CapabilityProjection,
        tenant_id: impl Into<String>,
        workspace_id: Option<String>,
    ) -> Self {
        Self {
            id: projection.id.to_string(),
            tenant_id: tenant_id.into(),
            workspace_id,
            subject_kind: projection.subject.kind,
            subject_id: projection.subject.id.clone(),
            inputs: projection.inputs.clone(),
            grants: projection.grants.clone(),
            inputs_digest: projection.inputs_digest.clone(),
            computed_at: projection.computed_at,
            expires_at: projection.expires_at,
        }
    }

    /// Rebuild the projection, proving the stored `inputs_digest` still covers the
    /// stored inputs.
    ///
    /// # Errors
    /// Returns [`CapabilityError::MalformedGrant`] when the row does not reconstruct a
    /// canonical projection.
    pub fn projection(&self) -> Result<CapabilityProjection, CapabilityError> {
        let id = CanonicalId::parse(&self.id)
            .map_err(|error| CapabilityError::MalformedGrant(error.to_string()))?;
        let projection = CapabilityProjection::from_parts(
            id,
            ProjectionSubject::new(self.subject_kind, self.subject_id.clone()),
            self.inputs.clone(),
            self.grants.clone(),
            self.computed_at,
            self.expires_at,
        )?;
        if projection.inputs_digest != self.inputs_digest {
            return Err(CapabilityError::MalformedGrant(
                "stored inputs_digest does not cover the stored inputs".to_string(),
            ));
        }
        Ok(projection)
    }

    /// The positional argument list for [`ProjectionRow::INSERT_SQL`].
    #[must_use]
    pub fn arguments(&self) -> Vec<ProjectionArgument> {
        vec![
            ProjectionArgument::Text(self.id.clone()),
            ProjectionArgument::Text(self.tenant_id.clone()),
            ProjectionArgument::OptionalText(self.workspace_id.clone()),
            ProjectionArgument::Text(self.subject_kind.as_str().to_string()),
            ProjectionArgument::Text(self.subject_id.clone()),
            ProjectionArgument::Json(
                serde_json::to_value(&self.inputs).expect("inputs are JSON-serializable"),
            ),
            ProjectionArgument::Json(
                serde_json::to_value(&self.grants).expect("grants are JSON-serializable"),
            ),
            ProjectionArgument::Text(self.inputs_digest.to_string()),
            ProjectionArgument::Timestamptz(self.computed_at),
            ProjectionArgument::OptionalTimestamptz(self.expires_at),
        ]
    }
}

/// The payload of a `capability.projected` event.
#[must_use]
pub fn projected_event_payload(row: &ProjectionRow) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "projection_id".to_string(),
        serde_json::Value::String(row.id.clone()),
    );
    payload.insert(
        "subject".to_string(),
        serde_json::json!({"kind": row.subject_kind.as_str(), "id": row.subject_id}),
    );
    payload.insert(
        "inputs".to_string(),
        serde_json::to_value(&row.inputs).expect("inputs are JSON-serializable"),
    );
    payload.insert(
        "grants".to_string(),
        serde_json::to_value(&row.grants).expect("grants are JSON-serializable"),
    );
    payload.insert(
        "inputs_digest".to_string(),
        serde_json::Value::String(row.inputs_digest.to_string()),
    );
    payload.insert(
        "computed_at".to_string(),
        serde_json::Value::String(row.computed_at.to_rfc3339()),
    );
    if let Some(expires_at) = row.expires_at {
        payload.insert(
            "expires_at".to_string(),
            serde_json::Value::String(expires_at.to_rfc3339()),
        );
    }
    serde_json::Value::Object(payload)
}

/// Stage `capability.projected` for a persisted projection and return the draft.
pub fn stage_projected_event(
    batch: &mut EventBatch,
    row: &ProjectionRow,
    correlation_id: quansio_core::CorrelationId,
    actor: Actor,
) -> EventDraft {
    let mut draft = EventDraft::new(
        CAPABILITY_AGGREGATE_TYPE,
        row.id.clone(),
        1,
        event_type(PROJECTED_EVENT_TYPE),
        correlation_id,
        actor,
    )
    .with_payload(projected_event_payload(row));
    if let Some(workspace_id) = &row.workspace_id {
        draft = draft.with_workspace(workspace_id.clone());
    }
    batch.emit(draft.clone());
    draft
}

/// Stage `capability.widening_rejected` for one ignored widening attempt and return the
/// draft.
pub fn stage_widening_rejected_event(
    batch: &mut EventBatch,
    projection_id: &CanonicalId,
    rejection: &WideningRejection,
    correlation_id: quansio_core::CorrelationId,
    actor: Actor,
) -> EventDraft {
    let payload = serde_json::json!({
        "projection_id": projection_id.to_string(),
        "layer": rejection.layer.as_str(),
        "grant": rejection.grant,
        "reason": rejection.reason,
    });
    let draft = EventDraft::new(
        CAPABILITY_AGGREGATE_TYPE,
        projection_id.to_string(),
        1,
        event_type(WIDENING_REJECTED_EVENT_TYPE),
        correlation_id,
        actor,
    )
    .with_payload(payload);
    batch.emit(draft.clone());
    draft
}

/// Stage one `capability.widening_rejected` event per rejected widening attempt in an
/// assembly, returning the number recorded.
pub fn stage_assembly_rejections(
    batch: &mut EventBatch,
    assembly: &crate::projection::Assembly,
    correlation_id: quansio_core::CorrelationId,
    actor: Actor,
) -> usize {
    for rejection in &assembly.rejections {
        stage_widening_rejected_event(
            batch,
            &assembly.projection.id,
            rejection,
            correlation_id,
            actor.clone(),
        );
    }
    assembly.rejections.len()
}

fn event_type(value: &str) -> EventType {
    EventType::parse(value).expect("canonical capability event type")
}
