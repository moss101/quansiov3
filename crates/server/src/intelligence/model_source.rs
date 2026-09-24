//! The runtime's model proposal source over the intelligence plane (APP-001; DOMAIN.md §5.6, §11).
//!
//! One model call of a turn, as the walking skeleton requires it — runtime → gateway → provider:
//!
//! 1. **Canonical content.** The runtime reads what it owns: the teammate's instructions (or
//!    `config/intelligence.yaml`'s default), the thread up to the run's trigger message, and the
//!    tool rounds this run has made so far (each call with the result the runtime recorded).
//! 2. **Context.** The Context plane labels, packs and renders it (`BuildContext`, INT-005); the
//!    runtime's boundary check ([`validate_projection`]) accepts it before use.
//! 3. **Model call.** `FulfillModel` carries exactly the rendered messages and the tools the run's
//!    capability projection exposes; the gateway picks the route from `config/models.yaml`.
//! 4. **Proposal.** Text deltas become the assistant text, tool calls become proposed calls (the
//!    dispatcher validates them — nothing here decides what may run), a typed error event fails
//!    the step with its §15 code, and the route, usage and projection are recorded in the
//!    ModelCall ledger.
//!
//! The model proposes; the runtime commits. Nothing here writes canonical state other than the
//! ModelCall record a gateway boundary owns.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use quansio_contracts::generated::intelligence::model_event::Kind;
use quansio_contracts::generated::intelligence::model_route::RequestClass;
use quansio_contracts::generated::intelligence::{ModelCallRequest as WireCall, ModelEvent};
use quansio_tools::{SourceTrust, ToolRegistry};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::sync::Arc;

use crate::control::schema;
use crate::control::teammates::TeammateStore;
use crate::intelligence::{CallScope, IntelligenceClient, IntelligenceError};
use crate::runtime::context_bridge::{
    record_projection, validate_projection, ContextBridgeError, ContextBridgePort,
    ProjectionRequest, SegmentRequest, SegmentRole, SegmentSource,
};
use crate::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, ProposedToolCall, RuntimeError,
    RuntimeIdentity,
};
use crate::runtime::turn_loop::{
    ModelCallLedger, ProjectionProvider, RecordedCall, StoredProjectionProvider,
};

const INTELLIGENCE_YAML: &str = include_str!("../../../../config/intelligence.yaml");

/// How the runtime bounds one model call (`config/intelligence.yaml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCallSettings {
    /// Tokens the Context plane may spend on the call's context.
    pub context_token_budget: u32,
    /// Output tokens the call may produce.
    pub max_output_tokens: u32,
    /// How long the context build plus the model call may take.
    pub deadline: Duration,
    /// Thread messages offered to the plane.
    pub max_thread_messages: u32,
    /// Skills the plane may resolve into one call's context.
    pub max_skills: u32,
    /// The system instructions of a run without a teammate definition.
    pub default_system_instructions: String,
}

#[derive(Deserialize)]
struct RawSettings {
    model_call: RawModelCall,
    default_system_instructions: String,
}

#[derive(Deserialize)]
struct RawModelCall {
    context_token_budget: u32,
    max_output_tokens: u32,
    deadline_ms: u64,
    max_thread_messages: u32,
    max_skills: u32,
}

impl ModelCallSettings {
    /// The repository's `config/intelligence.yaml`, compiled in.
    ///
    /// # Errors
    /// The file is not the documented shape, or a bound is zero.
    pub fn load() -> Result<Self, String> {
        Self::parse(INTELLIGENCE_YAML)
    }

    /// Parse a settings document.
    ///
    /// # Errors
    /// The document is not the documented shape, or a bound is zero.
    pub fn parse(document: &str) -> Result<Self, String> {
        let raw: RawSettings = serde_yaml::from_str(document)
            .map_err(|error| format!("config/intelligence.yaml: {error}"))?;
        let call = raw.model_call;
        if call.context_token_budget == 0
            || call.max_output_tokens == 0
            || call.deadline_ms == 0
            || call.max_thread_messages == 0
        {
            return Err("config/intelligence.yaml: every model_call bound must be positive".into());
        }
        if raw.default_system_instructions.trim().is_empty() {
            return Err("config/intelligence.yaml: default_system_instructions is empty".into());
        }
        Ok(Self {
            context_token_budget: call.context_token_budget,
            max_output_tokens: call.max_output_tokens,
            deadline: Duration::from_millis(call.deadline_ms),
            max_thread_messages: call.max_thread_messages,
            max_skills: call.max_skills,
            default_system_instructions: raw.default_system_instructions,
        })
    }
}

/// [`ModelProposalSource`] over the intelligence plane's `BuildContext` and `FulfillModel`.
#[derive(Clone)]
pub struct GatewayModelSource {
    pool: PgPool,
    client: IntelligenceClient,
    context: Arc<dyn ContextBridgePort>,
    settings: ModelCallSettings,
}

impl std::fmt::Debug for GatewayModelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayModelSource")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl GatewayModelSource {
    /// A source reading canonical content from `pool`, building context through `context` and
    /// calling the model through `client`.
    #[must_use]
    pub fn new(
        pool: PgPool,
        client: IntelligenceClient,
        context: Arc<dyn ContextBridgePort>,
        settings: ModelCallSettings,
    ) -> Self {
        Self {
            pool,
            client,
            context,
            settings,
        }
    }
}

/// The registry's ACTIVE versions as skill candidates (`{skill_id, version_id, name, semver,
/// status, manifest}`); the plane resolves the relevant ones (INT-009).
async fn skill_candidates(pool: &PgPool, tenant_id: &str) -> Result<Vec<Value>, RuntimeError> {
    let versions = crate::control::skills::SkillStore::new(pool.clone())
        .active_versions(tenant_id)
        .await
        .map_err(|error| RuntimeError::intelligence("INTERNAL", true, error.to_string()))?;
    Ok(versions
        .into_iter()
        .map(|(skill, version)| {
            json!({
                "skill_id": skill.id.to_string(),
                "version_id": version.id.to_string(),
                "name": skill.name,
                "semver": version.semver,
                "status": version.status.as_str(),
                "manifest": version.manifest,
            })
        })
        .collect())
}

/// The run facts one call needs.
struct RunFacts {
    workspace_id: String,
    agent_thread_id: String,
    trigger_ref: Option<String>,
}

/// One tool call this run made, as the runtime recorded it.
struct RecordedToolCall {
    id: String,
    tool_name: String,
    args: Value,
    status: String,
    result: Option<Value>,
    trust_rank: i16,
}

/// What the event stream said.
#[derive(Default)]
struct StreamOutcome {
    text: String,
    tool_calls: Vec<ProposedToolCall>,
    stop_reason: String,
    usage: Value,
    route: Option<quansio_contracts::generated::intelligence::ModelRoute>,
    route_id: String,
    error: Option<(String, bool)>,
}

#[async_trait]
impl ModelProposalSource for GatewayModelSource {
    async fn propose(&self, request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        // One span per model call (OPS-003); its context build and model call are children.
        let span = crate::observability::Span::start(
            "runtime.model_call",
            json!({
                "tenant_id": request.tenant_id,
                "run_id": request.run_id,
                "turn_id": request.turn_id,
                "step_id": request.step_id,
            }),
        );
        let result = span
            .context()
            .clone()
            .scope(self.propose_in_span(request))
            .await;
        match &result {
            Ok(_) => span.end("ok", json!({})),
            Err(error) => span.end("error", json!({ "code": error.code() })),
        }
        result
    }
}

impl GatewayModelSource {
    async fn propose_in_span(&self, request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let started = Instant::now();
        let run = self.run_facts(&request).await?;
        let definition = TeammateStore::new(self.pool.clone())
            .turn_definition(&request.tenant_id, &run.agent_thread_id)
            .await
            .ok()
            .flatten();

        // 1. Canonical content.
        let (mut segments, snapshot, task_text) = self.conversation(&request, &run).await?;
        let instructions = definition.as_ref().map_or_else(
            || self.settings.default_system_instructions.clone(),
            |definition| {
                [
                    definition.persona.trim(),
                    definition.standing_instructions.trim(),
                ]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n\n")
            },
        );
        let instructions = if instructions.trim().is_empty() {
            self.settings.default_system_instructions.clone()
        } else {
            instructions
        };
        segments.insert(
            0,
            SegmentRequest {
                seq: 0,
                source: SegmentSource::System,
                source_ref: definition.as_ref().map_or_else(
                    || "config/intelligence.yaml".to_string(),
                    |definition| format!("{}@v{}", definition.teammate_id, definition.version),
                ),
                role: SegmentRole::System,
                content: Value::String(instructions),
                trust_hint: None,
                snapshot_ref: snapshot.clone(),
                required: true,
            },
        );

        // 2. Context, checked at the boundary.
        let identity = RuntimeIdentity::system(
            &request.tenant_id,
            "gateway-model-source",
            quansio_core::CorrelationId::generate(&mut quansio_core::UlidGenerator::new()),
        );
        let projection = StoredProjectionProvider::new(self.pool.clone(), identity)?
            .projection_for_run(&request.run_id)
            .await?;
        let capability_projection_id = projection
            .as_ref()
            .map(|projection| projection.id.to_string())
            .unwrap_or_default();
        let tools: Vec<String> = projection
            .as_ref()
            .map(|projection| {
                ToolRegistry::builtin()
                    .exposed_names(projection, Utc::now())
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut granted_effect_classes: Vec<String> = projection
            .as_ref()
            .map(|projection| {
                projection
                    .grants
                    .iter()
                    .map(|grant| grant.effect_class.as_str().to_string())
                    .collect()
            })
            .unwrap_or_default();
        granted_effect_classes.sort();
        granted_effect_classes.dedup();
        let received = self
            .context
            .project(ProjectionRequest {
                tenant_id: request.tenant_id.clone(),
                run_id: request.run_id.clone(),
                turn_id: request.turn_id.clone(),
                workspace_id: run.workspace_id.clone(),
                capability_projection_id: capability_projection_id.clone(),
                snapshot_id: snapshot.clone(),
                token_budget: self.settings.context_token_budget,
                segments,
                skill_candidates: skill_candidates(&self.pool, &request.tenant_id).await?,
                task_text,
                exposed_tools: tools.clone(),
                granted_effect_classes,
                max_skills: self.settings.max_skills,
                ..ProjectionRequest::default()
            })
            .await
            .map_err(context_error)?;
        let validated = validate_projection(received, Some(&snapshot)).map_err(context_error)?;
        // What the model is about to see is recorded first (DOSSIER.md §8 layer 3).
        record_projection(
            &self.pool,
            &request.tenant_id,
            &request.run_id,
            &request.turn_id,
            &validated,
        )
        .await?;

        // 3. The model call.
        let call_id = format!("call_{}", request.step_id);
        let deadline_ms = u64::try_from(self.settings.deadline.as_millis()).unwrap_or(u64::MAX);
        let wire = WireCall {
            schema_version: crate::intelligence::SCHEMA_VERSION.to_string(),
            call_id: call_id.clone(),
            run_id: request.run_id.clone(),
            turn_id: request.turn_id.clone(),
            route_hint: definition
                .as_ref()
                .and_then(|definition| definition.model_route_preference.clone())
                .unwrap_or_default(),
            capability_projection_id: capability_projection_id.clone(),
            context_projection_id: validated.projection_id.clone(),
            messages: validated.messages.clone(),
            tools,
            output_constraints_json: String::new(),
            effort: String::new(),
            max_output_tokens: self.settings.max_output_tokens,
            stream: true,
            dlp_profile: String::new(),
            timeout_ms: deadline_ms,
            cancellation_token: request.step_id.clone(),
        };
        let scope = CallScope {
            tenant_id: request.tenant_id.clone(),
            workspace_id: run.workspace_id.clone(),
            correlation_id: format!("{}:{}", request.run_id, request.step_id),
            deadline: self.settings.deadline,
            capability_projection_id,
        };
        let mut client = self.client.clone();
        let events = client
            .fulfill_model(&scope, wire)
            .await
            .map_err(model_error)?;
        let outcome = read_stream(&events);

        // 4. Record, then propose.
        let latency = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let route = outcome.route.clone().unwrap_or_default();
        let provider = if route.provider.is_empty() { "unknown" } else { route.provider.as_str() };
        crate::observability::metrics::increment(
            "quansio_model_calls_total",
            "Model calls through the intelligence gateway by provider and stop reason.",
            &[("provider", provider), ("stop_reason", &outcome.stop_reason)],
        );
        crate::observability::metrics::observe(
            "quansio_model_call_duration_seconds",
            "Model call latency (context build plus model call) by provider.",
            &[("provider", provider)],
            started.elapsed().as_secs_f64(),
        );
        let _ = ModelCallLedger::new(self.pool.clone())
            .record(
                &request,
                &RecordedCall {
                    provider: route.provider.clone(),
                    model_id: route.model_id.clone(),
                    request_class: request_class(route.request_class()),
                    chosen_by: route.chosen_by.clone(),
                    stop_reason: outcome.stop_reason.clone(),
                    latency_ms: latency,
                    usage: outcome.usage.clone(),
                    route_id: (!outcome.route_id.is_empty()).then(|| outcome.route_id.clone()),
                    context_projection_id: Some(validated.projection_id.clone()),
                },
            )
            .await;
        if let Some((code, retryable)) = outcome.error {
            return Err(RuntimeError::intelligence(
                &code,
                retryable,
                format!("the model call {call_id} failed"),
            ));
        }
        Ok(ModelProposal {
            assistant_text: (!outcome.text.is_empty()).then_some(outcome.text),
            tool_calls: outcome.tool_calls,
            ..ModelProposal::default()
        })
    }
}

impl GatewayModelSource {
    async fn run_facts(&self, request: &ModelCallRequest) -> Result<RunFacts, RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &request.tenant_id).await?;
        let row = sqlx::query(
            "SELECT workspace_id, agent_thread_id, trigger_ref FROM runs \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&request.tenant_id)
        .bind(&request.run_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let row = row.ok_or_else(|| RuntimeError::NotFound {
            entity: "run",
            id: request.run_id.clone(),
            tenant_id: request.tenant_id.clone(),
        })?;
        Ok(RunFacts {
            workspace_id: row.try_get("workspace_id")?,
            agent_thread_id: row.try_get("agent_thread_id")?,
            trigger_ref: row.try_get("trigger_ref")?,
        })
    }

    /// The thread up to the trigger, then this run's tool rounds; the snapshot they are at; and the
    /// trigger's text, which is what the task is.
    async fn conversation(
        &self,
        request: &ModelCallRequest,
        run: &RunFacts,
    ) -> Result<(Vec<SegmentRequest>, String, String), RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &request.tenant_id).await?;
        let trigger = match &run.trigger_ref {
            Some(reference) => {
                sqlx::query("SELECT thread_id, seq FROM messages WHERE tenant_id = $1 AND id = $2")
                    .bind(&request.tenant_id)
                    .bind(reference)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            None => None,
        };
        let Some(trigger) = trigger else {
            tx.commit().await?;
            return Err(RuntimeError::InvalidArgument(format!(
                "run {} has no trigger message, so a model call has no conversation to answer",
                request.run_id
            )));
        };
        let thread_id: String = trigger.try_get("thread_id")?;
        let trigger_seq: i64 = trigger.try_get("seq")?;
        let messages = sqlx::query(
            "SELECT id, seq, author_kind, content_blocks FROM ( \
                SELECT id, seq, author_kind, content_blocks FROM messages \
                WHERE tenant_id = $1 AND thread_id = $2 AND seq <= $3 \
                ORDER BY seq DESC LIMIT $4) recent ORDER BY seq",
        )
        .bind(&request.tenant_id)
        .bind(&thread_id)
        .bind(trigger_seq)
        .bind(i64::from(self.settings.max_thread_messages))
        .fetch_all(&mut *tx)
        .await?;
        let steps = sqlx::query(
            "SELECT s.id, s.kind FROM steps s JOIN turns t ON t.id = s.turn_id \
             WHERE t.tenant_id = $1 AND t.run_id = $2 ORDER BY t.seq, s.seq",
        )
        .bind(&request.tenant_id)
        .bind(&request.run_id)
        .fetch_all(&mut *tx)
        .await?;
        let calls = sqlx::query(
            "SELECT c.id, c.step_id, c.tool_name, c.args, c.status, c.result, c.trust_level \
             FROM tool_calls c JOIN steps s ON s.id = c.step_id JOIN turns t ON t.id = s.turn_id \
             WHERE c.tenant_id = $1 AND t.run_id = $2 ORDER BY c.created_at, c.id",
        )
        .bind(&request.tenant_id)
        .bind(&request.run_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let snapshot = format!(
            "{thread_id}@{trigger_seq}/{}@{}",
            request.run_id,
            calls.len()
        );
        let mut segments = Vec::new();
        let mut task_text = String::new();
        let mut seq: u32 = 1;
        for message in &messages {
            let text = message_text(&message.try_get::<Value, _>("content_blocks")?);
            if text.trim().is_empty() {
                continue;
            }
            let id: String = message.try_get("id")?;
            let author: String = message.try_get("author_kind")?;
            let (role, trust_hint) = match author.as_str() {
                "user" => (SegmentRole::User, None),
                "agent" => (SegmentRole::Assistant, None),
                // Runtime-authored thread content (routine notices) is not the user's voice.
                _ => (SegmentRole::User, Some(SourceTrust::AgentGenerated)),
            };
            let trigger = run.trigger_ref.as_deref() == Some(id.as_str());
            if trigger {
                task_text.clone_from(&text);
            }
            segments.push(SegmentRequest {
                seq,
                source: SegmentSource::Thread,
                required: trigger,
                source_ref: id,
                role,
                content: Value::String(text),
                trust_hint,
                snapshot_ref: snapshot.clone(),
            });
            seq += 1;
        }

        // This run's tool rounds: the calls following each model_call step, with their results.
        let mut rounds: Vec<Vec<RecordedToolCall>> = Vec::new();
        for step in &steps {
            let step_id: String = step.try_get("id")?;
            let kind: String = step.try_get("kind")?;
            if kind == "model_call" {
                rounds.push(Vec::new());
                continue;
            }
            for call in calls.iter().filter(|call| {
                call.try_get::<String, _>("step_id").ok().as_deref() == Some(step_id.as_str())
            }) {
                let recorded = RecordedToolCall {
                    id: call.try_get("id")?,
                    tool_name: call.try_get("tool_name")?,
                    args: call.try_get("args")?,
                    status: call.try_get("status")?,
                    result: call.try_get("result")?,
                    trust_rank: call.try_get("trust_level")?,
                };
                match rounds.last_mut() {
                    Some(round) => round.push(recorded),
                    None => rounds.push(vec![recorded]),
                }
            }
        }
        for round in rounds.into_iter().filter(|round| !round.is_empty()) {
            let tool_calls: Vec<Value> = round
                .iter()
                .map(|call| json!({"id": call.id, "name": call.tool_name, "args": object(&call.args)}))
                .collect();
            segments.push(SegmentRequest {
                seq,
                source: SegmentSource::Thread,
                source_ref: request.run_id.clone(),
                role: SegmentRole::Assistant,
                content: json!({"parts": [], "tool_calls": tool_calls}),
                trust_hint: None,
                snapshot_ref: snapshot.clone(),
                required: false,
            });
            seq += 1;
            for call in round {
                let outcome = call
                    .result
                    .clone()
                    .unwrap_or_else(|| json!({"status": call.status}));
                segments.push(SegmentRequest {
                    seq,
                    source: SegmentSource::ToolResult,
                    source_ref: call.id.clone(),
                    role: SegmentRole::Tool,
                    content: json!({
                        "parts": [{"kind": "text", "text": outcome.to_string()}],
                        "tool_call_id": call.id,
                        "tool_name": call.tool_name,
                    }),
                    trust_hint: trust_from_rank(call.trust_rank),
                    snapshot_ref: snapshot.clone(),
                    required: false,
                });
                seq += 1;
            }
        }
        Ok((segments, snapshot, task_text))
    }
}

/// The text of a message's content blocks.
fn message_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Recorded arguments as the object a tool call carries (a non-object is wrapped, never dropped).
fn object(args: &Value) -> Value {
    if args.is_object() {
        args.clone()
    } else {
        json!({ "value": args })
    }
}

fn trust_from_rank(rank: i16) -> Option<SourceTrust> {
    SourceTrust::ALL
        .iter()
        .copied()
        .find(|trust| i16::from(trust.rank()) == rank)
}

fn request_class(class: RequestClass) -> String {
    match class {
        RequestClass::Unspecified | RequestClass::Chat => "chat",
        RequestClass::Planning => "planning",
        RequestClass::ToolHeavy => "tool_heavy",
        RequestClass::Synthesis => "synthesis",
        RequestClass::Verification => "verification",
        RequestClass::Embedding => "embedding",
        RequestClass::CheapWorker => "cheap_worker",
    }
    .to_string()
}

fn stop_reason(event: &ModelEvent) -> &'static str {
    use quansio_contracts::generated::intelligence::ModelStopReason;
    match event.stop_reason() {
        ModelStopReason::EndTurn => "end_turn",
        ModelStopReason::ToolUse => "tool_use",
        ModelStopReason::MaxTokens => "max_tokens",
        ModelStopReason::Refusal => "refusal",
        ModelStopReason::Cancelled => "cancelled",
        ModelStopReason::Error | ModelStopReason::Unspecified => "error",
    }
}

fn read_stream(events: &[ModelEvent]) -> StreamOutcome {
    let mut outcome = StreamOutcome {
        usage: json!({ "input": 0, "output": 0 }),
        ..StreamOutcome::default()
    };
    for event in events {
        if outcome.route_id.is_empty() && !event.route_id.is_empty() {
            outcome.route_id.clone_from(&event.route_id);
        }
        match event.kind() {
            Kind::CallStarted | Kind::CallCompleted => {
                if outcome.route.is_none() {
                    outcome.route.clone_from(&event.route);
                }
            }
            Kind::Delta => outcome.text.push_str(&event.text_delta),
            Kind::ToolCall => outcome.tool_calls.push(ProposedToolCall::new(
                event.tool_call_id.clone(),
                event.tool_name.clone(),
                serde_json::from_str(&event.tool_args_json)
                    .unwrap_or_else(|_| Value::String(event.tool_args_json.clone())),
            )),
            Kind::Usage => {
                if let Some(usage) = &event.usage {
                    outcome.usage = json!({
                        "input": usage.input_tokens,
                        "output": usage.output_tokens,
                        "cache_read": usage.cache_read_tokens,
                        "cache_write": usage.cache_write_tokens,
                    });
                }
            }
            Kind::Stop => outcome.stop_reason = stop_reason(event).to_string(),
            Kind::Error => {
                if let Some(error) = &event.error {
                    outcome.error = Some((error.code.clone(), error.retryable));
                }
            }
            Kind::ThinkingSummary | Kind::Unspecified => {}
        }
    }
    if outcome.stop_reason.is_empty() {
        outcome.stop_reason = "error".to_string();
        outcome
            .error
            .get_or_insert(("PROVIDER_UNAVAILABLE".to_string(), true));
    }
    outcome
}

fn context_error(error: ContextBridgeError) -> RuntimeError {
    let retryable = matches!(error, ContextBridgeError::Unavailable { .. });
    RuntimeError::intelligence(error.code(), retryable, error.to_string())
}

fn model_error(error: IntelligenceError) -> RuntimeError {
    match error {
        IntelligenceError::Refused { code, message } => {
            RuntimeError::intelligence(&code, false, message)
        }
        IntelligenceError::DeadlineExceeded => RuntimeError::intelligence(
            "PROVIDER_UNAVAILABLE",
            true,
            "the model call exceeded its deadline",
        ),
        other => RuntimeError::intelligence("PROVIDER_UNAVAILABLE", true, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_settings_load() {
        let settings = ModelCallSettings::load().expect("config/intelligence.yaml");
        assert!(settings.context_token_budget > settings.max_output_tokens);
        assert!(!settings.default_system_instructions.trim().is_empty());
    }

    #[test]
    fn a_zero_bound_or_missing_instructions_is_refused() {
        let base = "model_call:\n  context_token_budget: 10\n  max_output_tokens: 5\n  deadline_ms: 1000\n  max_thread_messages: 3\n  max_skills: 2\ndefault_system_instructions: be useful\n";
        assert!(ModelCallSettings::parse(base).is_ok());
        assert!(
            ModelCallSettings::parse(&base.replace("deadline_ms: 1000", "deadline_ms: 0")).is_err()
        );
        assert!(ModelCallSettings::parse(&base.replace("be useful", "\"\"")).is_err());
        assert!(ModelCallSettings::parse("model_call: {}").is_err());
    }

    fn event(kind: Kind) -> ModelEvent {
        ModelEvent {
            kind: kind as i32,
            route_id: "mr_1".to_string(),
            ..ModelEvent::default()
        }
    }

    #[test]
    fn a_stream_becomes_text_tool_calls_and_a_record() {
        let mut delta = event(Kind::Delta);
        delta.text_delta = "reading".to_string();
        let mut call = event(Kind::ToolCall);
        call.tool_call_id = "call_1".to_string();
        call.tool_name = "fs.read".to_string();
        call.tool_args_json = "{\"path\":\"/a\"}".to_string();
        let mut stop = event(Kind::Stop);
        stop.stop_reason =
            quansio_contracts::generated::intelligence::ModelStopReason::ToolUse as i32;
        let outcome = read_stream(&[
            event(Kind::CallStarted),
            delta,
            call,
            stop,
            event(Kind::CallCompleted),
        ]);
        assert_eq!(outcome.text, "reading");
        assert_eq!(
            outcome.tool_calls,
            vec![ProposedToolCall::new(
                "call_1",
                "fs.read",
                json!({"path": "/a"})
            )]
        );
        assert_eq!(outcome.stop_reason, "tool_use");
        assert_eq!(outcome.route_id, "mr_1");
        assert!(outcome.error.is_none());
    }

    #[test]
    fn an_error_event_or_a_stream_without_a_stop_fails_the_call() {
        let mut failed = event(Kind::Error);
        failed.error = Some(quansio_contracts::generated::intelligence::ModelError {
            code: "PROVIDER_RATE_LIMITED".to_string(),
            retryable: true,
            ..Default::default()
        });
        let outcome = read_stream(&[event(Kind::CallStarted), failed, event(Kind::CallCompleted)]);
        assert_eq!(
            outcome.error,
            Some(("PROVIDER_RATE_LIMITED".to_string(), true))
        );
        let cut = read_stream(&[event(Kind::CallStarted)]);
        assert_eq!(cut.error, Some(("PROVIDER_UNAVAILABLE".to_string(), true)));
        let error = RuntimeError::intelligence("PROVIDER_RATE_LIMITED", true, "x");
        assert_eq!(error.code(), "PROVIDER_RATE_LIMITED");
        assert_eq!(
            RuntimeError::intelligence("SOMETHING_ELSE", false, "x").code(),
            "INTERNAL"
        );
    }

    #[test]
    fn a_tool_calls_recorded_trust_rank_becomes_its_hint() {
        assert_eq!(trust_from_rank(5), Some(SourceTrust::UntrustedExternal));
        assert_eq!(trust_from_rank(1), Some(SourceTrust::TrustedSystem));
        assert_eq!(trust_from_rank(9), None);
    }
}
