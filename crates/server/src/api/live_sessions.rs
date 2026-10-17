//! Live browser, computer and terminal sessions (APP-007; DOMAIN.md §8.4–§8.6, §14).
//!
//! `GET /v1/targets?workspace_id=` lists a workspace's execution targets with the browser
//! sessions, terminal sessions and native-computer control row each one carries.
//! `GET /v1/browser-sessions/{id}` is the live view of one session: the row the agent and
//! the user share, the run it belongs to, the terminal sessions on the same target with
//! their durable cursors, and `automation` — whether the agent is paused, why, and whether
//! its input is fenced — computed from the row and the run's ProtocolState, never guessed by
//! a client.
//!
//! `RequestTakeover` and `Handback` are the two commands (§8.4 for a `session_id`, §8.6 for
//! a `target_id`). Each is one transaction: the machine authority moves the row (the same
//! row — a takeover never creates a second session), the runtime parks or releases the
//! owning run through its ProtocolState (`WAITING_TAKEOVER` ⇄ `RUNNING`), and the
//! `browser.control_changed` (or `target.control_changed`) event and the AuditEntry are
//! written with it. A handback of a session the user does not hold is refused, so it cannot
//! resume what policy paused.

use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use quansio_core::{CanonicalId, Prefix};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use quansio_machine::computer_use::store::{
    ComputerControl, ComputerControlError, ComputerControlStore,
};
use quansio_machine::computer_use::MachineHolder;
use quansio_machine::control::browser::{
    BrowserSession, BrowserSessionError, BrowserSessionStore, Decision, DecisionActor,
    DecisionReceipt, SessionStatus,
};
use quansio_machine::control::terminal::{TerminalSession, TerminalStore};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Postgres, Row, Transaction};

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::audit::{append_in, AuditDraft};
use crate::control::schema;
use crate::policy::rbac::ActionFamily;
use crate::runtime::protocol_state::{BrowserControlHolder, ProtocolStateStore};
use crate::runtime::state_machine::{Run, RunStatus, RuntimeIdentity, RuntimeStore};

/// The module that owns this mutation, named in a rejected transaction.
const LIVE_OWNER: &str = "crates/server/src/api/live_sessions";

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

fn browser_error(error: BrowserSessionError, correlation: &str) -> ApiError {
    let code = match &error {
        BrowserSessionError::NotFound(_) | BrowserSessionError::TargetNotFound(_) => {
            ApiErrorCode::NotFound
        }
        BrowserSessionError::SessionIdInvalid(_)
        | BrowserSessionError::DecisionNotGrantable(_)
        | BrowserSessionError::GuardInvalid { .. } => ApiErrorCode::ValidationSchema,
        BrowserSessionError::NotAllowed { .. }
        | BrowserSessionError::NotHeld { .. }
        | BrowserSessionError::DecisionSubjectNotFound(_) => ApiErrorCode::ConflictState,
        BrowserSessionError::UserDecisionRequired { .. } => ApiErrorCode::ScopeForbidden,
        BrowserSessionError::Database(_) | BrowserSessionError::Corrupt { .. } => {
            ApiErrorCode::Internal
        }
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn computer_error(error: ComputerControlError, correlation: &str) -> ApiError {
    let code = match &error {
        ComputerControlError::NotFound(_) | ComputerControlError::TargetNotFound(_) => {
            ApiErrorCode::NotFound
        }
        ComputerControlError::ToolCallIdInvalid(_) => ApiErrorCode::ValidationSchema,
        ComputerControlError::StaleGeneration { .. } => ApiErrorCode::FencedStaleGeneration,
        ComputerControlError::Held(_)
        | ComputerControlError::TakeoverPending
        | ComputerControlError::ActionInFlight(_)
        | ComputerControlError::ActionMismatch { .. }
        | ComputerControlError::TakeoverNotPending => ApiErrorCode::ConflictState,
        ComputerControlError::Database(_) | ComputerControlError::Corrupt(_) => {
            ApiErrorCode::Internal
        }
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn runtime_error(
    error: crate::runtime::state_machine::RuntimeError,
    correlation: &str,
) -> ApiError {
    let code = match error.code() {
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "FENCED_STALE_GENERATION" => ApiErrorCode::FencedStaleGeneration,
        "RUNTIME_ILLEGAL_TRANSITION" => ApiErrorCode::RuntimeIllegalTransition,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

pub(super) fn now_second() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ------------------------------------------------------------------ json shapes

/// The browser session row as the projection and the command result spell it.
#[must_use]
pub fn session_json(session: &BrowserSession) -> Value {
    json!({
        "id": session.id,
        "target_id": session.target_id,
        "run_id": session.run_id,
        "profile_ref": session.profile_ref,
        "control_holder": session.control_holder.as_str(),
        "control_since": session.control_since,
        "current_url": session.current_url,
        "tabs": session.tabs.iter().map(|tab| json!({
            "target_id": tab.target_id,
            "url": tab.url,
            "title": tab.title,
            "document_revision": tab.document_revision,
        })).collect::<Vec<_>>(),
        "screencast": session.screencast,
        "checkpoint_id": session.checkpoint_id,
        "status": session.status.as_str(),
        "agent_may_drive": session.agent_may_drive(),
        // What the hardened driver last reported (ADP-001): a dialog or a certificate error waiting
        // for an answer, downloads, and browser actions in doubt. Page-derived text in it is
        // UNTRUSTED_EXTERNAL; the surface renders it as data.
        "guard": session.guard.to_json(),
    })
}

fn terminal_json(session: &TerminalSession) -> Value {
    json!({
        "id": session.id,
        "target_id": session.target_id,
        "run_id": session.run_id,
        "pty_ref": session.pty_ref,
        "cursor": session.cursor,
        "status": session.status.as_str(),
        "last_command_id": session.last_command_id,
    })
}

fn control_json(control: &ComputerControl) -> Value {
    json!({
        "target_id": control.target_id,
        "run_id": control.run_id,
        "holder": control.holder.as_str(),
        "generation": control.generation,
        "takeover_pending": control.takeover_pending,
        "active_tool_call_id": control.active_tool_call_id,
        "control_since": control.control_since,
        "agent_may_input": control.agent_may_input(),
    })
}

fn run_json(run: &Run) -> Value {
    json!({
        "id": run.id.to_string(),
        "status": run.status.as_db_str(),
        "generation": run.generation.get(),
        "workspace_id": run.workspace_id,
        "execution_target_id": run.execution_target_id,
    })
}

/// `automation`: what the surface shows about the agent, from the row and the run.
fn automation_json(
    session: &BrowserSession,
    run_status: Option<RunStatus>,
    state_holder: Option<BrowserControlHolder>,
) -> Value {
    let reason = match session.status {
        SessionStatus::Active => None,
        SessionStatus::PausedTakeover => Some("user_control"),
        SessionStatus::PausedPolicy => Some("policy"),
        SessionStatus::Closed => Some("closed"),
    };
    // The fence is the row's (the worker reads it) and the ProtocolState's (the runtime
    // reads it); either one alone fences, so the surface reports the union.
    let fenced = !session.agent_may_drive()
        || matches!(state_holder, Some(BrowserControlHolder::User))
        || matches!(run_status, Some(RunStatus::WaitingTakeover));
    json!({
        "paused": reason.is_some(),
        "reason": reason,
        "agent_input_fenced": fenced,
        "run_status": run_status.map(RunStatus::as_db_str),
        "protocol_state_holder": state_holder.map(|holder| match holder {
            BrowserControlHolder::Agent => "agent",
            BrowserControlHolder::User => "user",
            BrowserControlHolder::None => "none",
        }),
    })
}

// ------------------------------------------------------------------ projections

/// `?workspace_id=`.
#[derive(Debug, Deserialize)]
pub struct TargetsQuery {
    workspace_id: String,
}

struct TargetRow {
    id: String,
    workspace_id: String,
    target_class: String,
    substrate: String,
    status: String,
    generation: i64,
}

async fn load_target(
    conn: &mut PgConnection,
    tenant_id: &str,
    target_id: &str,
) -> Result<Option<TargetRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, workspace_id, target_class, substrate, status, generation \
         FROM execution_targets WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(target_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| TargetRow {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        target_class: row.get("target_class"),
        substrate: row.get("substrate"),
        status: row.get("status"),
        generation: row.get("generation"),
    }))
}

fn target_json(target: &TargetRow) -> Value {
    json!({
        "id": target.id,
        "workspace_id": target.workspace_id,
        "target_class": target.target_class,
        "substrate": target.substrate,
        "status": target.status,
        "generation": target.generation,
    })
}

/// `GET /v1/targets?workspace_id=`: the workspace's targets with their live sessions.
pub async fn targets(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<TargetsQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    Authorization::check(
        state.pool(),
        &scope,
        Some(&query.workspace_id),
        ActionFamily::Read,
        "projection",
    )
    .await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(
        "SELECT id, workspace_id, target_class, substrate, status, generation \
         FROM execution_targets WHERE tenant_id = $1 AND workspace_id = $2 \
         ORDER BY created_at, id",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let target = TargetRow {
            id: row.get("id"),
            workspace_id: row.get("workspace_id"),
            target_class: row.get("target_class"),
            substrate: row.get("substrate"),
            status: row.get("status"),
            generation: row.get("generation"),
        };
        let browser = BrowserSessionStore::list_for_target(&mut tx, &scope.tenant_id, &target.id)
            .await
            .map_err(projection_error)?;
        let terminals = TerminalStore::list_for_target(&mut tx, &scope.tenant_id, &target.id)
            .await
            .map_err(projection_error)?;
        let control = match ComputerControlStore::load(&mut tx, &scope.tenant_id, &target.id).await
        {
            Ok(control) => Some(control),
            Err(ComputerControlError::NotFound(_)) => None,
            Err(error) => return Err(projection_error(error)),
        };
        let mut item = target_json(&target);
        item["browser_sessions"] = Value::Array(browser.iter().map(session_json).collect());
        item["terminal_sessions"] = Value::Array(terminals.iter().map(terminal_json).collect());
        item["computer_control"] = control.as_ref().map_or(Value::Null, control_json);
        items.push(item);
    }
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({
        "workspace_id": query.workspace_id,
        "items": items,
        "cursor": Value::Null,
    })))
}

/// `GET /v1/browser-sessions/{id}`: the live view of one session.
pub async fn browser_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let session = BrowserSessionStore::load(&mut tx, &scope.tenant_id, &id)
        .await
        .map_err(|error| browser_error(error, "projection"))?;
    let target = load_target(&mut tx, &scope.tenant_id, &session.target_id)
        .await
        .map_err(projection_error)?
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::NotFound,
                format!(
                    "execution target {} does not exist for this tenant",
                    session.target_id
                ),
                "projection",
            )
        })?;
    Authorization::check(
        state.pool(),
        &scope,
        Some(&target.workspace_id),
        ActionFamily::Read,
        "projection",
    )
    .await?;
    let terminals = TerminalStore::list_for_target(&mut tx, &scope.tenant_id, &target.id)
        .await
        .map_err(projection_error)?;
    let control = match ComputerControlStore::load(&mut tx, &scope.tenant_id, &target.id).await {
        Ok(control) => Some(control),
        Err(ComputerControlError::NotFound(_)) => None,
        Err(error) => return Err(projection_error(error)),
    };
    let (run, state_holder) = match &session.run_id {
        Some(run_id) => {
            let run = sqlx::query(
                "SELECT id, status, generation FROM runs WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&scope.tenant_id)
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(projection_error)?;
            let state = ProtocolStateStore::load(&mut tx, &scope.tenant_id, run_id)
                .await
                .map_err(projection_error)?;
            let holder = state.as_ref().and_then(|state| {
                state
                    .browser_control
                    .as_ref()
                    .filter(|control| control.session_id == session.id)
                    .map(|control| control.holder.clone())
            });
            let run = run.map(|row| {
                let status: String = row.get("status");
                json!({
                    "id": row.get::<String, _>("id"),
                    "status": status,
                    "generation": row.get::<i64, _>("generation"),
                    "browser_control": state.as_ref().and_then(|state| state.browser_control.as_ref()).map(|control| json!({
                        "session_id": control.session_id,
                        "holder": match control.holder {
                            BrowserControlHolder::Agent => "agent",
                            BrowserControlHolder::User => "user",
                            BrowserControlHolder::None => "none",
                        },
                        "since": control.since,
                    })),
                    "terminal_sessions": state.as_ref().map(|state| state.terminal_sessions.iter().map(|entry| json!({
                        "id": entry.id,
                        "cursor": entry.cursor,
                    })).collect::<Vec<_>>()).unwrap_or_default(),
                })
            });
            (run, holder)
        }
        None => (None, None),
    };
    tx.commit().await.map_err(projection_error)?;
    let run_status = run
        .as_ref()
        .and_then(|run| run.get("status").and_then(Value::as_str))
        .and_then(|status| RunStatus::from_db_str(status).ok());
    Ok(Json(json!({
        "session": session_json(&session),
        "target": target_json(&target),
        "run": run,
        "terminal_sessions": terminals.iter().map(terminal_json).collect::<Vec<_>>(),
        "computer_control": control.as_ref().map_or(Value::Null, control_json),
        "automation": automation_json(&session, run_status, state_holder),
    })))
}

// ------------------------------------------------------------------ commands

/// What a takeover or handback names: a browser session or a native-computer target.
#[derive(Debug)]
enum Subject {
    Browser(String),
    Computer(String),
}

fn subject(params: &Value, correlation: &str) -> Result<Subject, ApiError> {
    let session_id = params.get("session_id").and_then(Value::as_str);
    let target_id = params.get("target_id").and_then(Value::as_str);
    match (session_id, target_id) {
        (Some(session_id), None) => {
            CanonicalId::parse_typed(session_id, Prefix::BrowserSession).map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!("session_id: {error}"),
                    correlation,
                )
            })?;
            Ok(Subject::Browser(session_id.to_string()))
        }
        (None, Some(target_id)) => {
            CanonicalId::parse_typed(target_id, Prefix::ExecutionTarget).map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!("target_id: {error}"),
                    correlation,
                )
            })?;
            Ok(Subject::Computer(target_id.to_string()))
        }
        _ => Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            "exactly one of session_id (bsn_…) or target_id (tgt_…) is required",
            correlation,
        )),
    }
}

/// Run one mutation with its events in one transaction, keeping the typed refusal.
pub(super) async fn commit<T, F>(
    events: &EventStore,
    tenant_id: &str,
    mutation: F,
) -> Result<T, ApiError>
where
    T: Send,
    F: Send
        + 'static
        + for<'a> FnOnce(
            &'a mut Transaction<'static, Postgres>,
            &'a mut EventBatch,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, ApiError>> + Send + 'a>,
        >,
{
    let rejection: Arc<Mutex<Option<ApiError>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&rejection);
    let result = events
        .commit_mutation_tx(tenant_id, move |tx, batch| {
            Box::pin(async move {
                match mutation(tx, batch).await {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        let message = error.message.clone();
                        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                        Err(EventError::MutationRejected {
                            owner: LIVE_OWNER,
                            message,
                        })
                    }
                }
            })
        })
        .await;
    match result {
        Ok(value) => Ok(value),
        Err(error) => match rejection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            Some(error) => Err(error),
            None => Err(ApiError::new(
                ApiErrorCode::Internal,
                error.to_string(),
                "live",
            )),
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn stage(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    actor: &Actor,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
    workspace_id: &str,
    payload: Value,
) -> Result<(), ApiError> {
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND aggregate_type = $2 AND aggregate_id = $3",
    )
    .bind(&identity.tenant_id)
    .bind(aggregate_type)
    .bind(aggregate_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "live"))?;
    let event_type = EventType::parse(event_type)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "live"))?;
    let mut draft = EventDraft::new(
        aggregate_type,
        aggregate_id,
        u64::try_from(existing).unwrap_or(0).saturating_add(1),
        event_type,
        identity.correlation_id,
        actor.clone(),
    )
    .with_workspace(workspace_id)
    .with_payload(payload);
    if let Some(command_id) = identity.command_id {
        draft = draft.with_command_id(command_id);
    }
    batch.emit(draft);
    Ok(())
}

pub(super) fn actor_of(identity: &RuntimeIdentity, user_id: Option<&str>) -> Actor {
    user_id.map_or_else(|| identity.actor.clone(), Actor::user)
}

fn actor_json(actor: &Actor) -> Value {
    json!({ "kind": actor.kind.as_str(), "id": actor.id })
}

async fn target_workspace(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    target_id: &str,
    correlation: &str,
) -> Result<String, ApiError> {
    load_target(tx, tenant_id, target_id)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?
        .map(|target| target.workspace_id)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::NotFound,
                format!("execution target {target_id} does not exist for this tenant"),
                correlation,
            )
        })
}

/// The workspace a takeover acts in, for the role check that precedes the mutation.
async fn workspace_of(
    state: &ApiState,
    tenant_id: &str,
    subject: &Subject,
    correlation: &str,
) -> Result<String, ApiError> {
    let mut tx =
        state.pool().begin().await.map_err(|error| {
            ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation)
        })?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let target_id = match subject {
        Subject::Browser(session_id) => {
            BrowserSessionStore::load(&mut tx, tenant_id, session_id)
                .await
                .map_err(|error| browser_error(error, correlation))?
                .target_id
        }
        Subject::Computer(target_id) => target_id.clone(),
    };
    target_workspace(&mut tx, tenant_id, &target_id, correlation).await
}

/// Tell the worker that runs a browser session who holds it now. Best effort: the database has the truth and the server
/// repeats it before the next agent call, so a worker that is away learns it then.
async fn relay_control(state: &ApiState, tenant_id: &str, answer: &Value, holder: &str) -> bool {
    use quansio_machine::gateway::wire::BrowserOrder;
    let (Some(hub), Some(session_id)) = (
        state.worker_hub(),
        answer.pointer("/session/id").and_then(Value::as_str),
    ) else {
        return false;
    };
    let sent = hub
        .relay(
            tenant_id,
            BrowserOrder::Control {
                session_id: session_id.to_string(),
                holder: holder.to_string(),
                at: now_second(),
            },
        )
        .await
        == crate::worker::Relay::Sent;
    // A person who holds the session needs to see it; when the agent has it back the stream stops.
    hub.relay(
        tenant_id,
        BrowserOrder::Screencast {
            session_id: session_id.to_string(),
            enabled: holder == "user",
        },
    )
    .await;
    sent
}

/// `SendBrowserInput {session_id, tab_id, kind: "click" | "text", x?, y?, text?}`: a person's click or typing in a
/// browser session they hold (DOMAIN.md §8.4.2).
///
/// Only a signed-in person; a service principal is refused. Only while a person holds the session — the agent's input is
/// fenced then, and this is the one thing that is not — and only to a worker that is attached: an input that cannot be
/// delivered is an error, not a silent drop, because the person is looking at a page they believe they are driving.
/// Individual inputs are not audited (a click per frame would drown the log); the takeover and handback that bound them
/// are.
///
/// # Errors
/// See [`ApiError`].
pub async fn send_browser_input(
    state: &ApiState,
    scope: &TenantScope,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    use quansio_machine::gateway::wire::BrowserOrder;
    let invalid = |detail: &str| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            detail.to_string(),
            correlation,
        )
    };
    if scope.principal.is_some() {
        return Err(ApiError::new(
            ApiErrorCode::ScopeForbidden,
            "SendBrowserInput is a person's act: a service principal cannot make it",
            correlation,
        ));
    }
    if user_id.filter(|id| id.starts_with("usr_")).is_none() {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "SendBrowserInput needs a signed-in person",
            correlation,
        ));
    }
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let session_id = text("session_id")
        .ok_or_else(|| invalid("session_id is required"))?
        .to_string();
    let tab_id = text("tab_id")
        .ok_or_else(|| invalid("tab_id is required"))?
        .to_string();
    let order = match text("kind") {
        Some("click") => {
            let coordinate = |key: &str| {
                params
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && (0.0..=20_000.0).contains(value))
                    .ok_or_else(|| invalid(&format!("{key} is a number from 0 to 20000")))
            };
            BrowserOrder::UserClick {
                session_id: session_id.clone(),
                tab_id,
                x: coordinate("x")?,
                y: coordinate("y")?,
            }
        }
        Some("text") => {
            let typed = text("text").ok_or_else(|| invalid("text is required"))?;
            if typed.len() > 4096 {
                return Err(invalid("text is at most 4096 bytes"));
            }
            BrowserOrder::UserText {
                session_id: session_id.clone(),
                tab_id,
                text: typed.to_string(),
            }
        }
        _ => return Err(invalid("kind is click or text")),
    };
    let workspace_id = workspace_of(
        state,
        &scope.tenant_id,
        &Subject::Browser(session_id.clone()),
        correlation,
    )
    .await?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let mut tx = state.tenant_tx(&scope.tenant_id, correlation).await?;
    let session = BrowserSessionStore::load(&mut tx, &scope.tenant_id, &session_id)
        .await
        .map_err(|error| browser_error(error, correlation))?;
    if !matches!(
        session.control_holder,
        quansio_machine::control::browser::ControlHolder::User
    ) {
        return Err(ApiError::new(
            ApiErrorCode::ConflictState,
            "input is accepted only while a person holds the session; take it over first",
            correlation,
        ));
    }
    let delivered = match state.worker_hub() {
        Some(hub) => hub.relay(&scope.tenant_id, order).await == crate::worker::Relay::Sent,
        None => false,
    };
    if !delivered {
        return Err(ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "no worker is attached for this session; the input was not delivered",
            correlation,
        ));
    }
    Ok(json!({ "delivered": true }))
}

/// Give the worker that runs the session the receipt of a user's decision. The decision is recorded either way; a worker
/// that is away does not learn it, and the response says so (`worker_relayed: false`).
async fn relay_receipt(
    state: &ApiState,
    tenant_id: &str,
    receipt: &quansio_machine::control::browser::DecisionReceipt,
    certificate: bool,
) -> bool {
    use quansio_machine::gateway::wire::BrowserOrder;
    let Some(hub) = state.worker_hub() else {
        return false;
    };
    let receipt = receipt.to_wire();
    let order = if certificate {
        BrowserOrder::Certificate { receipt }
    } else {
        BrowserOrder::Permission { receipt }
    };
    hub.relay(tenant_id, order).await == crate::worker::Relay::Sent
}

/// `RequestTakeover {session_id}` or `RequestTakeover {target_id}`.
pub async fn request_takeover(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let subject = subject(params, correlation)?;
    let workspace_id = workspace_of(state, &scope.tenant_id, &subject, correlation).await?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let events = EventStore::new(state.pool().clone());
    let runtime = RuntimeStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| runtime_error(error, correlation))?;
    let identity = identity.clone();
    let actor = actor_of(&identity, user_id);
    let correlation_owned = correlation.to_string();
    let tenant = scope.tenant_id.clone();
    let mut answer = commit(&events, &scope.tenant_id, move |tx, batch| {
        Box::pin(async move {
            let correlation = correlation_owned.as_str();
            let at = now_second();
            match subject {
                Subject::Browser(session_id) => {
                    let session =
                        BrowserSessionStore::request_takeover(tx, &tenant, &session_id, &at)
                            .await
                            .map_err(|error| browser_error(error, correlation))?;
                    let run = park(
                        &runtime,
                        tx,
                        batch,
                        session.run_id.as_deref(),
                        &session.id,
                        &at,
                        correlation,
                    )
                    .await?;
                    stage(
                        tx,
                        batch,
                        &identity,
                        &actor,
                        "browser_session",
                        &session.id,
                        "browser.control_changed",
                        &workspace_id,
                        json!({
                            "session_id": session.id,
                            "target_id": session.target_id,
                            "run_id": session.run_id,
                            "holder": session.control_holder.as_str(),
                            "status": session.status.as_str(),
                            "since": at,
                            "command": "RequestTakeover",
                        }),
                    )
                    .await?;
                    audit(
                        tx,
                        &tenant,
                        &workspace_id,
                        &actor,
                        "RequestTakeover",
                        &format!("browser_session:{}", session.id),
                        "agent input fenced; same session",
                        correlation,
                    )
                    .await?;
                    Ok(json!({
                        "session": session_json(&session),
                        "run": run.as_ref().map(run_json),
                    }))
                }
                Subject::Computer(target_id) => {
                    let current = ComputerControlStore::load(tx, &tenant, &target_id)
                        .await
                        .map_err(|error| computer_error(error, correlation))?;
                    let pending = ComputerControlStore::request_takeover(
                        tx,
                        &tenant,
                        &target_id,
                        current.generation,
                    )
                    .await
                    .map_err(|error| computer_error(error, correlation))?;
                    // §8.6: the holder changes only once the exact active ToolCall has
                    // settled; with none active the takeover completes now, otherwise it
                    // stays pending and the surface shows the drain.
                    let control = if pending.active_tool_call_id.is_none() {
                        ComputerControlStore::complete_takeover(
                            tx,
                            &tenant,
                            &target_id,
                            pending.generation,
                            &at,
                        )
                        .await
                        .map_err(|error| computer_error(error, correlation))?
                    } else {
                        pending
                    };
                    let run = park(
                        &runtime,
                        tx,
                        batch,
                        control.run_id.as_deref(),
                        &target_id,
                        &at,
                        correlation,
                    )
                    .await?;
                    stage(
                        tx,
                        batch,
                        &identity,
                        &actor,
                        "target",
                        &target_id,
                        "target.control_changed",
                        &workspace_id,
                        json!({
                            "target_id": target_id,
                            "run_id": control.run_id,
                            "holder": control.holder.as_str(),
                            "generation": control.generation,
                            "takeover_pending": control.takeover_pending,
                            "since": at,
                            "command": "RequestTakeover",
                        }),
                    )
                    .await?;
                    audit(
                        tx,
                        &tenant,
                        &workspace_id,
                        &actor,
                        "RequestTakeover",
                        &format!("target:{target_id}"),
                        if control.takeover_pending {
                            "native input fenced; draining the active call"
                        } else {
                            "native input fenced; user holds the target"
                        },
                        correlation,
                    )
                    .await?;
                    Ok(json!({
                        "computer_control": control_json(&control),
                        "run": run.as_ref().map(run_json),
                    }))
                }
            }
        })
    })
    .await?;
    if answer.get("session").is_some() {
        let relayed = relay_control(state, &scope.tenant_id, &answer, "user").await;
        answer["worker_relayed"] = json!(relayed);
    }
    Ok(answer)
}

/// `Handback {session_id}` or `Handback {target_id}`.
pub async fn handback(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let subject = subject(params, correlation)?;
    let workspace_id = workspace_of(state, &scope.tenant_id, &subject, correlation).await?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let events = EventStore::new(state.pool().clone());
    let runtime = RuntimeStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| runtime_error(error, correlation))?;
    let identity = identity.clone();
    let actor = actor_of(&identity, user_id);
    let correlation_owned = correlation.to_string();
    let tenant = scope.tenant_id.clone();
    // Native input going back to the agent ends every remote session that holds it (DOMAIN.md 8.6.2): a person cannot keep driving.
    let resumed_computer = match &subject {
        Subject::Computer(target_id) => Some(target_id.clone()),
        Subject::Browser(_) => None,
    };
    let answer = commit(&events, &scope.tenant_id, move |tx, batch| {
        Box::pin(async move {
            let correlation = correlation_owned.as_str();
            let at = now_second();
            match subject {
                Subject::Browser(session_id) => {
                    let session = BrowserSessionStore::handback(tx, &tenant, &session_id, &at)
                        .await
                        .map_err(|error| browser_error(error, correlation))?;
                    let run = release(&runtime, tx, batch, session.run_id.as_deref(), &session.id, correlation).await?;
                    stage(tx, batch, &identity, &actor, "browser_session", &session.id, "browser.control_changed", &workspace_id, json!({
                        "session_id": session.id,
                        "target_id": session.target_id,
                        "run_id": session.run_id,
                        "holder": session.control_holder.as_str(),
                        "status": session.status.as_str(),
                        "since": at,
                        "command": "Handback",
                    }))
                    .await?;
                    audit(tx, &tenant, &workspace_id, &actor, "Handback", &format!("browser_session:{}", session.id), "agent resumes; same session", correlation).await?;
                    Ok(json!({
                        "session": session_json(&session),
                        "run": run.as_ref().map(run_json),
                    }))
                }
                Subject::Computer(target_id) => {
                    let current = ComputerControlStore::load(tx, &tenant, &target_id)
                        .await
                        .map_err(|error| computer_error(error, correlation))?;
                    if !matches!(current.holder, MachineHolder::User) {
                        return Err(ApiError::new(
                            ApiErrorCode::ConflictState,
                            format!("native computer input on {target_id} is held by {}, so it cannot be handed back", current.holder.as_str()),
                            correlation,
                        ));
                    }
                    let control = ComputerControlStore::handback(tx, &tenant, &target_id, current.generation, &at)
                        .await
                        .map_err(|error| computer_error(error, correlation))?;
                    let run = release(&runtime, tx, batch, control.run_id.as_deref(), &target_id, correlation).await?;
                    stage(tx, batch, &identity, &actor, "target", &target_id, "target.control_changed", &workspace_id, json!({
                        "target_id": target_id,
                        "run_id": control.run_id,
                        "holder": control.holder.as_str(),
                        "generation": control.generation,
                        "takeover_pending": control.takeover_pending,
                        "since": at,
                        "command": "Handback",
                    }))
                    .await?;
                    audit(tx, &tenant, &workspace_id, &actor, "Handback", &format!("target:{target_id}"), "agent resumes native input", correlation).await?;
                    Ok(json!({
                        "computer_control": control_json(&control),
                        "run": run.as_ref().map(run_json),
                    }))
                }
            }
        })
    })
    .await?;
    if let Some(target_id) = resumed_computer {
        super::remote::agent_resumed(state, &scope.tenant_id, &target_id).await;
    }
    let mut answer = answer;
    if answer.get("session").is_some() {
        let relayed = relay_control(state, &scope.tenant_id, &answer, "agent").await;
        answer["worker_relayed"] = json!(relayed);
    }
    Ok(answer)
}

// ------------------------------------------------------------------ user-only browser decisions (ADP-001)

/// What a decision command shares: a person, a session in the caller's tenant, an `allow` or `deny`.
struct DecisionRequest {
    user_id: String,
    session_id: String,
    tab_id: String,
    decision: Decision,
    workspace_id: String,
}

/// Refuse everyone but a signed-in person, read the common params and check the role, before any row is touched.
///
/// A service principal is refused as `SCOPE_FORBIDDEN` however it is scoped: a certificate error or a camera grant is
/// a person's act (DOMAIN.md 8.4.1 F), and no Tool, thread, routine or model output reaches this function.
async fn decision_request(
    state: &ApiState,
    scope: &TenantScope,
    user_id: Option<&str>,
    params: &Value,
    command: &str,
    correlation: &str,
) -> Result<DecisionRequest, ApiError> {
    if scope.principal.is_some() {
        return Err(ApiError::new(
            ApiErrorCode::ScopeForbidden,
            format!("{command} is a person's decision: a service principal cannot make it"),
            correlation,
        ));
    }
    let Some(user_id) = user_id.filter(|id| id.starts_with("usr_")) else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            format!("{command} needs a signed-in person"),
            correlation,
        ));
    };
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!("{key} is required"),
                    correlation,
                )
            })
    };
    let session_id = text("session_id")?;
    let tab_id = text("tab_id")?;
    let decision = match text("decision")?.as_str() {
        "allow" => Decision::Allow,
        "deny" => Decision::Deny,
        _ => {
            return Err(ApiError::new(
                ApiErrorCode::ValidationSchema,
                "decision is allow or deny",
                correlation,
            ))
        }
    };
    let workspace_id = workspace_of(
        state,
        &scope.tenant_id,
        &Subject::Browser(session_id.clone()),
        correlation,
    )
    .await?;
    // Letting a page past a certificate error, or open a camera, is the kind of act a person with the right to
    // approve effects in the workspace makes; a viewer cannot.
    Authorization::check(
        state.pool(),
        scope,
        Some(&workspace_id),
        ActionFamily::ApproveEffect,
        correlation,
    )
    .await?;
    Ok(DecisionRequest {
        user_id: user_id.to_string(),
        session_id,
        tab_id,
        decision,
        workspace_id,
    })
}

fn receipt_json(receipt: &DecisionReceipt) -> Value {
    json!({
        "session_id": receipt.session_id(),
        "tab_id": receipt.tab_id(),
        "kind": receipt.kind().as_str(),
        "subject": receipt.subject(),
        "decision": receipt.decision().as_str(),
        "decided_by": receipt.decided_by(),
        "decided_at": receipt.decided_at(),
    })
}

/// `ResolveBrowserCertificate {session_id, tab_id, certificate_id, decision}`: a person lets the tab continue past a
/// certificate error (`allow`) or cancels it (`deny`). The receipt is what the worker acts on.
pub async fn resolve_browser_certificate(
    state: &ApiState,
    scope: &TenantScope,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let request = decision_request(
        state,
        scope,
        user_id,
        params,
        "ResolveBrowserCertificate",
        correlation,
    )
    .await?;
    let certificate_id = params
        .get("certificate_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "certificate_id is required",
                correlation,
            )
        })?;
    let at = now_second();
    let mut tx = state.tenant_tx(&scope.tenant_id, correlation).await?;
    let receipt = BrowserSessionStore::decide_certificate(
        &mut tx,
        &scope.tenant_id,
        &request.session_id,
        &request.tab_id,
        certificate_id,
        request.decision,
        &DecisionActor::User {
            user_id: request.user_id.clone(),
        },
        &at,
    )
    .await
    .map_err(|error| browser_error(error, correlation))?;
    audit(
        &mut tx,
        &scope.tenant_id,
        &request.workspace_id,
        &Actor::user(&request.user_id),
        "ResolveBrowserCertificate",
        &format!("browser_session:{}", request.session_id),
        &format!(
            "certificate {certificate_id} on tab {}: {}",
            request.tab_id,
            request.decision.as_str()
        ),
        correlation,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let relayed = relay_receipt(state, &scope.tenant_id, &receipt, true).await;
    Ok(json!({ "receipt": receipt_json(&receipt), "worker_relayed": relayed }))
}

/// `DecideBrowserPermission {session_id, tab_id, origin, permission, decision}`: a person grants or denies camera or
/// microphone for one origin. Nothing else can be granted.
pub async fn decide_browser_permission(
    state: &ApiState,
    scope: &TenantScope,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let request = decision_request(
        state,
        scope,
        user_id,
        params,
        "DecideBrowserPermission",
        correlation,
    )
    .await?;
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!("{key} is required"),
                    correlation,
                )
            })
    };
    let origin = text("origin")?;
    let permission = text("permission")?;
    let at = now_second();
    let mut tx = state.tenant_tx(&scope.tenant_id, correlation).await?;
    let receipt = BrowserSessionStore::decide_permission(
        &mut tx,
        &scope.tenant_id,
        &request.session_id,
        &request.tab_id,
        origin,
        permission,
        request.decision,
        &DecisionActor::User {
            user_id: request.user_id.clone(),
        },
        &at,
    )
    .await
    .map_err(|error| browser_error(error, correlation))?;
    audit(
        &mut tx,
        &scope.tenant_id,
        &request.workspace_id,
        &Actor::user(&request.user_id),
        "DecideBrowserPermission",
        &format!("browser_session:{}", request.session_id),
        &format!(
            "{permission} for {origin} on tab {}: {}",
            request.tab_id,
            request.decision.as_str()
        ),
        correlation,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let relayed = relay_receipt(state, &scope.tenant_id, &receipt, false).await;
    Ok(json!({ "receipt": receipt_json(&receipt), "worker_relayed": relayed }))
}

/// Park the owning run, when there is one, under the caller's transaction.
async fn park(
    runtime: &RuntimeStore,
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    run_id: Option<&str>,
    fence_id: &str,
    at: &str,
    correlation: &str,
) -> Result<Option<Run>, ApiError> {
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let run_id = CanonicalId::parse_typed(run_id, Prefix::Run)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    runtime
        .park_for_takeover_in(tx, batch, &run_id, fence_id, at)
        .await
        .map_err(|error| runtime_error(error, correlation))
}

/// Release the owning run, when there is one, under the caller's transaction.
async fn release(
    runtime: &RuntimeStore,
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    run_id: Option<&str>,
    fence_id: &str,
    correlation: &str,
) -> Result<Option<Run>, ApiError> {
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let run_id = CanonicalId::parse_typed(run_id, Prefix::Run)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    runtime
        .release_takeover_in(tx, batch, &run_id, fence_id)
        .await
        .map_err(|error| runtime_error(error, correlation))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn audit(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    workspace_id: &str,
    actor: &Actor,
    action: &str,
    target_ref: &str,
    reason: &str,
    correlation: &str,
) -> Result<(), ApiError> {
    append_in(
        tx,
        tenant_id,
        AuditDraft {
            workspace_id: Some(workspace_id.to_string()),
            actor: actor_json(actor),
            action: action.to_string(),
            target_ref: Some(target_ref.to_string()),
            decision: Some("applied".to_string()),
            reason: Some(reason.to_string()),
            correlation_id: Some(correlation.to_string()),
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))
}

#[cfg(test)]
mod tests {
    use quansio_machine::control::browser::ControlHolder;

    use super::*;

    #[test]
    fn a_takeover_names_exactly_one_subject() {
        assert!(matches!(
            subject(
                &json!({ "session_id": "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }),
                "c"
            ),
            Ok(Subject::Browser(_))
        ));
        assert!(matches!(
            subject(
                &json!({ "target_id": "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }),
                "c"
            ),
            Ok(Subject::Computer(_))
        ));
        for params in [
            json!({}),
            json!({ "session_id": "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "target_id": "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }),
            json!({ "session_id": "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }),
            json!({ "session_id": "bsn_nope" }),
        ] {
            let error = subject(&params, "c").expect_err("refused");
            assert_eq!(error.code, "VALIDATION_SCHEMA");
        }
    }

    #[test]
    fn automation_reports_the_fence_from_row_and_state() {
        let mut session = BrowserSession {
            id: "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".into(),
            tenant_id: "tn_x".into(),
            target_id: "tgt_x".into(),
            run_id: None,
            profile_ref: None,
            control_holder: ControlHolder::Agent,
            control_since: None,
            current_url: None,
            tabs: Vec::new(),
            screencast: json!({}),
            checkpoint_id: None,
            status: SessionStatus::Active,
            guard: quansio_machine::control::browser::GuardState::default(),
        };
        let free = automation_json(&session, Some(RunStatus::Running), None);
        assert_eq!(free["paused"], false);
        assert_eq!(free["agent_input_fenced"], false);
        session.control_holder = ControlHolder::User;
        session.status = SessionStatus::PausedTakeover;
        let held = automation_json(
            &session,
            Some(RunStatus::WaitingTakeover),
            Some(BrowserControlHolder::User),
        );
        assert_eq!(held["reason"], "user_control");
        assert_eq!(held["agent_input_fenced"], true);
        session.control_holder = ControlHolder::Nobody;
        session.status = SessionStatus::PausedPolicy;
        let policy = automation_json(&session, None, None);
        assert_eq!(policy["reason"], "policy");
        assert_eq!(policy["agent_input_fenced"], true);
    }
}
