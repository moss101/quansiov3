//! Conversation, objective and thread surface (APP-004): the HTTP projection and command
//! path of the conversation owner (`control::conversation`), the runtime's turns, steps,
//! model calls, tool calls and questions (RUN-011) and CORE-007's artifacts.
//!
//! Nothing here is a second store. Every read is a projection over the canonical tables;
//! every write is a §14 command or CORE-007's own store. The per-turn inspector is the
//! `/v1/runs/{id}/timeline` projection: for each turn, the model route that answered, the
//! context projection's segments with their trust labels, the tool calls with their effect
//! status, the questions asked and answered, and the attachments the trigger carried.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use quansio_core::CanonicalId;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::artifacts::{
    ArtifactGrant, ArtifactKind, ArtifactStore, GrantLevel, NewArtifact, NewVersion, Origin,
    OriginKind, ProducedBy, Retention,
};
use crate::control::conversation::ConversationStore;
use crate::control::schema;
use crate::runtime::state_machine::RuntimeIdentity;

pub use crate::control::conversation::{Message, PostedMessage, Thread, ThreadSummary};

/// Largest upload accepted from the composer (bytes, after base64 decoding).
pub const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

fn identity_for(scope: &TenantScope) -> Result<RuntimeIdentity, ApiError> {
    let correlation =
        quansio_core::CorrelationId::generate(&mut quansio_core::UlidGenerator::new());
    Ok(RuntimeIdentity::system(
        &scope.tenant_id,
        "api",
        correlation,
    ))
}

/// `?workspace_id=` on the list projections.
#[derive(Debug, Deserialize)]
pub struct WorkspaceQuery {
    workspace_id: String,
}

/// `GET /v1/threads?workspace_id=`: the workspace's threads, most recently active first.
pub async fn list_threads(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let store = ConversationStore::new(state.pool().clone(), identity_for(&scope)?)
        .map_err(projection_error)?;
    let threads = store
        .list_threads(&query.workspace_id)
        .await
        .map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationBounds,
                error.to_string(),
                "projection",
            )
        })?;
    Ok(Json(json!({
        "threads": threads.iter().map(|thread| json!({
            "id": thread.id,
            "workspace_id": thread.workspace_id,
            "kind": thread.kind,
            "title": thread.title,
            "last_activity_at": thread.last_activity_at,
            "message_count": thread.message_count,
        })).collect::<Vec<_>>(),
    })))
}

/// `?run_id=` or `?thread_id=` on `GET /v1/questions`.
#[derive(Debug, Deserialize)]
pub struct QuestionsQuery {
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
}

/// `GET /v1/questions?run_id=|thread_id=`: open and answered agent questions.
pub async fn questions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<QuestionsQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    if query.run_id.is_none() && query.thread_id.is_none() {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            "questions need run_id or thread_id",
            "projection",
        ));
    }
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(
        "SELECT id, run_id, step_id, thread_id, kind, prompt, options, required, status, answer, expires_at, created_at, updated_at \
         FROM questions q WHERE q.tenant_id = $1 AND ($2::text IS NULL OR q.run_id = $2) \
         AND ($3::text IS NULL OR q.thread_id = $3 OR EXISTS ( \
             SELECT 1 FROM runs r JOIN messages m ON m.id = r.trigger_ref AND m.tenant_id = r.tenant_id \
             WHERE r.tenant_id = q.tenant_id AND r.id = q.run_id AND m.thread_id = $3)) \
         ORDER BY q.created_at ASC",
    )
    .bind(&scope.tenant_id)
    .bind(&query.run_id)
    .bind(&query.thread_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(
        json!({ "questions": rows.iter().map(question_json).collect::<Vec<_>>() }),
    ))
}

fn question_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": row.get::<String, _>("id"),
        "run_id": row.get::<String, _>("run_id"),
        "step_id": row.get::<Option<String>, _>("step_id"),
        "thread_id": row.get::<Option<String>, _>("thread_id"),
        "kind": row.get::<String, _>("kind"),
        "prompt": row.get::<String, _>("prompt"),
        "options": row.get::<Value, _>("options"),
        "required": row.get::<bool, _>("required"),
        "status": row.get::<String, _>("status"),
        "answer": row.get::<Option<Value>, _>("answer"),
        "expires_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("expires_at"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
    })
}

/// `GET /v1/runs/{id}/timeline`: the per-turn inspector and the run's ordered history.
pub async fn timeline(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    CanonicalId::parse_typed(&run_id, quansio_core::Prefix::Run).map_err(|error| {
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

    let run = sqlx::query(
        "SELECT id, workspace_id, work_node_id, agent_thread_id, status, generation, trigger_kind, trigger_ref, \
                created_at, updated_at FROM runs WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(&run_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(projection_error)?
    .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound, format!("run {run_id} is not in this tenant"), "projection"))?;
    let workspace_id: String = run.get("workspace_id");
    let trigger_ref: Option<String> = run.get("trigger_ref");

    // The trigger message, with its attachments, is where the run started.
    let trigger = match &trigger_ref {
        Some(message_id) if message_id.starts_with("msg_") => sqlx::query(
            "SELECT id, thread_id, seq, author_kind, author_id, content_blocks, attachments FROM messages \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&scope.tenant_id)
        .bind(message_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(projection_error)?
        .map(|row| {
            json!({
                "message_id": row.get::<String, _>("id"),
                "thread_id": row.get::<String, _>("thread_id"),
                "seq": row.get::<i64, _>("seq"),
                "author_kind": row.get::<String, _>("author_kind"),
                "author_id": row.get::<String, _>("author_id"),
                "content_blocks": row.get::<Value, _>("content_blocks"),
                "attachments": row.get::<Value, _>("attachments"),
            })
        }),
        _ => None,
    };

    let turns = sqlx::query(
        "SELECT id, seq, input_kind, input_ref, context_projection_id, status, step_count, token_ledger, started_at, ended_at \
         FROM turns WHERE tenant_id = $1 AND run_id = $2 ORDER BY seq",
    )
    .bind(&scope.tenant_id)
    .bind(&run_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;

    let mut turn_views = Vec::with_capacity(turns.len());
    for turn in &turns {
        let turn_id: String = turn.get("id");
        let steps = sqlx::query(
            "SELECT s.id, s.seq, s.kind, s.status, s.ref, s.evidence_ids, s.effect_id, s.created_at, s.updated_at \
             FROM steps s WHERE s.tenant_id = $1 AND s.turn_id = $2 ORDER BY s.seq",
        )
        .bind(&scope.tenant_id)
        .bind(&turn_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(projection_error)?;
        let mut step_views = Vec::with_capacity(steps.len());
        for step in &steps {
            let step_id: String = step.get("id");
            let kind: String = step.get("kind");
            let mut view = json!({
                "id": step_id,
                "seq": step.get::<i32, _>("seq"),
                "kind": kind,
                "status": step.get::<String, _>("status"),
                "ref": step.get::<Option<String>, _>("ref"),
                "evidence_ids": step.get::<Value, _>("evidence_ids"),
                "effect_id": step.get::<Option<String>, _>("effect_id"),
                "created_at": step.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                "updated_at": step.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
            });
            match kind.as_str() {
                "model_call" => {
                    // Which model answered: the model call and the route it was made on.
                    let call = sqlx::query(
                        "SELECT c.id, c.call_id, c.status, c.stop_reason, c.usage, c.latency_ms, c.context_projection_id, \
                                r.id AS route_id, r.request_class, r.provider, r.model_id, r.chosen_by, r.fallbacks, r.dlp_profile \
                         FROM model_calls c LEFT JOIN model_routes r ON r.id = c.route_id \
                         WHERE c.tenant_id = $1 AND c.step_id = $2 ORDER BY c.created_at DESC LIMIT 1",
                    )
                    .bind(&scope.tenant_id)
                    .bind(&step_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(projection_error)?;
                    view["model_call"] = call.map_or(Value::Null, |row| {
                        json!({
                            "id": row.get::<String, _>("id"),
                            "call_id": row.get::<String, _>("call_id"),
                            "status": row.get::<String, _>("status"),
                            "stop_reason": row.get::<Option<String>, _>("stop_reason"),
                            "usage": row.get::<Value, _>("usage"),
                            "latency_ms": row.get::<Option<i64>, _>("latency_ms"),
                            "route": row.get::<Option<String>, _>("route_id").map(|route_id| json!({
                                "id": route_id,
                                "request_class": row.get::<String, _>("request_class"),
                                "provider": row.get::<String, _>("provider"),
                                "model_id": row.get::<String, _>("model_id"),
                                "chosen_by": row.get::<String, _>("chosen_by"),
                                "fallbacks": row.get::<Value, _>("fallbacks"),
                                "dlp_profile": row.get::<Option<String>, _>("dlp_profile"),
                            })),
                        })
                    });
                }
                "tool_call" => {
                    let calls = sqlx::query(
                        "SELECT tc.id, tc.tool_name, tc.args_digest, tc.status, tc.effect_id, \
                                e.effect_class, e.status AS effect_status \
                         FROM tool_calls tc LEFT JOIN effect_records e ON e.id = tc.effect_id \
                         WHERE tc.tenant_id = $1 AND tc.step_id = $2 ORDER BY tc.created_at",
                    )
                    .bind(&scope.tenant_id)
                    .bind(&step_id)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(projection_error)?;
                    view["tool_calls"] = json!(calls
                        .iter()
                        .map(|row| json!({
                            "id": row.get::<String, _>("id"),
                            "tool_name": row.get::<String, _>("tool_name"),
                            "args_digest": row.get::<String, _>("args_digest"),
                            "status": row.get::<String, _>("status"),
                            "effect_id": row.get::<Option<String>, _>("effect_id"),
                            "effect_class": row.get::<Option<String>, _>("effect_class"),
                            "effect_status": row.get::<Option<String>, _>("effect_status"),
                        }))
                        .collect::<Vec<_>>());
                }
                "wait" => {
                    // A wait step whose ref is a question: the question itself, answered or not.
                    if let Some(reference) = step.get::<Option<String>, _>("ref") {
                        let question = sqlx::query(
                            "SELECT id, run_id, step_id, thread_id, kind, prompt, options, required, status, answer, expires_at, created_at, updated_at \
                             FROM questions WHERE tenant_id = $1 AND (id = $2 OR step_id = $3) ORDER BY created_at DESC LIMIT 1",
                        )
                        .bind(&scope.tenant_id)
                        .bind(&reference)
                        .bind(&step_id)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(projection_error)?;
                        view["question"] = question.as_ref().map_or(Value::Null, question_json);
                    }
                }
                _ => {}
            }
            step_views.push(view);
        }

        // What the model saw: the context projection's segments, each with its trust label.
        let context_projection_id: Option<String> = turn.get("context_projection_id");
        let context = match &context_projection_id {
            Some(id) => sqlx::query(
                "SELECT id, segments, token_ledger, degradation, search_program_id FROM context_projections \
                 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&scope.tenant_id)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(projection_error)?
            .map(|row| {
                let segments: Value = row.get("segments");
                let sources = segments.as_array().map(|items| items.iter().map(|segment| json!({
                    "kind": segment.get("kind").cloned().unwrap_or(Value::Null),
                    "source": segment.get("source").or_else(|| segment.get("ref")).cloned().unwrap_or(Value::Null),
                    "trust_level": segment.get("trust_level").or_else(|| segment.get("trust")).cloned().unwrap_or(Value::Null),
                    "tokens": segment.get("tokens").cloned().unwrap_or(Value::Null),
                })).collect::<Vec<_>>()).unwrap_or_default();
                json!({
                    "id": row.get::<String, _>("id"),
                    "sources": sources,
                    "token_ledger": row.get::<Value, _>("token_ledger"),
                    "degradation": row.get::<Value, _>("degradation"),
                    "search_program_id": row.get::<Option<String>, _>("search_program_id"),
                })
            }),
            None => None,
        };

        turn_views.push(json!({
            "id": turn_id,
            "seq": turn.get::<i32, _>("seq"),
            "input": { "kind": turn.get::<String, _>("input_kind"), "ref": turn.get::<Option<String>, _>("input_ref") },
            "status": turn.get::<String, _>("status"),
            "step_count": turn.get::<i32, _>("step_count"),
            "token_ledger": turn.get::<Value, _>("token_ledger"),
            "context": context,
            "steps": step_views,
            "started_at": turn.get::<chrono::DateTime<chrono::Utc>, _>("started_at"),
            "ended_at": turn.get::<Option<chrono::DateTime<chrono::Utc>>, _>("ended_at"),
        }));
    }

    let questions = sqlx::query(
        "SELECT id, run_id, step_id, thread_id, kind, prompt, options, required, status, answer, expires_at, created_at, updated_at \
         FROM questions WHERE tenant_id = $1 AND run_id = $2 ORDER BY created_at",
    )
    .bind(&scope.tenant_id)
    .bind(&run_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;

    Ok(Json(json!({
        "run": {
            "id": run_id,
            "workspace_id": workspace_id,
            "work_node_id": run.get::<String, _>("work_node_id"),
            "agent_thread_id": run.get::<String, _>("agent_thread_id"),
            "status": run.get::<String, _>("status"),
            "generation": run.get::<i64, _>("generation"),
            "trigger": { "kind": run.get::<String, _>("trigger_kind"), "ref": trigger_ref },
            "created_at": run.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            "updated_at": run.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        },
        "trigger_message": trigger,
        "turns": turn_views,
        "questions": questions.iter().map(question_json).collect::<Vec<_>>(),
    })))
}

/// Params of the `CreateArtifact` command (DOMAIN.md §14, "upload init/complete" in one
/// bounded step): a base64-encoded upload from the composer.
#[derive(Debug, Deserialize)]
pub struct UploadRequest {
    workspace_id: String,
    title: String,
    #[serde(default = "default_media_type")]
    media_type: String,
    bytes_base64: String,
    #[serde(default)]
    thread_id: Option<String>,
}

fn default_media_type() -> String {
    "application/octet-stream".to_string()
}

fn kind_for(media_type: &str) -> ArtifactKind {
    match media_type {
        m if m.starts_with("image/") => ArtifactKind::Image,
        "text/csv" | "application/json" => ArtifactKind::Data,
        m if m.starts_with("text/") || m == "application/pdf" => ArtifactKind::Document,
        _ => ArtifactKind::Other,
    }
}

/// `CreateArtifact`: upload → Artifact + first version (CORE-007) so a message can attach it.
///
/// # Errors
/// `AUTH_REQUIRED` without an acting user, `TARGET_UNAVAILABLE` when no object store is
/// configured, `VALIDATION_*` for a bad title, encoding or size.
pub async fn create_artifact(
    state: &ApiState,
    tenant_id: &str,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let request: UploadRequest = serde_json::from_value(params.clone()).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            error.to_string(),
            correlation,
        )
    })?;
    let Some(user_id) = user_id.map(str::to_string) else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "uploads need an acting user",
            correlation,
        ));
    };
    let Some(objects) = state.objects().cloned() else {
        return Err(ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "no object store is configured on this deployment; attachments are unavailable",
            correlation,
        ));
    };
    let title = request.title.trim();
    if title.is_empty() || title.len() > 200 {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            "title must be 1..=200 characters",
            correlation,
        ));
    }
    let bytes = STANDARD
        .decode(request.bytes_base64.trim())
        .map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("bytes_base64: {error}"),
                correlation,
            )
        })?;
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD_BYTES {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            format!("upload must be 1..={MAX_UPLOAD_BYTES} bytes"),
            correlation,
        ));
    }
    let mut conn = state.pool().acquire().await.map_err(projection_error)?;
    let store = ArtifactStore::new(objects.as_ref());
    let artifact = store
        .create(
            &mut conn,
            tenant_id,
            &user_id,
            &NewArtifact {
                workspace_id: request.workspace_id.clone(),
                kind: kind_for(&request.media_type),
                title: title.to_string(),
                role: Some("attachment".to_string()),
                origin: Origin {
                    kind: OriginKind::Upload,
                    reference: request.thread_id.clone().unwrap_or_else(|| user_id.clone()),
                },
                grants: vec![ArtifactGrant {
                    principal: user_id.clone(),
                    level: GrantLevel::Write,
                }],
                retention: Retention::default(),
            },
        )
        .await
        .map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationBounds,
                error.to_string(),
                correlation,
            )
        })?;
    let stored = store
        .add_version(
            &mut conn,
            tenant_id,
            &user_id,
            &artifact.id,
            &NewVersion {
                bytes,
                media_type: request.media_type.clone(),
                expected_digest: None,
                produced_by: ProducedBy::default(),
            },
        )
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    Ok(json!({
        "artifact_id": artifact.id.to_canonical(),
        "version_id": stored.version.id.to_canonical(),
        "workspace_id": artifact.workspace_id,
        "title": artifact.title,
        "kind": artifact.kind,
        "media_type": stored.version.media_type,
        "size_bytes": stored.version.size_bytes,
        "content_digest": stored.version.content_digest.to_string(),
        "replayed": stored.replayed,
    }))
}

/// `GET /v1/artifacts?workspace_id=`: the workspace's artifacts, newest first.
pub async fn list_artifacts(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(
        "SELECT a.id, a.kind, a.title, a.role, a.origin, a.current_version_id, a.created_at, \
                v.media_type, v.size_bytes, v.content_digest \
         FROM artifacts a LEFT JOIN artifact_versions v ON v.id = a.current_version_id \
         WHERE a.tenant_id = $1 AND a.workspace_id = $2 AND a.deleted_at IS NULL \
         ORDER BY a.created_at DESC LIMIT 200",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({ "artifacts": rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"),
        "kind": row.get::<String, _>("kind"),
        "title": row.get::<String, _>("title"),
        "role": row.get::<Option<String>, _>("role"),
        "origin": row.get::<Value, _>("origin"),
        "current_version_id": row.get::<Option<String>, _>("current_version_id"),
        "media_type": row.get::<Option<String>, _>("media_type"),
        "size_bytes": row.get::<Option<i64>, _>("size_bytes"),
        "content_digest": row.get::<Option<String>, _>("content_digest"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
    })).collect::<Vec<_>>() })))
}

/// `GET /v1/artifacts/{id}/versions`.
pub async fn artifact_versions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(artifact_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(
        "SELECT id, seq, content_digest, size_bytes, media_type, produced_by_run_id, produced_by_step_id, \
                produced_by_user_id, parent_version_id, created_at \
         FROM artifact_versions WHERE tenant_id = $1 AND artifact_id = $2 ORDER BY seq",
    )
    .bind(&scope.tenant_id)
    .bind(&artifact_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({ "versions": rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"),
        "seq": row.get::<i32, _>("seq"),
        "content_digest": row.get::<String, _>("content_digest"),
        "size_bytes": row.get::<i64, _>("size_bytes"),
        "media_type": row.get::<String, _>("media_type"),
        "produced_by": {
            "run_id": row.get::<Option<String>, _>("produced_by_run_id"),
            "step_id": row.get::<Option<String>, _>("produced_by_step_id"),
            "user_id": row.get::<Option<String>, _>("produced_by_user_id"),
        },
        "parent_version_id": row.get::<Option<String>, _>("parent_version_id"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
    })).collect::<Vec<_>>() })))
}
