//! Mutating command dispatch (APP-001).
//!
//! The handler records the command in `commands` and then calls the canonical owner.

use std::sync::Arc;

use super::{ApiError, ApiErrorCode, ApiState, CommandRequest, CommandResponse, TenantScope};
use crate::control::conversation::ConversationStore;
use crate::runtime::agents::AgentDelegationPort;
use crate::runtime::agents::{AgentKind, AgentStore, NewAgentThread};
use crate::runtime::state_machine::{
    NewRun, Run, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
    TurnOutcome,
};
use crate::runtime::turn_loop::{
    QuestionAnswer, QuestionService, StoredProjectionProvider, ToolDispatchService,
};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use quansio_capability::persistence::{stage_projected_event, ProjectionArgument, ProjectionRow};
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Digest, Generation, Prefix, UlidGenerator};
use quansio_events::EventStore;
use serde_json::{json, Value};

/// Apply a catalog command.
pub async fn invoke(
    State(state): State<ApiState>,
    Path(command): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let command_id = TenantScope::command_id(&request)?;
    let correlation_id = CorrelationId::generate(&mut UlidGenerator::new());
    let correlation = correlation_id.to_string();
    if !state.limits().allow(&scope.tenant_id) {
        return Err(ApiError::new(
            ApiErrorCode::RateLimited,
            "this tenant has exceeded the API rate limit",
            correlation,
        ));
    }

    let params_digest = Digest::of_canonical_json(&request.params.to_string())
        .as_str()
        .to_string();
    let known = state
        .catalog()
        .commands()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&command));
    if !known {
        return Err(ApiError::new(
            ApiErrorCode::NotFound,
            format!("{command:?} is not a command in the catalog"),
            correlation,
        ));
    }

    let inserted = sqlx::query(
        "INSERT INTO commands (id, tenant_id, command_name, command_id, params_digest, status, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, 'accepted', $6) \
         ON CONFLICT (tenant_id, command_id) DO NOTHING",
    )
    .bind(command_id.as_canonical().to_string())
    .bind(&scope.tenant_id)
    .bind(&command)
    .bind(command_id.as_canonical().to_string())
    .bind(&params_digest)
    .bind(&correlation)
    .execute(state.pool())
    .await
    .map_err(database_error)?;

    if inserted.rows_affected() == 0 {
        return replay(&state, &scope, &command_id, &params_digest, &correlation).await;
    }

    let user_id = scope.user_id.clone();
    let identity = RuntimeIdentity::system(&scope.tenant_id, "api", correlation_id)
        .with_command_id(command_id);
    let result = match command.as_str() {
        "CreateWorkspace" | "UpdateWorkspace" | "UpdateUserSettings" => {
            identity_command(&state, &scope, &command, &request.params, &correlation).await?
        }
        "CancelRun" => cancel_run(&state, &identity, &request.params, &correlation).await?,
        "PauseRun" => pause_run(&state, &identity, &request.params, &correlation).await?,
        "ResumeRun" => resume_run(&state, &identity, &request.params, &correlation).await?,
        "CreateThread" => create_thread(&state, &identity, &request.params, &correlation).await?,
        "PostMessage" => {
            post_message(
                &state,
                &identity,
                &request.params,
                user_id.as_deref(),
                &correlation,
            )
            .await?
        }
        "AnswerQuestion" => {
            answer_question(
                &state,
                &identity,
                &request.params,
                user_id.as_deref(),
                &correlation,
            )
            .await?
        }
        "CancelTurn" => cancel_turn(&state, &identity, &request.params, &correlation).await?,
        "CreateArtifact" => {
            super::artifacts::create_artifact(
                &state,
                &scope.tenant_id,
                user_id.as_deref(),
                &request.params,
                &correlation,
            )
            .await?
        }
        other => {
            return Err(ApiError::new(
                ApiErrorCode::Internal,
                format!("{other} is in the catalog and has no handler in this build"),
                correlation,
            ));
        }
    };

    sqlx::query(
        "UPDATE commands SET status = 'applied', result = $3 WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(command_id.as_canonical().to_string())
    .bind(&result)
    .execute(state.pool())
    .await
    .map_err(database_error)?;

    Ok(Json(CommandResponse {
        command_id: command_id.as_canonical().to_string().to_string(),
        replayed: false,
        result,
    }))
}

async fn replay(
    state: &ApiState,
    scope: &TenantScope,
    command_id: &quansio_core::CommandId,
    params_digest: &str,
    correlation: &str,
) -> Result<Json<CommandResponse>, ApiError> {
    let existing = sqlx::query_as::<_, (String, Option<Value>, Option<Value>)>(
        "SELECT params_digest, result, error FROM commands WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(command_id.as_canonical().to_string())
    .fetch_optional(state.pool())
    .await
    .map_err(database_error)?
    .ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::Internal,
            "a command conflicted but cannot be read back",
            correlation.to_string(),
        )
    })?;
    if existing.0 != params_digest {
        return Err(ApiError::new(
            ApiErrorCode::ConflictIdempotencyMismatch,
            "this command_id was already used with different parameters",
            correlation,
        ));
    }
    Ok(Json(CommandResponse {
        command_id: command_id.as_canonical().to_string().to_string(),
        replayed: true,
        result: existing.1.or(existing.2).unwrap_or(Value::Null),
    }))
}

async fn cancel_run(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .cancel(&run_id, generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok(run_json(&run))
}

async fn pause_run(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .suspend(&run_id, generation, "api")
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok(run_json(&run))
}

async fn resume_run(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .resume(&run_id, generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok(run_json(&run))
}

async fn create_thread(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let workspace_id = required_str(params, "workspace_id", correlation)?;
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("direct");
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    let store = ConversationStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let thread = store
        .create_thread(workspace_id, kind, title)
        .await
        .map_err(|error| conversation_error(error, correlation))?;
    Ok(json!({
        "thread_id": thread.id,
        "workspace_id": thread.workspace_id,
        "kind": thread.kind,
    }))
}

async fn post_message(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    user_id: Option<&str>,
    correlation: &str,
) -> Result<Value, ApiError> {
    let workspace_id = required_str(params, "workspace_id", correlation)?;
    let content = required_str(params, "content", correlation)?;
    let thread_id = params.get("thread_id").and_then(Value::as_str);
    let author = user_id
        .or_else(|| params.get("author_id").and_then(Value::as_str))
        .unwrap_or("usr_api");
    let attachments = match params.get("attachments") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    ApiError::new(
                        ApiErrorCode::ValidationSchema,
                        "attachments must be artifact ids",
                        correlation,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(ApiError::new(
                ApiErrorCode::ValidationSchema,
                "attachments must be an array of artifact ids",
                correlation,
            ))
        }
    };
    let store = ConversationStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let posted = store
        .post_message_with_attachments(workspace_id, thread_id, author, content, &attachments)
        .await
        .map_err(|error| conversation_error(error, correlation))?;

    let work_node = load_work_node(state, &identity.tenant_id, workspace_id, correlation).await?;
    let agent_thread =
        ensure_teammate(state, identity, workspace_id, &work_node, correlation).await?;
    let mut new_run = NewRun::new(
        workspace_id.to_string(),
        work_node,
        agent_thread,
        RunTriggerKind::Message,
    );
    new_run.trigger_ref = Some(posted.message.id.clone());
    if let Some(target) = load_target(state, &identity.tenant_id, workspace_id).await? {
        new_run.execution_target_id = Some(target);
    }
    let engine =
        chat_engine(state, identity.clone()).map_err(|error| internal(error, correlation))?;
    let run = engine
        .create_run(new_run)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    seed_projection(state, identity, workspace_id, &run)
        .await
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .enqueue(&run.id, run.generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let run = engine
        .start(&run.id, run.generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let outcome = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Message).with_reference(posted.message.id.clone()),
        )
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let reply = post_agent_reply(
        &store,
        &posted.thread.id,
        &agent_thread.to_string(),
        &run.id.to_string(),
        &outcome,
        correlation,
    )
    .await?;
    Ok(json!({
        "thread_id": posted.thread.id,
        "message_id": posted.message.id,
        "run_id": run.id.to_string(),
        "run_status": run.status.as_db_str(),
        "turn": turn_json(&outcome),
        "reply_message_id": reply,
    }))
}

/// The assistant text of a completed turn becomes the agent's durable message in the thread.
async fn post_agent_reply(
    store: &ConversationStore,
    thread_id: &str,
    agent_id: &str,
    run_id: &str,
    outcome: &TurnOutcome,
    correlation: &str,
) -> Result<Option<String>, ApiError> {
    let TurnOutcome::Completed {
        turn_id,
        assistant_text: Some(text),
    } = outcome
    else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let message = store
        .post_agent_message(thread_id, agent_id, text, run_id, turn_id)
        .await
        .map_err(|error| conversation_error(error, correlation))?;
    Ok(Some(message.id))
}

/// `AnswerQuestion {question_id, value}`: record the answer, release the parked run and
/// run its next turn (RUN-011 resume) so the reply follows the answer.
async fn answer_question(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    user_id: Option<&str>,
    correlation: &str,
) -> Result<Value, ApiError> {
    let question_id = required_str(params, "question_id", correlation)?;
    let value = params.get("value").cloned().ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "AnswerQuestion needs a value",
            correlation,
        )
    })?;
    let answering_user = user_id
        .or_else(|| params.get("user_id").and_then(Value::as_str))
        .unwrap_or("usr_api")
        .to_string();
    let questions = QuestionService::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let answered = questions
        .answer(
            question_id,
            QuestionAnswer {
                user_id: answering_user,
                value,
            },
        )
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let mut result = json!({
        "question_id": answered.question.id,
        "run_id": answered.question.run_id,
        "status": answered.question.status.as_str(),
        "resumed_run_id": answered.resumed_run_id,
    });
    let Some(resumed) = answered.resumed_run_id.as_deref() else {
        return Ok(result);
    };
    let run_id = CanonicalId::parse_typed(resumed, Prefix::Run)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let engine =
        chat_engine(state, identity.clone()).map_err(|error| internal(error, correlation))?;
    let run = engine
        .store()
        .load_run(&run_id)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let outcome = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Wake).with_reference(question_id.to_string()),
        )
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    result["turn"] = turn_json(&outcome);
    if let Some(thread_id) = thread_of_run(state, &identity.tenant_id, resumed).await? {
        let store = ConversationStore::new(state.pool().clone(), identity.clone())
            .map_err(|error| internal(error, correlation))?;
        result["reply_message_id"] = json!(
            post_agent_reply(
                &store,
                &thread_id,
                &run.agent_thread_id.to_string(),
                resumed,
                &outcome,
                correlation,
            )
            .await?
        );
        result["thread_id"] = json!(thread_id);
    }
    Ok(result)
}

/// `CancelTurn {run_id, generation?}`: cancel the run the turn belongs to and withdraw its
/// open questions; the durable `run.cancelled` event tells every client the turn stopped.
async fn cancel_turn(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .cancel(&run_id, generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let questions = QuestionService::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let cancelled = questions
        .cancel_for_run(&run_id.to_string())
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let mut result = run_json(&run);
    result["cancelled_questions"] = json!(cancelled);
    Ok(result)
}

/// The conversation thread a run was triggered from, when its trigger was a message.
async fn thread_of_run(
    state: &ApiState,
    tenant_id: &str,
    run_id: &str,
) -> Result<Option<String>, ApiError> {
    sqlx::query_scalar(
        "SELECT m.thread_id FROM runs r JOIN messages m ON m.id = r.trigger_ref AND m.tenant_id = r.tenant_id \
         WHERE r.tenant_id = $1 AND r.id = $2",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(state.pool())
    .await
    .map_err(database_error)
}

fn chat_engine(state: &ApiState, identity: RuntimeIdentity) -> Result<RuntimeEngine, RuntimeError> {
    let delegation: Arc<dyn crate::runtime::state_machine::DelegationPort> = Arc::new(
        AgentDelegationPort::with_structural_check(state.pool().clone(), identity.clone())?,
    );
    let projections = StoredProjectionProvider::new(state.pool().clone(), identity.clone())?;
    let dispatcher = ToolDispatchService::new(
        state.pool().clone(),
        identity.clone(),
        Arc::new(projections),
        state.roles(),
        state.host(),
        Arc::clone(&delegation),
    )?;
    let questions = QuestionService::new(state.pool().clone(), identity.clone())?;
    let live: Arc<dyn crate::runtime::state_machine::LiveFramePort> = state.live().clone();
    Ok(RuntimeEngine::new(state.pool().clone(), identity)?
        .with_model_source(state.model())
        .with_tool_dispatch(Arc::new(dispatcher))
        .with_delegation(delegation)
        .with_question_port(Arc::new(questions))
        .with_live_frames(live))
}

async fn ensure_teammate(
    state: &ApiState,
    identity: &RuntimeIdentity,
    workspace_id: &str,
    work_node: &CanonicalId,
    correlation: &str,
) -> Result<CanonicalId, ApiError> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM agent_threads WHERE tenant_id = $1 AND workspace_id = $2 \
         AND agent_kind = 'teammate' ORDER BY created_at LIMIT 1",
    )
    .bind(&identity.tenant_id)
    .bind(workspace_id)
    .fetch_optional(state.pool())
    .await
    .map_err(database_error)?;
    if let Some(id) = existing {
        return CanonicalId::parse_typed(&id, Prefix::AgentThread).map_err(|error| {
            ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation)
        });
    }
    let agents = AgentStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let created = agents
        .create_thread(
            NewAgentThread::new(workspace_id.to_string(), AgentKind::Teammate)
                .with_work_node(*work_node),
        )
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok(created.id)
}

async fn load_work_node(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
    correlation: &str,
) -> Result<CanonicalId, ApiError> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM work_nodes WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .fetch_optional(state.pool())
    .await
    .map_err(database_error)?;
    let id = id.ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationBounds,
            "the workspace has no work node",
            correlation,
        )
    })?;
    CanonicalId::parse_typed(&id, Prefix::WorkNode)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))
}

async fn load_target(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
) -> Result<Option<String>, ApiError> {
    sqlx::query_scalar(
        "SELECT id FROM execution_targets WHERE tenant_id = $1 AND workspace_id = $2 \
         ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .fetch_optional(state.pool())
    .await
    .map_err(database_error)
}

async fn seed_projection(
    state: &ApiState,
    identity: &RuntimeIdentity,
    workspace_id: &str,
    run: &Run,
) -> Result<(), String> {
    let grants: Vec<Grant> = [("read.internal", "fs", "**")]
        .into_iter()
        .map(|(class, kind, selector)| {
            Grant::from_json(&json!({
                "effect_class": class,
                "resource": { "kind": kind, "selector": selector },
                "constraints": {},
            }))
            .expect("grant")
        })
        .collect();
    let mut generator = UlidGenerator::new();
    let projection = CapabilityProjection::from_parts(
        CanonicalId::generate(Prefix::CapabilityProjection, &mut generator),
        ProjectionSubject::new(SubjectKind::Run, run.id.to_string()),
        Vec::new(),
        grants,
        Utc::now(),
        Some(Utc::now() + chrono::Duration::hours(1)),
    )
    .map_err(|error| error.to_string())?;
    let row = ProjectionRow::from_projection(
        &projection,
        identity.tenant_id.clone(),
        Some(workspace_id.to_string()),
    );
    let store = EventStore::new(state.pool().clone());
    let actor = identity.actor.clone();
    let correlation = identity.correlation_id;
    store
        .commit_mutation_tx(&identity.tenant_id, move |tx, batch| {
            Box::pin(async move {
                let mut query = sqlx::query(ProjectionRow::INSERT_SQL);
                for argument in row.arguments() {
                    query = match argument {
                        ProjectionArgument::Text(value) => query.bind(value),
                        ProjectionArgument::OptionalText(value) => query.bind(value),
                        ProjectionArgument::Json(value) => query.bind(value),
                        ProjectionArgument::Timestamptz(value) => query.bind(value),
                        ProjectionArgument::OptionalTimestamptz(value) => query.bind(value),
                    };
                }
                query.execute(&mut **tx).await?;
                stage_projected_event(batch, &row, correlation, actor);
                Ok(())
            })
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_ref(params: &Value, correlation: &str) -> Result<(CanonicalId, Generation), ApiError> {
    let run_id =
        CanonicalId::parse_typed(required_str(params, "run_id", correlation)?, Prefix::Run)
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    error.to_string(),
                    correlation,
                )
            })?;
    let generation = Generation::new(
        params
            .get("generation")
            .and_then(Value::as_u64)
            .unwrap_or(Generation::INITIAL.get()),
    )
    .map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationBounds,
            error.to_string(),
            correlation,
        )
    })?;
    Ok((run_id, generation))
}

fn required_str<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("{key} is required"),
            correlation,
        )
    })
}

fn run_json(run: &Run) -> Value {
    json!({ "run_id": run.id.to_string(), "status": run.status.as_db_str() })
}

fn turn_json(outcome: &TurnOutcome) -> Value {
    match outcome {
        TurnOutcome::Completed {
            turn_id,
            assistant_text,
        } => json!({
            "kind": "completed",
            "turn_id": turn_id,
            "assistant_text": assistant_text,
        }),
        TurnOutcome::Succeeded {
            run_id,
            turn_id,
            evidence_ids,
        } => json!({
            "kind": "succeeded",
            "run_id": run_id,
            "turn_id": turn_id,
            "evidence_ids": evidence_ids,
        }),
        TurnOutcome::Parked {
            run_id,
            turn_id,
            state,
            wait_key,
        } => json!({
            "kind": "parked",
            "run_id": run_id,
            "turn_id": turn_id,
            "state": state.as_db_str(),
            "wait_key": wait_key,
        }),
        TurnOutcome::BudgetExhausted {
            turn_id,
            steps_used,
            max_steps,
        } => json!({
            "kind": "budget_exhausted",
            "turn_id": turn_id,
            "steps_used": steps_used,
            "max_steps": max_steps,
        }),
        other => json!({ "kind": "other", "debug": format!("{other:?}") }),
    }
}

fn database_error(error: sqlx::Error) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "database")
}

fn internal(error: impl std::fmt::Display, correlation: &str) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation)
}

fn conversation_error(
    error: crate::control::conversation::ConversationError,
    correlation: &str,
) -> ApiError {
    let code = if matches!(
        error,
        crate::control::conversation::ConversationError::NotFound(_)
    ) {
        ApiErrorCode::NotFound
    } else {
        ApiErrorCode::ValidationBounds
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn runtime_error(error: RuntimeError, correlation: &str) -> ApiError {
    // The runtime's codes are the catalog's spellings (`RuntimeError::code`); a code the
    // catalog does not know is an internal fault, not a client error.
    let code = match error.code() {
        "RUNTIME_ILLEGAL_TRANSITION" => ApiErrorCode::RuntimeIllegalTransition,
        "FENCED_STALE_GENERATION" => ApiErrorCode::FencedStaleGeneration,
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "CAPABILITY_DENIED" => ApiErrorCode::CapabilityDenied,
        "RUNTIME_LEASE_LOST" => ApiErrorCode::LeaseLost,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

/// §14 identity commands: `CreateWorkspace`, `UpdateWorkspace`, `UpdateUserSettings` (APP-002).
///
/// Every one of them needs an acting user, so a trusted-header request without
/// `x-quansio-user` and a bearer-less request are both refused with `AUTH_REQUIRED`.
async fn identity_command(
    state: &ApiState,
    scope: &TenantScope,
    command: &str,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let Some(user_id) = scope.user_id.as_deref() else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            format!("{command} needs an acting user"),
            correlation,
        ));
    };
    let identity = state.auth().identity();
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    let schema =
        |message: String| ApiError::new(ApiErrorCode::ValidationSchema, message, correlation);
    let outcome = match command {
        "CreateWorkspace" => {
            let name =
                text("name").ok_or_else(|| schema("CreateWorkspace needs `name`".to_string()))?;
            let workspace_id = identity
                .create_workspace(&scope.tenant_id, user_id, name)
                .await
                .map_err(|error| schema(error.to_string()))?;
            json!({ "workspace_id": workspace_id, "tenant_id": scope.tenant_id, "name": name.trim() })
        }
        "UpdateWorkspace" => {
            let workspace_id = text("workspace_id")
                .ok_or_else(|| schema("UpdateWorkspace needs `workspace_id`".to_string()))?;
            let change = identity
                .update_workspace(
                    &scope.tenant_id,
                    user_id,
                    workspace_id,
                    text("name"),
                    params.get("settings"),
                )
                .await
                .map_err(|error| schema(error.to_string()))?;
            json!({
                "workspace_id": workspace_id,
                "effective_settings": change.effective,
                "effects": change.effects.iter().map(|effect| effect.to_json()).collect::<Vec<_>>(),
            })
        }
        _ => {
            let workspace_id = text("workspace_id").ok_or_else(|| {
                schema(
                    "UpdateUserSettings needs `workspace_id` to report effective settings"
                        .to_string(),
                )
            })?;
            let settings = params
                .get("settings")
                .ok_or_else(|| schema("UpdateUserSettings needs `settings`".to_string()))?;
            let change = identity
                .update_user_settings(&scope.tenant_id, workspace_id, user_id, settings)
                .await
                .map_err(|error| schema(error.to_string()))?;
            json!({
                "user_id": user_id,
                "workspace_id": workspace_id,
                "effective_settings": change.effective,
                "effects": change.effects.iter().map(|effect| effect.to_json()).collect::<Vec<_>>(),
            })
        }
    };
    Ok(outcome)
}
