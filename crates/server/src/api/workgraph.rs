//! Plan, WorkGraph and agent control (APP-005): the `GET /v1/workgraph` projection and the
//! §14 work commands `CreateObjective` and `ProposeGraphEdit`.
//!
//! The view is a projection of the canonical graph revision (`graph_heads`) over
//! `work_nodes`, `work_edges`, `agent_threads`, `agent_graph_edges` and `runs`. Every edit
//! is a command compiled and validated by RUN-003's planner exactly like a model's
//! PlanProposal and committed by CORE-005's revision-checked graph transaction: a stale
//! `base_revision` is `CONFLICT_REVISION` and writes nothing, so a client that edited an
//! older revision has to rebase, never overwrite.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_capability::{Constraints, Grant, PolicyDecision};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::conversation::ConversationStore;
use crate::control::schema;
use crate::policy::PolicyStore;
use crate::runtime::planning::{
    GraphPlanWorkspace, PlanError, PlanOutcome, PlanProposerProjection, PlanWorkspacePort, Planner,
};
use crate::runtime::state_machine::RuntimeIdentity;

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

/// `?workspace_id=`.
#[derive(Debug, Deserialize)]
pub struct WorkgraphQuery {
    workspace_id: String,
}

/// Node states in which nothing is being done and something is being waited for.
const PARKED_RUN_STATES: [&str; 5] = [
    "WAITING_APPROVAL",
    "WAITING_QUESTION",
    "WAITING_CHILD",
    "WAITING_TIMER",
    "BLOCKED",
];

/// `GET /v1/workgraph?workspace_id=`: the plan as the canonical revision has it.
pub async fn workgraph(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkgraphQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    CanonicalId::parse_typed(&query.workspace_id, Prefix::Workspace).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationBounds,
            error.to_string(),
            "projection",
        )
    })?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM graph_heads WHERE tenant_id = $1 AND workspace_id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(projection_error)?
    .unwrap_or(1);
    let nodes = sqlx::query(
        "SELECT id, kind, title, description, parent_id, status, owner_agent_thread_id, created_by, \
                completion_contract, capability_needs, budget_id, priority, revision, thread_id, origin, \
                created_at, updated_at \
         FROM work_nodes WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    let edges = sqlx::query(
        "SELECT id, from_node_id, to_node_id, kind, revision FROM work_edges \
         WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    // The latest run per node: what is being done about it right now.
    let runs = sqlx::query(
        "SELECT DISTINCT ON (work_node_id) work_node_id, id, agent_thread_id, status, generation, updated_at \
         FROM runs WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY work_node_id, created_at DESC",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    let agents = sqlx::query(
        "SELECT id, agent_kind, parent_id, work_node_id, status, generation, suspended_reason, \
                handoff_to_agent_thread_id, created_at, updated_at \
         FROM agent_threads WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    let handoffs = sqlx::query(
        "SELECT e.id, e.parent_agent_thread_id, e.child_agent_thread_id, e.work_node_id, \
                e.delegation_capability_id, e.delegated_at, e.joined_at \
         FROM agent_graph_edges e JOIN agent_threads c ON c.id = e.child_agent_thread_id \
         WHERE e.tenant_id = $1 AND c.workspace_id = $2 ORDER BY e.delegated_at, e.id",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    let open_questions = sqlx::query(
        "SELECT q.id, q.run_id, q.prompt FROM questions q WHERE q.tenant_id = $1 AND q.workspace_id = $2 AND q.status = 'open'",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;

    let mut run_by_node: HashMap<String, Value> = HashMap::new();
    let mut run_node: HashMap<String, String> = HashMap::new();
    for row in &runs {
        let node_id: String = row.get("work_node_id");
        let run_id: String = row.get("id");
        run_node.insert(run_id.clone(), node_id.clone());
        run_by_node.insert(
            node_id,
            json!({
                "id": run_id,
                "agent_thread_id": row.get::<String, _>("agent_thread_id"),
                "status": row.get::<String, _>("status"),
                "generation": row.get::<i64, _>("generation"),
                "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
            }),
        );
    }
    let questions_by_node: HashMap<String, Vec<Value>> =
        open_questions.iter().fold(HashMap::new(), |mut acc, row| {
            let run_id: String = row.get("run_id");
            if let Some(node) = run_node.get(&run_id) {
                acc.entry(node.clone()).or_default().push(json!({
                    "question_id": row.get::<String, _>("id"),
                    "prompt": row.get::<String, _>("prompt"),
                }));
            }
            acc
        });

    let status_of: HashMap<String, String> = nodes
        .iter()
        .map(|row| (row.get::<String, _>("id"), row.get::<String, _>("status")))
        .collect();
    // `from depends_on to`: the `from` node waits for the `to` node (orchestration's rule).
    let mut prerequisites: HashMap<String, Vec<String>> = HashMap::new();
    for row in &edges {
        let kind: String = row.get("kind");
        if kind == "depends_on" || kind == "blocked_by" {
            prerequisites
                .entry(row.get::<String, _>("from_node_id"))
                .or_default()
                .push(row.get::<String, _>("to_node_id"));
        }
    }

    // Blockers, each with a typed reason: an unsatisfied prerequisite, a parked run, an
    // open question, or a node the graph itself marked blocked/waiting.
    let mut blockers: Vec<Value> = Vec::new();
    let terminal: HashSet<&str> = ["done", "cancelled"].into_iter().collect();
    for row in &nodes {
        let node_id: String = row.get("id");
        let status: String = row.get("status");
        if terminal.contains(status.as_str()) {
            continue;
        }
        if let Some(unmet) = prerequisites.get(&node_id).and_then(|deps| {
            deps.iter()
                .find(|dep| status_of.get(*dep).is_some_and(|s| s != "done"))
        }) {
            blockers.push(json!({ "node_id": node_id, "kind": "prerequisite", "ref": unmet, "status": status_of.get(unmet) }));
            continue;
        }
        if let Some(run) = run_by_node.get(&node_id) {
            let run_status = run["status"].as_str().unwrap_or("");
            if PARKED_RUN_STATES.contains(&run_status) {
                let question = questions_by_node
                    .get(&node_id)
                    .and_then(|items| items.first())
                    .cloned();
                blockers.push(json!({ "node_id": node_id, "kind": "run", "ref": run["id"], "status": run_status, "question": question }));
                continue;
            }
        }
        if status == "blocked" || status == "waiting" {
            blockers.push(json!({ "node_id": node_id, "kind": "status", "ref": Value::Null, "status": status }));
        }
    }

    Ok(Json(json!({
        "workspace_id": query.workspace_id,
        "revision": revision,
        "nodes": nodes.iter().map(|row| {
            let id: String = row.get("id");
            json!({
                "id": id,
                "kind": row.get::<String, _>("kind"),
                "title": row.get::<String, _>("title"),
                "description": row.get::<Option<String>, _>("description"),
                "parent_id": row.get::<Option<String>, _>("parent_id"),
                "status": row.get::<String, _>("status"),
                "owner_agent_thread_id": row.get::<Option<String>, _>("owner_agent_thread_id"),
                "created_by": row.get::<Value, _>("created_by"),
                "completion_contract": row.get::<Value, _>("completion_contract"),
                "capability_needs": row.get::<Value, _>("capability_needs"),
                "budget_id": row.get::<Option<String>, _>("budget_id"),
                "priority": row.get::<i16, _>("priority"),
                "revision": row.get::<i64, _>("revision"),
                "thread_id": row.get::<Option<String>, _>("thread_id"),
                "origin": row.get::<String, _>("origin"),
                "run": run_by_node.get(&id).cloned().unwrap_or(Value::Null),
                "open_questions": questions_by_node.get(&id).cloned().unwrap_or_default(),
                "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
            })
        }).collect::<Vec<_>>(),
        "edges": edges.iter().map(|row| json!({
            "id": row.get::<String, _>("id"),
            "from_node_id": row.get::<String, _>("from_node_id"),
            "to_node_id": row.get::<String, _>("to_node_id"),
            "kind": row.get::<String, _>("kind"),
            "revision": row.get::<i64, _>("revision"),
        })).collect::<Vec<_>>(),
        "agents": agents.iter().map(|row| json!({
            "id": row.get::<String, _>("id"),
            "kind": row.get::<String, _>("agent_kind"),
            "parent_id": row.get::<Option<String>, _>("parent_id"),
            "work_node_id": row.get::<Option<String>, _>("work_node_id"),
            "status": row.get::<String, _>("status"),
            "generation": row.get::<i64, _>("generation"),
            "suspended_reason": row.get::<Option<String>, _>("suspended_reason"),
            "handoff_to_agent_thread_id": row.get::<Option<String>, _>("handoff_to_agent_thread_id"),
            "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        })).collect::<Vec<_>>(),
        "handoffs": handoffs.iter().map(|row| json!({
            "id": row.get::<String, _>("id"),
            "parent_agent_thread_id": row.get::<String, _>("parent_agent_thread_id"),
            "child_agent_thread_id": row.get::<String, _>("child_agent_thread_id"),
            "work_node_id": row.get::<Option<String>, _>("work_node_id"),
            "delegation_capability_id": row.get::<Option<String>, _>("delegation_capability_id"),
            "delegated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("delegated_at"),
            "joined_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("joined_at"),
        })).collect::<Vec<_>>(),
        "blockers": blockers,
    })))
}

// ---- commands ----

/// The planner for a user acting in the tenant, over the graph-backed workspace port.
fn planner(state: &ApiState, identity: &RuntimeIdentity) -> Result<Planner, ApiError> {
    let port: Arc<dyn PlanWorkspacePort> = Arc::new(GraphPlanWorkspace::new(
        state.pool().clone(),
        identity.clone(),
    ));
    Planner::new(state.pool().clone(), port, identity.clone())
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "planner"))
}

/// What the user may ask for in `capability_needs`: the allow rules of the workspace's
/// policies, as grant templates. An edit that needs more than that is widening.
async fn user_grants(
    state: &ApiState,
    identity: &RuntimeIdentity,
    workspace_id: &str,
) -> Result<Value, ApiError> {
    let store = PolicyStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "planner"))?;
    let policies = store
        .load_policy_set(workspace_id)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "planner"))?;
    let grants: Vec<Value> = policies
        .in_order()
        .iter()
        .flat_map(|policy| policy.rules.iter())
        .filter(|rule| rule.decision == PolicyDecision::Allow)
        .map(|rule| {
            Grant::new(
                rule.effect_class.clone(),
                rule.resource_selector.clone(),
                Constraints::default(),
            )
            .to_json()
        })
        .collect();
    Ok(Value::Array(grants))
}

fn plan_error(error: PlanError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "CONFLICT_REVISION" => ApiErrorCode::ConflictRevision,
        "VALIDATION_BOUNDS" => ApiErrorCode::ValidationBounds,
        "CAPABILITY_DENIED" => ApiErrorCode::CapabilityDenied,
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "NOT_FOUND" => ApiErrorCode::NotFound,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn outcome_json(outcome: &PlanOutcome) -> Value {
    json!({
        "proposal_id": outcome.proposal_id,
        "revision": outcome.revision,
        "applied": outcome.applied,
        "nodes_added": outcome.nodes_added,
        "nodes_updated": outcome.nodes_updated,
        "edges_added": outcome.edges_added,
        "edges_removed": outcome.edges_removed,
        "event_ids": outcome.event_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "mutations": outcome.mutation_summary,
    })
}

fn required_str<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("{key} is required"),
                correlation,
            )
        })
}

/// `ProposeGraphEdit`: a user-originated plan against the revision the user observed.
///
/// # Errors
/// `CONFLICT_REVISION` when `base_revision` is no longer current (nothing is written; the
/// caller reloads the projection and rebases), plus the planner's other typed rejections.
pub async fn propose_graph_edit(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let Some(user_id) = user_id else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "ProposeGraphEdit needs an acting user",
            correlation,
        ));
    };
    let workspace_id = required_str(params, "workspace_id", correlation)?;
    let base_revision = params
        .get("base_revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "base_revision is required",
                correlation,
            )
        })?;
    let proposal_id = format!("prop_{}", UlidGenerator::new().generate().to_base32());
    let raw = json!({
        "proposal_id": proposal_id,
        "run_id": Value::Null,
        "workspace_id": workspace_id,
        "base_revision": base_revision,
        "nodes_add": params.get("nodes_add").cloned().unwrap_or_else(|| json!([])),
        "nodes_update": params.get("nodes_update").cloned().unwrap_or_else(|| json!([])),
        "edges_add": params.get("edges_add").cloned().unwrap_or_else(|| json!([])),
        "edges_remove": params.get("edges_remove").cloned().unwrap_or_else(|| json!([])),
        "rationale": params.get("rationale").cloned().unwrap_or_else(|| json!("user edit")),
        "capability_needs": params.get("capability_needs").cloned().unwrap_or_else(|| json!([])),
    });
    let planner = planner(state, identity)?;
    let outcome = planner
        .propose(
            &raw,
            PlanProposerProjection {
                agent_thread_id: None,
                grants: user_grants(state, identity, workspace_id).await?,
                created_by: json!({ "kind": "user", "id": user_id }),
            },
        )
        .await
        .map_err(|error| plan_error(error, correlation))?;
    Ok(outcome_json(&outcome))
}

/// `CreateObjective`: the root of a new WorkGraph subtree and its `objective` thread,
/// created through the same validated path as any other plan.
///
/// # Errors
/// The planner's typed rejections; `AUTH_REQUIRED` without an acting user.
pub async fn create_objective(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let Some(user_id) = user_id else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "CreateObjective needs an acting user",
            correlation,
        ));
    };
    let workspace_id = required_str(params, "workspace_id", correlation)?;
    let title = required_str(params, "title", correlation)?.trim();
    if title.len() > 200 {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            "title must be at most 200 characters",
            correlation,
        ));
    }
    let contract = params
        .get("completion_contract")
        .cloned()
        .filter(|value| {
            value.is_object() && !value.as_object().is_some_and(serde_json::Map::is_empty)
        })
        .unwrap_or_else(|| {
            json!({
                "deterministic_checks": [],
                "semantic_verification": { "required": false },
                "human_signoff_required": true
            })
        });
    // The objective's thread first, so the node can point at it from birth.
    let conversation = ConversationStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let thread = conversation
        .create_thread(workspace_id, "objective", Some(title.to_string()))
        .await
        .map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationBounds,
                error.to_string(),
                correlation,
            )
        })?;
    let planner = planner(state, identity)?;
    let snapshot_revision: i64 = {
        let mut tx = state.pool().begin().await.map_err(projection_error)?;
        schema::set_tenant_context(&mut tx, &identity.tenant_id)
            .await
            .map_err(projection_error)?;
        let revision = sqlx::query_scalar(
            "SELECT revision FROM graph_heads WHERE tenant_id = $1 AND workspace_id = $2",
        )
        .bind(&identity.tenant_id)
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(projection_error)?
        .unwrap_or(1);
        tx.commit().await.map_err(projection_error)?;
        revision
    };
    let raw = json!({
        "proposal_id": format!("prop_{}", UlidGenerator::new().generate().to_base32()),
        "run_id": Value::Null,
        "workspace_id": workspace_id,
        "base_revision": snapshot_revision,
        "nodes_add": [{
            "id": "objective",
            "kind": "objective",
            "title": title,
            "description": params.get("description").cloned().unwrap_or(Value::Null),
            "status": "draft",
            "completion_contract": contract,
            "capability_needs": [],
            "priority": params.get("priority").and_then(Value::as_i64).unwrap_or(1),
            "thread_id": thread.id,
        }],
        "nodes_update": [],
        "edges_add": [],
        "edges_remove": [],
        "rationale": "CreateObjective",
        "capability_needs": [],
    });
    let outcome = planner
        .propose(
            &raw,
            PlanProposerProjection {
                agent_thread_id: None,
                grants: json!([]),
                created_by: json!({ "kind": "user", "id": user_id }),
            },
        )
        .await
        .map_err(|error| plan_error(error, correlation))?;
    // The node the plan created is the one bound to the thread.
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &identity.tenant_id)
        .await
        .map_err(projection_error)?;
    let objective_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM work_nodes WHERE tenant_id = $1 AND workspace_id = $2 AND thread_id = $3 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&identity.tenant_id)
    .bind(workspace_id)
    .bind(&thread.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let mut result = outcome_json(&outcome);
    result["objective_id"] = json!(objective_id);
    result["thread_id"] = json!(thread.id);
    result["workspace_id"] = json!(workspace_id);
    Ok(result)
}
