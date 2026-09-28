//! The run-side plan seam: a `work.propose_plan` call compiled and committed by the
//! [`Planner`] as the proposing run (RUN-003, DOMAIN.md §4.5, §5.6).
//!
//! The model supplies only the plan's changes. The runtime fixes everything that carries
//! authority: the proposal id is the call's, the run and workspace are the proposing run's
//! (a proposal naming another is refused), the proposer projection is the run's stored
//! Capability Projection, and every created node is stamped as made by the proposing
//! AgentThread. A refusal reaches the model with the planner's own code
//! (`CONFLICT_REVISION`, `CAPABILITY_DENIED`, `VALIDATION_SCHEMA`, …).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;

use super::{
    GraphPlanWorkspace, PlanError, PlanOutcome, PlanProposerProjection, PlanWorkspacePort, Planner,
};
use crate::runtime::state_machine::{RuntimeError, RuntimeIdentity};
use crate::runtime::turn_loop::{
    PlanApplicationContext, PlanApplicationPort, ProjectionProvider, StoredProjectionProvider,
};

/// The task that owns plan compilation, named on a refusal.
pub const PLAN_OWNER: &str = "RUN-003";

/// The graph-backed [`PlanApplicationPort`] a served turn loop composes.
pub struct RunPlanApplication {
    pool: PgPool,
    identity: RuntimeIdentity,
    projections: Arc<dyn ProjectionProvider>,
}

impl RunPlanApplication {
    /// Build the port for one tenant, reading proposer grants from the stored projection.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        let projections = Arc::new(StoredProjectionProvider::new(
            pool.clone(),
            identity.clone(),
        )?);
        Ok(Self::with_projections(pool, identity, projections))
    }

    /// Build the port with an explicit projection source.
    #[must_use]
    pub fn with_projections(
        pool: PgPool,
        identity: RuntimeIdentity,
        projections: Arc<dyn ProjectionProvider>,
    ) -> Self {
        Self {
            pool,
            identity,
            projections,
        }
    }

    fn planner(&self) -> Result<Planner, RuntimeError> {
        let workspace: Arc<dyn PlanWorkspacePort> = Arc::new(GraphPlanWorkspace::new(
            self.pool.clone(),
            self.identity.clone(),
        ));
        Planner::new(self.pool.clone(), workspace, self.identity.clone())
    }
}

/// Pin the model's proposal to the call and the run: fill the identity fields it may omit,
/// and refuse one that names a different proposal, run or workspace.
fn pinned_proposal(
    proposal_id: &str,
    proposal_json: &str,
    context: &PlanApplicationContext,
) -> Result<Value, PlanError> {
    let mut raw: Value = serde_json::from_str(proposal_json)
        .map_err(|error| PlanError::schema(format!("proposal_json is not JSON: {error}")))?;
    let object = raw
        .as_object_mut()
        .ok_or_else(|| PlanError::schema("proposal_json must be a JSON object"))?;
    for (field, value) in [
        ("proposal_id", proposal_id),
        ("run_id", context.run_id.as_str()),
        ("workspace_id", context.workspace_id.as_str()),
    ] {
        match object.get(field) {
            None | Some(Value::Null) => {
                object.insert(field.to_string(), Value::String(value.to_string()));
            }
            Some(Value::String(given)) if given == value => {}
            Some(other) => {
                return Err(PlanError::schema(format!(
                    "{field} {other} is not this call's {value}"
                )));
            }
        }
    }
    for list in [
        "nodes_add",
        "nodes_update",
        "edges_add",
        "edges_remove",
        "capability_needs",
    ] {
        object.entry(list).or_insert_with(|| json!([]));
    }
    Ok(raw)
}

fn outcome_json(outcome: &PlanOutcome) -> Value {
    json!({
        "accepted": true,
        "proposal_id": outcome.proposal_id,
        "revision": outcome.revision,
        "nodes_added": outcome.nodes_added,
        "nodes_updated": outcome.nodes_updated,
        "edges_added": outcome.edges_added,
        "edges_removed": outcome.edges_removed,
        "mutations": outcome.mutation_summary,
    })
}

fn refusal(error: &PlanError) -> RuntimeError {
    RuntimeError::OwnerRefused {
        owner: PLAN_OWNER,
        code: error.code(),
        detail: error.to_string(),
    }
}

#[async_trait]
impl PlanApplicationPort for RunPlanApplication {
    async fn apply(
        &self,
        proposal_id: &str,
        proposal_json: &str,
        context: PlanApplicationContext,
    ) -> Result<Value, RuntimeError> {
        let raw = pinned_proposal(proposal_id, proposal_json, &context)
            .map_err(|error| refusal(&error))?;
        let projection = self
            .projections
            .projection_for_run(&context.run_id)
            .await?
            .ok_or_else(|| RuntimeError::OwnerRefused {
                owner: PLAN_OWNER,
                code: "CAPABILITY_DENIED",
                detail: format!("run {} has no capability projection", context.run_id),
            })?;
        let proposer = PlanProposerProjection {
            agent_thread_id: Some(context.agent_thread_id.clone()),
            grants: Value::Array(
                projection
                    .grants
                    .iter()
                    .map(|grant| grant.to_json())
                    .collect(),
            ),
            created_by: json!({ "kind": "agent", "id": context.agent_thread_id }),
        };
        let outcome = self
            .planner()?
            .propose(&raw, proposer)
            .await
            .map_err(|error| refusal(&error))?;
        Ok(outcome_json(&outcome))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> PlanApplicationContext {
        PlanApplicationContext {
            run_id: "run_a".to_string(),
            workspace_id: "ws_a".to_string(),
            agent_thread_id: "ath_a".to_string(),
            generation: 1,
        }
    }

    #[test]
    fn a_proposal_is_pinned_to_its_call_run_and_workspace() {
        let raw = pinned_proposal("prop_1", r#"{"base_revision": 3}"#, &context()).expect("pin");
        assert_eq!(raw["proposal_id"], "prop_1");
        assert_eq!(raw["run_id"], "run_a");
        assert_eq!(raw["workspace_id"], "ws_a");
        assert_eq!(raw["nodes_add"], json!([]));

        for foreign in [
            r#"{"workspace_id": "ws_b"}"#,
            r#"{"run_id": "run_b"}"#,
            r#"{"proposal_id": "prop_2"}"#,
            r#"{"workspace_id": 7}"#,
        ] {
            let error = pinned_proposal("prop_1", foreign, &context()).expect_err(foreign);
            assert_eq!(error.code(), "VALIDATION_SCHEMA", "{foreign}: {error}");
        }
        for malformed in ["not json", "[1]"] {
            assert_eq!(
                pinned_proposal("prop_1", malformed, &context())
                    .expect_err(malformed)
                    .code(),
                "VALIDATION_SCHEMA"
            );
        }
    }
}
