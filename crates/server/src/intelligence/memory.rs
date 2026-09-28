//! The served `memory.propose` seam: a candidate offered to INT-007's gated memory owner over
//! the intelligence plane's `ProposeMemory` RPC (DOMAIN.md §11.4).
//!
//! The runtime has already authorized the call (capability, policy, approvals, the Effect
//! Ledger); this port only forwards it. What the model may say is limited to the claim — its
//! subject, content and confidence, and a scope hint. Provenance is the runtime's: a memory
//! proposed by a run is an `explicit_user` direction whose ref is the message that triggered
//! the run, and a run no message triggered cannot propose one. The owner then decides the
//! memory's identity and scope and stores it as a `candidate`, which retrieval never returns
//! until it is promoted — so nothing reaches what the platform remembers without that gate.

use std::time::Duration;

use async_trait::async_trait;
use quansio_contracts::generated::intelligence::MemoryCandidate;
use serde_json::{json, Value};
use sqlx::PgPool;

use super::{CallScope, IntelligenceClient, IntelligenceError};
use crate::runtime::state_machine::{RuntimeError, RuntimeIdentity, MEMORY_OWNER};
use crate::runtime::turn_loop::{
    MemoryProposalContext, MemoryProposalPort, ProjectionProvider, StoredProjectionProvider,
};

/// The only provenance a run's proposal carries (INT-007's closed vocabulary).
const EXPLICIT_USER: &str = "explicit_user";

/// [`MemoryProposalPort`] over the intelligence plane.
pub struct GatewayMemoryProposals {
    client: IntelligenceClient,
    deadline: Duration,
    projections: StoredProjectionProvider,
    tenant_id: String,
}

impl GatewayMemoryProposals {
    /// Build the port for one tenant.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the tenant id is not canonical.
    pub fn new(
        pool: PgPool,
        identity: RuntimeIdentity,
        client: IntelligenceClient,
        deadline: Duration,
    ) -> Result<Self, RuntimeError> {
        let tenant_id = identity.tenant_id.clone();
        Ok(Self {
            client,
            deadline,
            projections: StoredProjectionProvider::new(pool, identity)?,
            tenant_id,
        })
    }
}

fn refusal(code: &str, detail: impl Into<String>) -> RuntimeError {
    let code = match code {
        "VALIDATION_SCHEMA" => "VALIDATION_SCHEMA",
        "VALIDATION_BOUNDS" => "VALIDATION_BOUNDS",
        "SCOPE_FORBIDDEN" => "SCOPE_FORBIDDEN",
        "POLICY_DENIED" => "POLICY_DENIED",
        "CONFLICT_STATE" => "CONFLICT_STATE",
        "NOT_FOUND" => "NOT_FOUND",
        "ROUTE_UNAVAILABLE" => "ROUTE_UNAVAILABLE",
        _ => "INTERNAL",
    };
    RuntimeError::OwnerRefused {
        owner: MEMORY_OWNER,
        code,
        detail: detail.into(),
    }
}

/// The wire candidate for a model's claim, with the runtime's provenance pinned.
fn candidate(context: &MemoryProposalContext) -> Result<MemoryCandidate, RuntimeError> {
    let Some(trigger) = context.trigger_message_id.as_deref() else {
        return Err(refusal(
            "POLICY_DENIED",
            "only a run a message triggered may propose memory: its provenance is that message",
        ));
    };
    let claim: Value = serde_json::from_str(&context.candidate_json).map_err(|error| {
        refusal(
            "VALIDATION_SCHEMA",
            format!("candidate_json is not JSON: {error}"),
        )
    })?;
    let object = claim
        .as_object()
        .ok_or_else(|| refusal("VALIDATION_SCHEMA", "candidate_json must be a JSON object"))?;
    for field in object.keys() {
        if !matches!(
            field.as_str(),
            "subject_ref" | "content" | "confidence" | "provenance_kind" | "provenance_ref"
        ) {
            return Err(refusal(
                "VALIDATION_SCHEMA",
                format!("candidate_json has no field {field}"),
            ));
        }
    }
    for (field, pinned) in [
        ("provenance_kind", EXPLICIT_USER),
        ("provenance_ref", trigger),
    ] {
        match object.get(field) {
            None | Some(Value::Null) => {}
            Some(Value::String(given)) if given == pinned => {}
            Some(other) => {
                return Err(refusal(
                    "VALIDATION_SCHEMA",
                    format!("{field} {other} is not this run's {pinned}"),
                ));
            }
        }
    }
    let text = |field: &str| {
        object
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default()
    };
    let confidence = match object.get("confidence") {
        None | Some(Value::Null) => 0.5,
        Some(value) => value
            .as_f64()
            .filter(|confidence| (0.0..=1.0).contains(confidence))
            .ok_or_else(|| refusal("VALIDATION_SCHEMA", "confidence must be a number in 0..=1"))?,
    };
    Ok(MemoryCandidate {
        subject_ref: text("subject_ref"),
        content: text("content"),
        provenance_kind: EXPLICIT_USER.to_string(),
        provenance_ref: trigger.to_string(),
        confidence,
        scope_hint: context.scope.clone().unwrap_or_default(),
        ..MemoryCandidate::default()
    })
}

#[async_trait]
impl MemoryProposalPort for GatewayMemoryProposals {
    async fn propose(&self, context: MemoryProposalContext) -> Result<Value, RuntimeError> {
        let candidate = candidate(&context)?;
        let projection = self
            .projections
            .projection_for_run(&context.run_id)
            .await?
            .ok_or_else(|| {
                refusal(
                    "POLICY_DENIED",
                    format!("run {} has no capability projection", context.run_id),
                )
            })?;
        let scope = CallScope {
            tenant_id: self.tenant_id.clone(),
            workspace_id: context.workspace_id.clone(),
            correlation_id: format!("{}:memory", context.run_id),
            deadline: self.deadline,
            capability_projection_id: projection.id.to_string(),
        };
        let mut client = self.client.clone();
        let ack = client
            .propose_memory(&scope, candidate)
            .await
            .map_err(|error| match error {
                IntelligenceError::Refused { code, message } => refusal(&code, message),
                other => refusal("ROUTE_UNAVAILABLE", other.to_string()),
            })?;
        Ok(json!({
            "accepted": ack.accepted,
            "memory_id": ack.proposal_id,
            "status": "candidate",
            "recorded": ack.reason.is_empty(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(candidate_json: &str, trigger: Option<&str>) -> MemoryProposalContext {
        MemoryProposalContext {
            run_id: "run_a".to_string(),
            workspace_id: "ws_a".to_string(),
            candidate_json: candidate_json.to_string(),
            scope: Some("user".to_string()),
            trigger_message_id: trigger.map(str::to_string),
        }
    }

    #[test]
    fn provenance_is_the_runs_trigger_never_the_models() {
        let wire = candidate(&context(
            r#"{"subject_ref": "usr_a", "content": "prefers tea", "confidence": 0.9}"#,
            Some("msg_1"),
        ))
        .expect("candidate");
        assert_eq!(wire.provenance_kind, "explicit_user");
        assert_eq!(wire.provenance_ref, "msg_1");
        assert_eq!(wire.scope_hint, "user");
        assert!((wire.confidence - 0.9).abs() < f64::EPSILON);

        for claimed in [
            r#"{"subject_ref": "s", "content": "c", "provenance_kind": "verified_run"}"#,
            r#"{"subject_ref": "s", "content": "c", "provenance_ref": "msg_2"}"#,
            r#"{"subject_ref": "s", "content": "c", "status": "active"}"#,
            r#"{"subject_ref": "s", "content": "c", "confidence": 3}"#,
            "not json",
        ] {
            let error = candidate(&context(claimed, Some("msg_1"))).expect_err(claimed);
            assert_eq!(error.code(), "VALIDATION_SCHEMA", "{claimed}: {error}");
        }
        let error = candidate(&context(r#"{"subject_ref": "s", "content": "c"}"#, None))
            .expect_err("no trigger message");
        assert_eq!(error.code(), "POLICY_DENIED");
    }
}
