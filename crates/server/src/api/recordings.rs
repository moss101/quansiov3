//! Skill recording through the API (ADP-003, DOMAIN.md §11.7, §14): five commands and two projections.
//!
//! Every recording command is a person's: a service principal is refused before anything is read, the
//! owner fence (user, session family, epoch) is built here from the authenticated session and checked by
//! the store, and nothing a thread, a model or a tool says can reach these handlers. The handlers hold no
//! SQL; the session row, the archive's validation and the registry write belong to
//! `control::skills::recording`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::skills::recording::{
    RecordingCaller, RecordingCompiler, RecordingConfig, RecordingError, SlotDecision,
};
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::RuntimeIdentity;

/// Rows the list projection returns at most.
const MAX_ROWS: i64 = 100;

fn error(error: RecordingError, correlation: &str) -> ApiError {
    if error.code() == "INTERNAL" {
        eprintln!("quansio-server: recording failed ({correlation}): {error}");
    }
    let code = ApiErrorCode::ALL
        .into_iter()
        .find(|known| known.as_str() == error.code())
        .unwrap_or(ApiErrorCode::Internal);
    let message = if code == ApiErrorCode::Internal {
        "the recording could not be processed; try again".to_string()
    } else {
        error.to_string()
    };
    let refusal = ApiError::new(code, message, correlation);
    match error.details() {
        Some(details) => refusal.with_details(details),
        None => refusal,
    }
}

fn required_str<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::new(ApiErrorCode::ValidationSchema, format!("{key} is required"), correlation)
        })
}

fn epoch(params: &Value, correlation: &str) -> Result<i64, ApiError> {
    params
        .get("epoch")
        .and_then(Value::as_i64)
        .filter(|epoch| *epoch >= 1)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "epoch is required: a positive integer the desktop advances when the signed-in account changes",
                correlation,
            )
        })
}

/// The person making the call, with the owner fence the store checks.
async fn caller(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<RecordingCaller, ApiError> {
    if scope.principal.is_some() {
        return Err(ApiError::new(
            ApiErrorCode::ScopeForbidden,
            "a recording is a person's: a service principal cannot start, finish or submit one",
            correlation,
        ));
    }
    let Some(user_id) = scope.user_id.clone() else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "recording needs a signed-in person",
            correlation,
        ));
    };
    let session_family_id = match &scope.session_id {
        Some(session_id) => state
            .auth()
            .session_family(&scope.tenant_id, session_id)
            .await
            .map_err(|failure| {
                ApiError::new(ApiErrorCode::Internal, failure.to_string(), correlation)
            })?,
        None => None,
    };
    Ok(RecordingCaller {
        tenant_id: scope.tenant_id.clone(),
        user_id,
        session_family_id,
        epoch: epoch(params, correlation)?,
        correlation_id: identity.correlation_id,
    })
}

fn compiler(state: &ApiState) -> Result<RecordingCompiler, ApiError> {
    // Only a compile calls the plane; a deployment whose model settings do not load can still start,
    // finish and cancel recordings.
    let deadline = crate::intelligence::model_source::ModelCallSettings::load()
        .map_or(std::time::Duration::from_secs(60), |settings| settings.deadline);
    Ok(RecordingCompiler::new(
        state.pool().clone(),
        state.intelligence().cloned(),
        deadline,
    ))
}

/// A user may act on a recording only while they can still author work in its workspace.
async fn authorize_workspace(
    state: &ApiState,
    scope: &TenantScope,
    workspace_id: &str,
    correlation: &str,
) -> Result<(), ApiError> {
    super::rbac::Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await
    .map(|_| ())
}

async fn authorize_recording(
    state: &ApiState,
    scope: &TenantScope,
    compiler: &RecordingCompiler,
    recording_id: &str,
    correlation: &str,
) -> Result<(), ApiError> {
    let Some(user_id) = scope.user_id.as_deref() else {
        return Ok(());
    };
    // Unknown or another person's recording: the store gives the typed answer.
    if let Some(row) = compiler
        .store()
        .get_owned(&scope.tenant_id, user_id, recording_id)
        .await
        .map_err(|failure| error(failure, correlation))?
    {
        authorize_workspace(state, scope, &row.workspace_id, correlation).await?;
    }
    Ok(())
}

/// `StartRecording {workspace_id, start_key, epoch}`.
pub async fn start_recording(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let caller = caller(state, scope, identity, params, correlation).await?;
    let workspace_id = required_str(params, "workspace_id", correlation)?;
    let start_key = required_str(params, "start_key", correlation)?;
    authorize_workspace(state, scope, workspace_id, correlation).await?;
    let config = RecordingConfig::committed();
    let recording = compiler(state)?
        .store()
        .start(&caller, workspace_id, start_key, config)
        .await
        .map_err(|failure| error(failure, correlation))?;
    Ok(json!({
        "recording": recording.to_json(false),
        "recording_id": recording.id,
        "epoch": recording.epoch,
        "expires_at": recording.expires_at,
        "limits": config.limits_json(),
        "rule_set": crate::control::skills::recording::RULE_SET,
    }))
}

/// `FinishRecording {recording_id, epoch, archive {content_base64, content_digest}}`: the only way a
/// recording becomes finished.
pub async fn finish_recording(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let caller = caller(state, scope, identity, params, correlation).await?;
    let recording_id = required_str(params, "recording_id", correlation)?;
    let compiler = compiler(state)?;
    authorize_recording(state, scope, &compiler, recording_id, correlation).await?;
    let archive = params.get("archive").ok_or_else(|| {
        ApiError::new(ApiErrorCode::ValidationSchema, "archive is required", correlation)
    })?;
    let encoded = required_str(archive, "content_base64", correlation)?;
    let digest = required_str(archive, "content_digest", correlation)?;
    let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
    let config = RecordingConfig::committed();
    // Base64 inflates by a third; refuse an upload that cannot fit before decoding it.
    if encoded.len() > config.archive_max_bytes / 3 * 4 + 8 {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            format!("the archive is over the {} byte limit", config.archive_max_bytes),
            correlation,
        ));
    }
    let bytes = STANDARD.decode(encoded.trim()).map_err(|failure| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("archive.content_base64: {failure}"),
            correlation,
        )
    })?;
    let Some(objects) = state.objects().cloned() else {
        return Err(ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "no object store is configured on this deployment; a recording cannot be stored",
            correlation,
        ));
    };
    let recording = compiler
        .store()
        .finish(&caller, objects, recording_id, &bytes, digest, config)
        .await
        .map_err(|failure| error(failure, correlation))?;
    Ok(json!({ "recording": recording.to_json(false) }))
}

/// `CancelRecording {recording_id, epoch}`.
pub async fn cancel_recording(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let caller = caller(state, scope, identity, params, correlation).await?;
    let recording_id = required_str(params, "recording_id", correlation)?;
    let compiler = compiler(state)?;
    authorize_recording(state, scope, &compiler, recording_id, correlation).await?;
    let recording = compiler
        .store()
        .cancel(&caller, recording_id)
        .await
        .map_err(|failure| error(failure, correlation))?;
    Ok(json!({ "recording": recording.to_json(false) }))
}

/// `CompileRecording {recording_id, epoch}`: the plane proposes, the server validates and keeps it.
pub async fn compile_recording(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let caller = caller(state, scope, identity, params, correlation).await?;
    let recording_id = required_str(params, "recording_id", correlation)?;
    let compiler = compiler(state)?;
    authorize_recording(state, scope, &compiler, recording_id, correlation).await?;
    let Some(objects) = state.objects().cloned() else {
        return Err(ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "no object store is configured on this deployment",
            correlation,
        ));
    };
    let outcome = compiler
        .compile(&caller, objects.as_ref(), recording_id)
        .await
        .map_err(|failure| error(failure, correlation))?;
    Ok(json!({
        "recording": outcome.recording.to_json(false),
        "proposal": outcome.proposal,
    }))
}

#[derive(Debug, Deserialize)]
struct SubmitSlot {
    slot_id: String,
    classification: String,
}

/// `SubmitRecordingSkill {recording_id, epoch, name, semver, slots[]}`: one `DRAFT` skill version.
pub async fn submit_recording_skill(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let caller = caller(state, scope, identity, params, correlation).await?;
    let recording_id = required_str(params, "recording_id", correlation)?;
    let name = required_str(params, "name", correlation)?;
    let semver = required_str(params, "semver", correlation)?;
    let slots: Vec<SubmitSlot> = serde_json::from_value(params.get("slots").cloned().unwrap_or_else(|| json!([])))
        .map_err(|failure| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("slots: {failure}"),
                correlation,
            )
        })?;
    let compiler = compiler(state)?;
    authorize_recording(state, scope, &compiler, recording_id, correlation).await?;
    let decisions: Vec<SlotDecision> = slots
        .into_iter()
        .map(|slot| SlotDecision {
            slot_id: slot.slot_id,
            classification: slot.classification,
        })
        .collect();
    let outcome = compiler
        .submit(&caller, recording_id, name, semver, &decisions)
        .await
        .map_err(|failure| error(failure, correlation))?;
    Ok(json!({
        "recording": outcome.recording.to_json(false),
        "skill_id": outcome.skill_id,
        "skill_version": {
            "skill_version_id": outcome.version.id.to_string(),
            "semver": outcome.version.semver,
            "status": outcome.version.status.as_str(),
            "resolves": outcome.version.resolves(),
        },
    }))
}

/// `?limit=` for the list projection.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    limit: Option<i64>,
}

fn session_user(scope: &TenantScope) -> Result<&str, ApiError> {
    scope.user_id.as_deref().ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::AuthRequired,
            "recordings are listed for a signed-in person",
            "projection",
        )
    })
}

/// `GET /v1/recordings`: the caller's own recordings with state and redaction summary: never the archive
/// and never someone else's.
pub async fn recordings(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let user_id = session_user(&scope)?;
    let rows = compiler(&state)?
        .store()
        .list_owned(&scope.tenant_id, user_id, query.limit.unwrap_or(MAX_ROWS).clamp(1, MAX_ROWS))
        .await
        .map_err(|failure| error(failure, "projection"))?;
    Ok(Json(json!({
        "recordings": rows.iter().map(|row| row.to_json(false)).collect::<Vec<_>>(),
    })))
}

/// `GET /v1/recordings/{id}`: one of the caller's own recordings, with its proposal when compiled.
pub async fn recording(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let user_id = session_user(&scope)?;
    let row = compiler(&state)?
        .store()
        .get_owned(&scope.tenant_id, user_id, &id)
        .await
        .map_err(|failure| error(failure, "projection"))?
        .ok_or_else(|| {
            ApiError::new(ApiErrorCode::NotFound, format!("recording {id} not found"), "projection")
        })?;
    Ok(Json(json!({ "recording": row.to_json(true) })))
}
