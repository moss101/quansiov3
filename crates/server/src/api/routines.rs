//! Routines and notifications over the public API (APP-010, DOMAIN.md §13, §14).
//!
//! Commands: `CreateRoutine`, `UpdateRoutine`, `PauseRoutine`, `ResumeRoutine`,
//! `DeleteRoutine`, `TriggerRoutineNow` (the routine owner, `control::routines`) and
//! `UpdateNotificationPreferences`, `MarkNotificationRead` (the notification owner,
//! `notify`). Projections: `GET /v1/routines?workspace_id=` (with each routine's recent
//! fires) and `GET /v1/notifications?workspace_id=` (the caller's own, with their
//! preferences).
//!
//! [`InteractiveDispatch`] is the deployable's `RunDispatch`: it executes a routine fire by
//! the same steps `PostMessage` answers a message with — the routine's objective thread
//! gets the prompt as a message, `route_to_teammate` chooses the teammate, the run's
//! capability projection is seeded from the same policies, the turn runs through the same
//! engine and the reply is posted — and it looks every step up before creating it, so a
//! fire resumed after a crash continues instead of repeating.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::Actor;
use quansio_graph::{GraphStore, NewWorkNode, WorkNodeKind, WorkOrigin};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::commands::{
    chat_engine, internal, load_target, post_agent_reply, route_to_teammate, runtime_error,
    seed_projection,
};
use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::collaboration::CollaborationStore;
use crate::control::conversation::{ConversationStore, NewMessage};
use crate::control::routines::{FireLedger, NewRoutine, RoutineChange, RoutineError, RoutineStore};
use crate::control::schema;
use crate::notify::{NotificationStore, NotifyError};
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::{
    NewRun, RunStatus, RunTriggerKind, RuntimeIdentity, TurnInput,
};
use crate::scheduler::{
    AbsencePolicy, DispatchOutcome, GraphDispatch, RoutineFireOutcome, RoutineFireRequest,
    RoutineScheduler, RoutineTrigger, RunDispatch, RunDispatchRequest, RunResumeRequest,
    SchedulerError,
};

fn routine_error(error: RoutineError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn notify_error(error: NotifyError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "NOT_FOUND" => ApiErrorCode::NotFound,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn required<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    text(params, key).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("{key} is required"),
            correlation,
        )
    })
}

fn signed_in<'a>(scope: &'a TenantScope, correlation: &str) -> Result<&'a str, ApiError> {
    scope.user_id.as_deref().ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::AuthRequired,
            "routines and notifications belong to a signed-in person",
            correlation,
        )
    })
}

fn trigger_of(value: &Value, correlation: &str) -> Result<RoutineTrigger, ApiError> {
    serde_json::from_value(value.clone()).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("trigger is {{kind: cron|interval|event, spec, timezone?}}: {error}"),
            correlation,
        )
    })
}

fn absence_of(value: Option<&str>, correlation: &str) -> Result<Option<AbsencePolicy>, ApiError> {
    value
        .map(|policy| {
            AbsencePolicy::from_db_str(policy).map_err(|_| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    "absence_policy is skip, queue or catch_up_once",
                    correlation,
                )
            })
        })
        .transpose()
}

async fn check_teammate(
    state: &ApiState,
    scope: &TenantScope,
    workspace_id: &str,
    teammate_id: &str,
    correlation: &str,
) -> Result<(), ApiError> {
    CanonicalId::parse_typed(teammate_id, Prefix::Teammate).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("teammate_id: {error}"),
            correlation,
        )
    })?;
    let mut tx = state
        .pool()
        .begin()
        .await
        .map_err(|error| internal(error, correlation))?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(|error| internal(error, correlation))?;
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM teammates WHERE tenant_id = $1 AND workspace_id = $2 AND id = $3",
    )
    .bind(&scope.tenant_id)
    .bind(workspace_id)
    .bind(teammate_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| internal(error, correlation))?;
    tx.commit()
        .await
        .map_err(|error| internal(error, correlation))?;
    match status.as_deref() {
        Some("active") => Ok(()),
        Some(other) => Err(ApiError::new(
            ApiErrorCode::ConflictState,
            format!("teammate {teammate_id} is {other}"),
            correlation,
        )),
        None => Err(ApiError::new(
            ApiErrorCode::NotFound,
            format!("teammate {teammate_id} is not in workspace {workspace_id}"),
            correlation,
        )),
    }
}

fn store_for(state: &ApiState, scope: &TenantScope) -> RoutineStore {
    let actor = scope
        .user_id
        .as_deref()
        .map_or_else(|| Actor::system("api"), Actor::user);
    RoutineStore::new(state.pool().clone(), scope.tenant_id.clone(), actor)
}

/// A routine the caller may change: its owner, or a workspace admin.
async fn manageable(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<crate::control::routines::RoutineView, ApiError> {
    let routine_id = required(params, "routine_id", correlation)?;
    let routine = store_for(state, scope)
        .get(routine_id)
        .await
        .map_err(|error| routine_error(error, correlation))?;
    let owner = scope.user_id.as_deref() == Some(routine.owner_user_id.as_str());
    let action = if owner {
        ActionFamily::AuthorWork
    } else {
        ActionFamily::ManageWorkspace
    };
    Authorization::check(
        state.pool(),
        scope,
        Some(&routine.workspace_id),
        action,
        correlation,
    )
    .await?;
    Ok(routine)
}

/// `CreateRoutine {workspace_id, title, prompt, trigger {kind, spec, timezone?},
/// teammate_id?, absence_policy?, budget?, notification_prefs?}`.
pub async fn create_routine(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let owner = signed_in(scope, correlation)?;
    let workspace_id = required(params, "workspace_id", correlation)?;
    Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let trigger = trigger_of(params.get("trigger").unwrap_or(&Value::Null), correlation)?;
    let teammate_id = text(params, "teammate_id").map(str::to_string);
    if let Some(teammate) = &teammate_id {
        check_teammate(state, scope, workspace_id, teammate, correlation).await?;
    }
    let new = NewRoutine {
        workspace_id: workspace_id.to_string(),
        owner_user_id: owner.to_string(),
        teammate_id,
        trigger,
        objective_template: json!({
            "title": text(params, "title").unwrap_or(""),
            "prompt": text(params, "prompt").unwrap_or(""),
        }),
        budget: params.get("budget").cloned().unwrap_or_else(|| json!({})),
        notification_prefs: params
            .get("notification_prefs")
            .cloned()
            .unwrap_or_else(|| json!({})),
        absence_policy: absence_of(text(params, "absence_policy"), correlation)?
            .unwrap_or(AbsencePolicy::Skip),
    };
    let routine = store_for(state, scope)
        .create(new, Utc::now())
        .await
        .map_err(|error| routine_error(error, correlation))?;
    Ok(routine.to_json())
}

/// `UpdateRoutine {routine_id, title?, prompt?, trigger?, teammate_id?, absence_policy?}`.
pub async fn update_routine(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let routine = manageable(state, scope, params, correlation).await?;
    let mut change = RoutineChange::default();
    if let Some(trigger) = params.get("trigger") {
        change.trigger = Some(trigger_of(trigger, correlation)?);
    }
    if params.get("title").is_some() || params.get("prompt").is_some() {
        change.objective_template = Some(json!({
            "title": text(params, "title").or_else(|| routine.objective_template.get("title").and_then(Value::as_str)).unwrap_or(""),
            "prompt": text(params, "prompt").or_else(|| routine.objective_template.get("prompt").and_then(Value::as_str)).unwrap_or(""),
        }));
    }
    match params.get("teammate_id") {
        None => {}
        Some(Value::Null) => change.teammate_id = Some(None),
        Some(value) => {
            let teammate = value.as_str().ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    "teammate_id is an agt_ id or null",
                    correlation,
                )
            })?;
            check_teammate(state, scope, &routine.workspace_id, teammate, correlation).await?;
            change.teammate_id = Some(Some(teammate.to_string()));
        }
    }
    change.absence_policy = absence_of(text(params, "absence_policy"), correlation)?;
    let routine = store_for(state, scope)
        .update(&routine.id, change, Utc::now())
        .await
        .map_err(|error| routine_error(error, correlation))?;
    Ok(routine.to_json())
}

/// `PauseRoutine`, `ResumeRoutine` or `DeleteRoutine {routine_id}`.
pub async fn transition_routine(
    state: &ApiState,
    scope: &TenantScope,
    command: &str,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let routine = manageable(state, scope, params, correlation).await?;
    let store = store_for(state, scope);
    let routine = match command {
        "PauseRoutine" => store.pause(&routine.id).await,
        "ResumeRoutine" => store.resume(&routine.id, Utc::now()).await,
        _ => store.delete(&routine.id).await,
    }
    .map_err(|error| routine_error(error, correlation))?;
    Ok(routine.to_json())
}

/// `TriggerRoutineNow {routine_id}`: claim a fire keyed by this command and execute it now
/// through the same executor the scheduler uses.
pub async fn trigger_routine_now(
    state: &ApiState,
    scope: &TenantScope,
    command_id: &str,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let routine_id = required(params, "routine_id", correlation)?;
    let store = store_for(state, scope);
    let routine = store
        .get(routine_id)
        .await
        .map_err(|error| routine_error(error, correlation))?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&routine.workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let fire = store
        .trigger_now(routine_id, command_id)
        .await
        .map_err(|error| routine_error(error, correlation))?;
    let scheduler = RoutineScheduler::new(state.pool().clone(), scope.tenant_id.clone())
        .map_err(|error| internal(error, correlation))?;
    let dispatch = InteractiveDispatch::new(state.clone());
    scheduler
        .execute_pending(&dispatch, Utc::now(), 16)
        .await
        .map_err(|error| internal(error, correlation))?;
    let fire = FireLedger::new(state.pool().clone(), scope.tenant_id.clone())
        .by_key(&fire.fire_key)
        .await
        .map_err(|error| internal(error, correlation))?
        .unwrap_or(fire);
    Ok(fire.to_json())
}

/// `UpdateNotificationPreferences {workspace_id, kind, channels[]}` for the caller.
pub async fn update_notification_preferences(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user = signed_in(scope, correlation)?;
    let workspace_id = required(params, "workspace_id", correlation)?;
    Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::Read,
        correlation,
    )
    .await?;
    let kind = required(params, "kind", correlation)?;
    let channels = params
        .get("channels")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "channels is a list",
                correlation,
            )
        })?
        .iter()
        .map(|channel| {
            channel.as_str().map(str::to_string).ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    "channels are strings",
                    correlation,
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stored = NotificationStore::new(state.pool().clone(), scope.tenant_id.clone())
        .set_preference(workspace_id, user, kind, channels)
        .await
        .map_err(|error| notify_error(error, correlation))?;
    Ok(json!({ "workspace_id": workspace_id, "kind": kind, "channels": stored }))
}

/// `MarkNotificationRead {notification_id}`: the caller's own notification only.
pub async fn mark_notification_read(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user = signed_in(scope, correlation)?;
    let id = required(params, "notification_id", correlation)?;
    let notification = NotificationStore::new(state.pool().clone(), scope.tenant_id.clone())
        .mark_read(id, user)
        .await
        .map_err(|error| notify_error(error, correlation))?;
    Ok(notification.to_json())
}

// ------------------------------------------------------------------ projections

/// `?workspace_id=`.
#[derive(Debug, Deserialize)]
pub struct WorkspaceQuery {
    workspace_id: String,
    #[serde(default)]
    unread: Option<bool>,
}

/// `GET /v1/routines?workspace_id=`: the workspace's routines with their recent fires.
pub async fn routines(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
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
    let routines = store_for(&state, &scope)
        .list(&query.workspace_id)
        .await
        .map_err(|error| routine_error(error, "projection"))?;
    let ledger = FireLedger::new(state.pool().clone(), scope.tenant_id.clone());
    let mut items = Vec::with_capacity(routines.len());
    for routine in &routines {
        let fires = ledger
            .fires_of(&routine.id, 10)
            .await
            .map_err(|error| internal(error, "projection"))?;
        let mut item = routine.to_json();
        item["fires"] = Value::Array(fires.iter().map(|fire| fire.to_json()).collect());
        items.push(item);
    }
    Ok(Json(
        json!({ "workspace_id": query.workspace_id, "items": items, "cursor": Value::Null }),
    ))
}

/// `GET /v1/notifications?workspace_id=&unread=`: the caller's notifications and preferences.
pub async fn notifications(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let user = signed_in(&scope, "projection")?.to_string();
    Authorization::check(
        state.pool(),
        &scope,
        Some(&query.workspace_id),
        ActionFamily::Read,
        "projection",
    )
    .await?;
    let store = NotificationStore::new(state.pool().clone(), scope.tenant_id.clone());
    let items = store
        .list(
            &query.workspace_id,
            &user,
            query.unread.unwrap_or(false),
            200,
        )
        .await
        .map_err(|error| notify_error(error, "projection"))?;
    let preferences = store
        .preferences(&query.workspace_id, &user)
        .await
        .map_err(|error| notify_error(error, "projection"))?;
    Ok(Json(json!({
        "workspace_id": query.workspace_id,
        "items": items.iter().map(|item| item.to_json()).collect::<Vec<_>>(),
        "unread": items.iter().filter(|item| item.read_at.is_none()).count(),
        "preferences": preferences.iter().map(|(kind, channels, chosen)| json!({
            "kind": kind, "channels": channels, "chosen": chosen,
        })).collect::<Vec<_>>(),
        "cursor": Value::Null,
    })))
}

// ------------------------------------------------------------------ the executor

/// The deployable's scheduler dispatch: routine fires take the interactive path.
#[derive(Clone)]
pub struct InteractiveDispatch {
    state: ApiState,
}

impl InteractiveDispatch {
    /// A dispatch over the composed API state.
    #[must_use]
    pub fn new(state: ApiState) -> Self {
        Self { state }
    }
}

fn dispatch_failure(subject: &str, error: impl std::fmt::Display) -> SchedulerError {
    SchedulerError::RunDispatch {
        run_id: subject.to_string(),
        reason: error.to_string(),
    }
}

#[async_trait]
impl RunDispatch for InteractiveDispatch {
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        // A run a person or a routine started is driven by its own request path; the
        // scheduler moving it too would race that path.
        let mut tx = self.state.pool().begin().await?;
        schema::set_tenant_context(&mut tx, &request.tenant_id).await?;
        let trigger: Option<String> =
            sqlx::query_scalar("SELECT trigger_kind FROM runs WHERE tenant_id = $1 AND id = $2")
                .bind(&request.tenant_id)
                .bind(&request.run_id)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        if matches!(trigger.as_deref(), Some("message" | "routine")) {
            return Ok(DispatchOutcome::NotDispatchable);
        }
        GraphDispatch::new(self.state.pool().clone(), request.tenant_id.clone())
            .dispatch_queued(request)
            .await
    }

    async fn resume_timer_wait(
        &self,
        request: RunResumeRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        GraphDispatch::new(self.state.pool().clone(), request.tenant_id.clone())
            .resume_timer_wait(request)
            .await
    }

    async fn fire_routine(
        &self,
        request: RoutineFireRequest,
    ) -> Result<RoutineFireOutcome, SchedulerError> {
        fire_routine(&self.state, &request).await.map_err(|error| {
            dispatch_failure(
                &request.routine_id,
                format!("{}: {}", error.code, error.message),
            )
        })
    }
}

/// Execute one routine fire by the interactive path, continuing whatever a previous attempt
/// already did.
async fn fire_routine(
    state: &ApiState,
    request: &RoutineFireRequest,
) -> Result<RoutineFireOutcome, ApiError> {
    let correlation_id = CorrelationId::generate(&mut UlidGenerator::new());
    let correlation = correlation_id.to_string();
    let identity = RuntimeIdentity::system(
        &request.tenant_id,
        format!("routine:{}", request.routine_id),
        correlation_id,
    );
    let ledger = FireLedger::new(state.pool().clone(), request.tenant_id.clone());
    let fire = ledger
        .by_key(&request.fire_key)
        .await
        .map_err(|error| internal(error, &correlation))?
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::NotFound,
                format!("fire {} is not claimed", request.fire_key),
                &correlation,
            )
        })?;
    let title = request
        .objective_template
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Routine")
        .to_string();
    let prompt = request
        .objective_template
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // 1. The Objective of this fire, found by its key.
    let objective_id = match existing_objective(
        state,
        &request.tenant_id,
        &request.fire_key,
        &correlation,
    )
    .await?
    {
        Some(id) => id,
        None => {
            let store = GraphStore::new(state.pool().clone(), request.tenant_id.clone())
                .map_err(|error| internal(error, &correlation))?;
            let mut node = NewWorkNode::new(
                &request.workspace_id,
                WorkNodeKind::Objective,
                title.clone(),
                json!({ "kind": "routine", "id": request.routine_id, "fire_key": request.fire_key }),
            );
            node.origin = WorkOrigin::Routine;
            node.description = Some(prompt.clone());
            store
                .create_node(node)
                .await
                .map_err(|error| internal(error, &correlation))?
                .id
        }
    };
    ledger
        .record_step(&fire.id, None, None, Some(&objective_id.to_string()), None)
        .await
        .map_err(|error| internal(error, &correlation))?;

    // 2. The routine's thread: the one its earlier fires used, else a new objective thread.
    let collaboration = CollaborationStore::new(state.pool().clone(), identity.clone());
    let thread_id = match fire.thread_id.clone() {
        Some(thread) => thread,
        None => match ledger
            .thread_of(&request.routine_id)
            .await
            .map_err(|error| internal(error, &correlation))?
        {
            Some(thread) => thread,
            None => {
                collaboration
                    .create_thread(
                        &request.workspace_id,
                        "objective",
                        Some(title.clone()),
                        None,
                        Vec::new(),
                    )
                    .await
                    .map_err(|error| {
                        super::collaboration::collaboration_error(error, &correlation)
                    })?
                    .0
                    .id
            }
        },
    };
    ledger
        .record_step(&fire.id, Some(&thread_id), None, None, None)
        .await
        .map_err(|error| internal(error, &correlation))?;

    // 3. The teammate, chosen exactly as for a message.
    let (agent_thread, teammate) = route_to_teammate(
        state,
        &identity,
        &request.workspace_id,
        &objective_id,
        request.teammate_id.as_deref(),
        &correlation,
    )
    .await?;

    // 4. The fire's message, found by the key in its notice.
    let conversation = ConversationStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, &correlation))?;
    let marker = json!([{ "type": "system_notice", "notice": { "fire_key": request.fire_key } }]);
    let message = match conversation
        .find_by_block(&thread_id, &marker)
        .await
        .map_err(|error| internal(error, &correlation))?
    {
        Some(message) => message,
        None => {
            let window = request.fire_window.to_rfc3339();
            let blocks = json!([
                {
                    "type": "system_notice",
                    "text": format!("Routine \u{201c}{title}\u{201d} fired ({}).", fire.source),
                    "notice": {
                        "kind": "routine_fire",
                        "routine_id": request.routine_id,
                        "fire_key": request.fire_key,
                        "window": window,
                        "source": fire.source,
                    },
                },
                { "type": "text", "text": prompt },
            ]);
            let new = NewMessage {
                author_kind: "system",
                author_id: format!("routine:{}", request.routine_id),
                content_blocks: blocks,
                attachments: Vec::new(),
                mentions: Vec::new(),
                run_id: None,
                turn_id: None,
            };
            collaboration
                .post(
                    &thread_id,
                    new,
                    teammate.as_ref().map(|teammate| teammate.id.to_string()),
                )
                .await
                .map_err(|error| super::collaboration::collaboration_error(error, &correlation))?
                .1
        }
    };
    ledger
        .record_step(&fire.id, None, Some(&message.id), None, None)
        .await
        .map_err(|error| internal(error, &correlation))?;

    // 5. The run the message started, found by its trigger; created, projected, started and
    //    turned exactly as PostMessage does, continuing from where a previous attempt stopped.
    let engine =
        chat_engine(state, identity.clone()).map_err(|error| internal(error, &correlation))?;
    let existing = existing_run(state, &request.tenant_id, &message.id, &correlation).await?;
    let run = match existing {
        Some(run_id) => engine
            .store()
            .load_run(&run_id)
            .await
            .map_err(|error| runtime_error(error, &correlation))?,
        None => {
            let mut new_run = NewRun::new(
                request.workspace_id.clone(),
                objective_id,
                agent_thread,
                RunTriggerKind::Routine,
            );
            new_run.trigger_ref = Some(message.id.clone());
            let preferred = teammate
                .as_ref()
                .and_then(|teammate| teammate.default_execution_target_id.clone());
            if let Some(target) = preferred {
                new_run.execution_target_id = Some(target);
            } else if let Some(target) =
                load_target(state, &request.tenant_id, &request.workspace_id).await?
            {
                new_run.execution_target_id = Some(target);
            }
            let run = engine
                .create_run(new_run)
                .await
                .map_err(|error| runtime_error(error, &correlation))?;
            seed_projection(state, &identity, &request.workspace_id, &run)
                .await
                .map_err(|error| internal(error, &correlation))?;
            run
        }
    };
    ledger
        .record_step(&fire.id, None, None, None, Some(&run.id.to_string()))
        .await
        .map_err(|error| internal(error, &correlation))?;
    let mut run = run;
    if run.status == RunStatus::Created {
        run = engine
            .enqueue(&run.id, run.generation)
            .await
            .map_err(|error| runtime_error(error, &correlation))?;
    }
    if run.status == RunStatus::Queued {
        run = engine
            .start(&run.id, run.generation)
            .await
            .map_err(|error| runtime_error(error, &correlation))?;
    }
    let turns = engine
        .store()
        .list_turns(&run.id)
        .await
        .map_err(|error| runtime_error(error, &correlation))?;
    if run.status == RunStatus::Running && turns.is_empty() {
        let outcome = engine
            .run_turn(
                &run.id,
                run.generation,
                TurnInput::new(RunTriggerKind::Routine).with_reference(message.id.clone()),
            )
            .await
            .map_err(|error| runtime_error(error, &correlation))?;
        post_agent_reply(
            &conversation,
            &thread_id,
            &agent_thread.to_string(),
            &run.id.to_string(),
            &outcome,
            &correlation,
        )
        .await?;
    }
    Ok(RoutineFireOutcome {
        objective_id: objective_id.to_string(),
        run_id: run.id.to_string(),
    })
}

async fn existing_objective(
    state: &ApiState,
    tenant_id: &str,
    fire_key: &str,
    correlation: &str,
) -> Result<Option<CanonicalId>, ApiError> {
    let mut tx = state
        .pool()
        .begin()
        .await
        .map_err(|error| internal(error, correlation))?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| internal(error, correlation))?;
    let row = sqlx::query(
        "SELECT id FROM work_nodes WHERE tenant_id = $1 AND created_by ->> 'fire_key' = $2 \
         ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(fire_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| internal(error, correlation))?;
    tx.commit()
        .await
        .map_err(|error| internal(error, correlation))?;
    row.map(|row| {
        CanonicalId::parse_typed(&row.get::<String, _>("id"), Prefix::WorkNode)
            .map_err(|error| internal(error, correlation))
    })
    .transpose()
}

async fn existing_run(
    state: &ApiState,
    tenant_id: &str,
    message_id: &str,
    correlation: &str,
) -> Result<Option<CanonicalId>, ApiError> {
    let mut tx = state
        .pool()
        .begin()
        .await
        .map_err(|error| internal(error, correlation))?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| internal(error, correlation))?;
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM runs WHERE tenant_id = $1 AND trigger_ref = $2 ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(message_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| internal(error, correlation))?;
    tx.commit()
        .await
        .map_err(|error| internal(error, correlation))?;
    id.map(|id| {
        CanonicalId::parse_typed(&id, Prefix::Run).map_err(|error| internal(error, correlation))
    })
    .transpose()
}

/// Shared by the composition's background loop: the dispatch it hands the scheduler.
#[must_use]
pub fn interactive_dispatch(state: &ApiState) -> Arc<dyn RunDispatch> {
    Arc::new(InteractiveDispatch::new(state.clone()))
}
