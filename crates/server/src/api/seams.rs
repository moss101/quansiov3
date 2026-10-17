//! Composition seams for the walking skeleton (APP-001).
//!
//! The model gateway's deterministic conformance stub is INT-002's offline provider.
//! This module is the in-process port the runtime already uses: it proposes the same
//! `fs.read` tool the stub's `tool_call` scenario uses, then finishes the turn. It is
//! not live-provider evidence.
//!
//! APP-004 adds a scenario the conversation surface can drive end to end: a trigger
//! message containing [`QUESTION_SCENARIO`] makes the stub ask a `single_choice`
//! question first and reply with the chosen option once the answer resumes the run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::control::schema;
use crate::policy::{ActorRoles, TenantRole, WorkspaceRole};
use crate::runtime::state_machine::{
    DelegationRequest, ModelCallRequest, ModelProposal, ModelProposalSource, ProposedQuestion,
    ProposedToolCall, RuntimeError,
};
use crate::runtime::turn_loop::{
    ActingActor, HostDispatch, HostFailure, HostOutcome, ModelCallLedger, RecordedCall,
    RoleProvider, ToolHostPort,
};

/// Marker in a trigger message that selects the question scenario.
pub const QUESTION_SCENARIO: &str = "[[scenario:question]]";
/// Marker in a trigger message that selects the approval scenario: the stub proposes an
/// `fs.write`, whose class a test policy sets to `ask`, so the run parks `WAITING_APPROVAL`;
/// once the approval resumes it, the stub finishes.
pub const APPROVAL_SCENARIO: &str = "[[scenario:approval]]";
/// Marker in a trigger message that selects the delegation scenario: the stub hands the
/// run's work node to a worker (RUN-002's port) and the run parks in `WAITING_CHILD`.
pub const DELEGATE_SCENARIO: &str = "[[scenario:delegate]]";
/// The provider the seam records itself as on the ModelRoute rows it writes.
pub const SEAM_PROVIDER: &str = "in-process";
/// The model the seam records itself as; not a `config/models.yaml` model, and not a
/// live-provider record.
pub const SEAM_MODEL_ID: &str = "conformance-stub";
/// The choices the question scenario offers.
pub const QUESTION_OPTIONS: [&str; 2] = ["alpha", "beta"];

/// Conformance-stub model: one `fs.read` proposal, then assistant text.
#[derive(Debug, Default)]
pub struct ConformanceStubModel {
    calls: Mutex<HashMap<String, u32>>,
    pool: Option<PgPool>,
}

impl ConformanceStubModel {
    /// A fresh stub.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A stub that can read the run's trigger message to pick a scenario.
    #[must_use]
    pub fn with_pool(pool: PgPool) -> Self {
        Self {
            calls: Mutex::new(HashMap::new()),
            pool: Some(pool),
        }
    }

    /// The text of the message that triggered `run_id`, when the run has one.
    async fn trigger_text(&self, tenant_id: &str, run_id: &str) -> Option<String> {
        let pool = self.pool.as_ref()?;
        let mut tx = pool.begin().await.ok()?;
        schema::set_tenant_context(&mut tx, tenant_id).await.ok()?;
        let blocks: Option<Value> = sqlx::query_scalar(
            "SELECT m.content_blocks FROM runs r JOIN messages m ON m.id = r.trigger_ref \
             AND m.tenant_id = r.tenant_id WHERE r.tenant_id = $1 AND r.id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
        let text = blocks?
            .as_array()?
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        Some(text)
    }

    /// Record the ModelRoute and ModelCall rows for this proposal through the gateway
    /// ledger (DOMAIN.md §11.1: the boundary writes the durable ModelCallRecord at the end
    /// of the call). The route names the seam itself, so the inspector says truthfully
    /// what answered.
    async fn record_call(&self, request: &ModelCallRequest, stop_reason: &str, latency_ms: i64) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };
        // The definition behind the run's teammate is read at turn time (APP-014): the
        // route record names it — its version and its route preference — so a change to
        // the definition is visible on the very next turn, without restart. A real model
        // source would build its system segment from the same read.
        let chosen_by = match self
            .turn_definition(&request.tenant_id, &request.run_id)
            .await
        {
            Some(definition) => format!(
                "seam:conformance;teammate:{}@v{}{}",
                definition.teammate_id,
                definition.version,
                definition
                    .model_route_preference
                    .as_deref()
                    .map(|route| format!(";prefers:{route}"))
                    .unwrap_or_default()
            ),
            None => "seam:conformance".to_string(),
        };
        // A ledger failure must not fail the turn: the proposal is already made, and the
        // step's own status stays the runtime's truth.
        let _ = ModelCallLedger::new(pool.clone())
            .record(
                request,
                &RecordedCall {
                    provider: SEAM_PROVIDER.to_string(),
                    model_id: SEAM_MODEL_ID.to_string(),
                    request_class: "chat".to_string(),
                    chosen_by,
                    stop_reason: stop_reason.to_string(),
                    latency_ms,
                    usage: json!({ "input": 0, "output": 0 }),
                    route_id: None,
                    context_projection_id: None,
                },
            )
            .await;
    }

    /// The teammate definition behind the run's AgentThread, as of now.
    async fn turn_definition(
        &self,
        tenant_id: &str,
        run_id: &str,
    ) -> Option<crate::control::teammates::TurnDefinition> {
        let pool = self.pool.as_ref()?;
        let mut tx = pool.begin().await.ok()?;
        schema::set_tenant_context(&mut tx, tenant_id).await.ok()?;
        let thread: Option<String> =
            sqlx::query_scalar("SELECT agent_thread_id FROM runs WHERE tenant_id = $1 AND id = $2")
                .bind(tenant_id)
                .bind(run_id)
                .fetch_optional(&mut *tx)
                .await
                .ok()
                .flatten();
        drop(tx);
        crate::control::teammates::TeammateStore::new(pool.clone())
            .turn_definition(tenant_id, &thread?)
            .await
            .ok()
            .flatten()
    }

    /// The latest answered question of `run_id`, as `(prompt, answer value)`.
    async fn answered(&self, tenant_id: &str, run_id: &str) -> Option<(String, Value)> {
        let pool = self.pool.as_ref()?;
        let mut tx = pool.begin().await.ok()?;
        schema::set_tenant_context(&mut tx, tenant_id).await.ok()?;
        let row: Option<(String, Value)> = sqlx::query_as(
            "SELECT prompt, answer FROM questions WHERE tenant_id = $1 AND run_id = $2 \
             AND status = 'answered' ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
        row
    }
}

#[async_trait]
impl ModelProposalSource for ConformanceStubModel {
    async fn propose(&self, request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let started = std::time::Instant::now();
        let proposal = self.propose_inner(&request).await?;
        let stop_reason = if proposal.tool_calls.is_empty() {
            "end_turn"
        } else {
            "tool_use"
        };
        let latency = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        self.record_call(&request, stop_reason, latency).await;
        Ok(proposal)
    }
}

impl ConformanceStubModel {
    async fn propose_inner(
        &self,
        request: &ModelCallRequest,
    ) -> Result<ModelProposal, RuntimeError> {
        let count = {
            let mut calls = self.calls.lock().unwrap_or_else(|error| error.into_inner());
            let count = calls.entry(request.run_id.clone()).or_insert(0);
            *count += 1;
            *count
        };
        let scenario = self
            .trigger_text(&request.tenant_id, &request.run_id)
            .await
            .unwrap_or_default();
        if scenario.contains(APPROVAL_SCENARIO) {
            if count == 1 {
                let content = "approved content";
                return Ok(ModelProposal {
                    assistant_text: Some("writing the file once you approve.".to_string()),
                    tool_calls: vec![ProposedToolCall::new(
                        "call_approval",
                        "fs.write",
                        json!({
                            "path": "out.txt",
                            "content": content,
                            "content_digest": quansio_core::Digest::of(content.as_bytes()).to_string(),
                        }),
                    )],
                    ..ModelProposal::default()
                });
            }
            return Ok(ModelProposal {
                assistant_text: Some("the file is written.".to_string()),
                ..ModelProposal::default()
            });
        }
        if scenario.contains(DELEGATE_SCENARIO) && count == 1 {
            return Ok(ModelProposal {
                assistant_text: Some("handing this to a worker.".to_string()),
                delegate_requests: vec![DelegationRequest {
                    request_id: "dlg_conformance".to_string(),
                    work_node_id: None,
                    instruction: "do the delegated part".to_string(),
                }],
                ..ModelProposal::default()
            });
        }
        if scenario.contains(QUESTION_SCENARIO) {
            return match self.answered(&request.tenant_id, &request.run_id).await {
                None => Ok(ModelProposal {
                    assistant_text: Some("I need one answer before I continue.".to_string()),
                    question: Some(ProposedQuestion {
                        question_id: format!("q_conformance_{count}"),
                        prompt: "Which option should I take?".to_string(),
                        kind: "single_choice".to_string(),
                        options: QUESTION_OPTIONS.iter().map(ToString::to_string).collect(),
                        required: true,
                    }),
                    ..ModelProposal::default()
                }),
                Some((_, answer)) => {
                    let chosen = answer.get("value").map_or_else(
                        || answer.to_string(),
                        |value| match value.as_str() {
                            Some(text) => text.to_string(),
                            None => value.to_string(),
                        },
                    );
                    Ok(ModelProposal {
                        assistant_text: Some(format!("done: you chose {chosen}")),
                        ..ModelProposal::default()
                    })
                }
            };
        }
        if count == 1 {
            Ok(ModelProposal {
                assistant_text: Some("working".to_string()),
                tool_calls: vec![ProposedToolCall::new(
                    "call_conformance",
                    "fs.read",
                    json!({ "path": "notes.md" }),
                )],
                ..ModelProposal::default()
            })
        } else {
            Ok(ModelProposal {
                assistant_text: Some("done".to_string()),
                ..ModelProposal::default()
            })
        }
    }
}

/// Host that settles a dispatch without leaving the process.
#[derive(Debug, Default)]
pub struct ConformanceStubHost;

#[async_trait]
impl ToolHostPort for ConformanceStubHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        Ok(HostOutcome::Success {
            output: json!({
                "ok": true,
                "tool": dispatch.tool,
                "stub": "conformance",
            }),
            evidence_ids: Vec::new(),
            remote_ref: None,
        })
    }
}

/// Session identity until APP-002 owns it: a workspace member who can approve.
#[derive(Debug, Clone)]
pub struct TenantMemberRoles {
    user_id: Option<String>,
}

impl TenantMemberRoles {
    /// Roles for an optional user.
    #[must_use]
    pub fn new(user_id: Option<String>) -> Self {
        Self { user_id }
    }
}

#[async_trait]
impl RoleProvider for TenantMemberRoles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Approver)),
            user_id: self.user_id.clone(),
        }))
    }
}

/// Shared walking-skeleton seams.
pub fn skeleton_seams(
    pool: PgPool,
    user_id: Option<String>,
) -> (
    Arc<dyn ModelProposalSource>,
    Arc<dyn ToolHostPort>,
    Arc<dyn RoleProvider>,
) {
    (
        Arc::new(ConformanceStubModel::with_pool(pool)),
        Arc::new(ConformanceStubHost),
        Arc::new(TenantMemberRoles::new(user_id)),
    )
}
