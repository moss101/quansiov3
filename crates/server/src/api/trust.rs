//! Approvals, effects and evidence (APP-006): what Quansio wants to do and why.
//!
//! Projections over RUN-006's approval requests and receipts, RUN-007's Effect Ledger and
//! CORE-007's evidence, plus the §14 trust commands `ApproveEffect` and `DenyEffect`.
//!
//! An approval is bound to what the user previewed: the command carries the
//! `params_digest` the page showed, and a request whose digest differs, or that is no
//! longer `requested` (superseded because the params changed, expired, denied, already
//! granted), is refused with a typed code and nothing is granted. The grant itself is
//! RUN-006's signed single-use receipt; the resumed turn consumes it against the effect's
//! digest and generation before dispatching. Unsettled effects (`DISPATCHED`,
//! `OUTCOME_UNKNOWN`, `RECONCILING`, `RECONCILIATION_MANUAL`) are reported as such and
//! never re-dispatched or hidden.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::Utc;
use quansio_core::{CanonicalId, Digest, Generation, Prefix};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::artifacts::{ArtifactError, EvidenceId, EvidenceStore, ObjectStore};
use crate::control::schema;
use crate::effects::{EffectLedger, EffectRecord, EffectStatus};
use crate::policy::{ApprovalRequestStatus, ApprovalRuntime, ApprovalSigner, PolicyError};
use crate::runtime::state_machine::{RunTriggerKind, RuntimeIdentity, TurnInput};

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

/// `?workspace_id=` or `?run_id=`.
#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

fn scope_filter(query: &ScopeQuery) -> Result<(), ApiError> {
    if query.workspace_id.is_none() && query.run_id.is_none() {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            "workspace_id or run_id is required",
            "projection",
        ));
    }
    Ok(())
}

/// `GET /v1/approvals?workspace_id=|run_id=[&status=]`: every request with its typed
/// consequence preview, the digest it binds, the effect it authorizes and its receipts.
pub async fn approvals(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    scope_filter(&query)?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(
        "SELECT a.id, a.workspace_id, a.run_id, a.effect_id, a.requested_of, a.summary, a.consequence_preview, \
                a.params_digest, a.capability_projection_id, a.status, a.expires_at, a.created_at, a.updated_at, \
                e.effect_class, e.tier, e.resource, e.target_kind, e.target_id, e.status AS effect_status, e.tool_call_id, e.step_id, \
                t.tool_name, t.args, \
                (SELECT jsonb_agg(jsonb_build_object('id', r.id, 'approver_user_id', r.approver_user_id, \
                        'granted_at', r.granted_at, 'expires_at', r.expires_at, \
                        'consumed', (e.approval_receipt_id = r.id)) ORDER BY r.granted_at) \
                 FROM approval_receipts r WHERE r.tenant_id = a.tenant_id AND r.request_id = a.id) AS receipts \
         FROM approval_requests a \
         LEFT JOIN effect_records e ON e.id = a.effect_id AND e.tenant_id = a.tenant_id \
         LEFT JOIN tool_calls t ON t.id = e.tool_call_id AND t.tenant_id = e.tenant_id \
         WHERE a.tenant_id = $1 AND ($2::text IS NULL OR a.workspace_id = $2) AND ($3::text IS NULL OR a.run_id = $3) \
           AND ($4::text IS NULL OR a.status = $4) \
         ORDER BY a.created_at DESC LIMIT 200",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .bind(&query.run_id)
    .bind(&query.status)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({ "approvals": rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"),
        "workspace_id": row.get::<String, _>("workspace_id"),
        "run_id": row.get::<String, _>("run_id"),
        "effect_id": row.get::<String, _>("effect_id"),
        "requested_of": row.get::<Value, _>("requested_of"),
        "summary": row.get::<String, _>("summary"),
        "consequence_preview": row.get::<Value, _>("consequence_preview"),
        "params_digest": row.get::<String, _>("params_digest"),
        "capability_projection_id": row.get::<String, _>("capability_projection_id"),
        "status": row.get::<String, _>("status"),
        "expires_at": row.get::<chrono::DateTime<chrono::Utc>, _>("expires_at"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        "effect": row.get::<Option<String>, _>("effect_class").map(|class| json!({
            "class": class,
            "tier": row.get::<Option<i16>, _>("tier"),
            "resource": row.get::<Option<Value>, _>("resource"),
            "target": row.get::<Option<String>, _>("target_kind").map(|kind| json!({ "kind": kind, "id": row.get::<Option<String>, _>("target_id") })),
            "status": row.get::<Option<String>, _>("effect_status"),
            "tool_call_id": row.get::<Option<String>, _>("tool_call_id"),
            "step_id": row.get::<Option<String>, _>("step_id"),
            "tool_name": row.get::<Option<String>, _>("tool_name"),
            "args": row.get::<Option<Value>, _>("args"),
        })),
        "receipts": row.get::<Option<Value>, _>("receipts").unwrap_or_else(|| json!([])),
    })).collect::<Vec<_>>() })))
}

fn effect_json(record: &EffectRecord) -> Value {
    json!({
        "id": record.id,
        "workspace_id": record.workspace_id,
        "run_id": record.run_id,
        "step_id": record.step_id,
        "tool_call_id": record.tool_call_id,
        "effect_class": record.effect_class.as_str(),
        "tier": record.tier,
        "resource": record.resource,
        "params_digest": record.params_digest.to_string(),
        "idempotency_key": record.idempotency_key,
        "capability_projection_id": record.capability_projection_id,
        "policy_decision_id": record.policy_decision_id,
        "approval_receipt_id": record.approval_receipt_id,
        "status": record.status.as_str(),
        "unsettled": record.status.is_unsettled() || record.status == EffectStatus::ReconciliationManual,
        "terminal": record.status.is_terminal(),
        "target": record.target,
        "outcome": record.outcome,
        "reconciliation": record.reconciliation,
        "generation": record.generation.get(),
        "reserved_at": record.reserved_at,
        "dispatched_at": record.dispatched_at,
        "settled_at": record.settled_at,
        "created_at": record.created_at,
    })
}

/// `GET /v1/effects?run_id=|workspace_id=[&status=]`: the Effect Ledger as it is — class,
/// tier, resource, target, the digest of what was dispatched, the outcome with its evidence
/// ids, the reconciliation record, and whether the effect is still unsettled.
pub async fn effects(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    scope_filter(&query)?;
    let identity = identity_for(&scope);
    let ledger = EffectLedger::new(state.pool().clone(), identity).map_err(projection_error)?;
    let status = match query.status.as_deref() {
        Some(value) => Some(EffectStatus::parse(value).map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                error.to_string(),
                "projection",
            )
        })?),
        None => None,
    };
    let records = match (&query.run_id, &query.workspace_id) {
        (Some(run_id), _) => ledger
            .list_for_run(run_id, status)
            .await
            .map_err(projection_error)?,
        (None, Some(workspace_id)) => ledger
            .list_for_workspace(workspace_id, status, 500)
            .await
            .map_err(projection_error)?,
        (None, None) => Vec::new(),
    };
    Ok(Json(
        json!({ "effects": records.iter().map(effect_json).collect::<Vec<_>>() }),
    ))
}

/// `GET /v1/evidence/{id}/content`: the evidence bytes, verified against the recorded
/// digest by CORE-007 on the way out. A mismatch is `EVIDENCE_DIGEST_MISMATCH`, never bytes.
pub async fn evidence_content(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(evidence_id): Path<String>,
) -> Result<Response, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let id = EvidenceId::parse(&evidence_id).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            error.to_string(),
            "projection",
        )
    })?;
    let Some(objects) = state.objects().cloned() else {
        return Err(ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "no object store is configured on this deployment",
            "projection",
        ));
    };
    let objects: Arc<dyn ObjectStore> = objects;
    let mut tx = state.tenant_tx(&scope.tenant_id, "projection").await?;
    let store = EvidenceStore::new(objects.as_ref());
    let evidence = store
        .load(&mut tx, &scope.tenant_id, &id)
        .await
        .map_err(|error| evidence_error(error, "projection"))?;
    let bytes = store
        .fetch_bytes(&mut tx, &scope.tenant_id, &id)
        .await
        .map_err(|error| evidence_error(error, "projection"))?;
    tx.commit().await.map_err(projection_error)?;
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{evidence_id}\""),
        )
        .header("x-content-type-options", "nosniff")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("x-quansio-digest", evidence.content_digest.to_string())
        .body(Body::from(bytes))
        .map_err(projection_error)?;
    Ok(response)
}

/// A digest mismatch is not the caller's fault and not retryable: the stored bytes do not
/// hash to the recorded digest. The §15 vocabulary has no integrity family, so the code is
/// `INTERNAL` with typed `details` a client can branch on.
fn evidence_error(error: ArtifactError, correlation: &str) -> ApiError {
    match &error {
        ArtifactError::DigestMismatch {
            entity,
            id,
            expected,
            actual,
        } => ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation).with_details(
            json!({
                "kind": "digest_mismatch",
                "entity": entity,
                "id": id,
                "expected": expected.to_string(),
                "actual": actual.to_string(),
            }),
        ),
        ArtifactError::NotFound { .. } | ArtifactError::NoBytes(_) => {
            ApiError::new(ApiErrorCode::NotFound, error.to_string(), correlation)
        }
        ArtifactError::AccessDenied { .. } => ApiError::new(
            ApiErrorCode::CapabilityDenied,
            error.to_string(),
            correlation,
        ),
        _ => ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation),
    }
}

fn identity_for(scope: &TenantScope) -> RuntimeIdentity {
    RuntimeIdentity::system(
        &scope.tenant_id,
        "api",
        quansio_core::CorrelationId::generate(&mut quansio_core::UlidGenerator::new()),
    )
}

// ---- commands ----

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

fn policy_error(error: PolicyError, correlation: &str) -> ApiError {
    let message = error.to_string();
    let code = match &error {
        PolicyError::ApprovalNotPending { .. } => ApiErrorCode::ApprovalSuperseded,
        PolicyError::ApprovalInvalid(_) => ApiErrorCode::ApprovalInvalid,
        PolicyError::RuntimeConflict(_) | PolicyError::RunStateConflict { .. } => {
            ApiErrorCode::ConflictState
        }
        PolicyError::ApprovalNotFound { .. } | PolicyError::EffectNotFound { .. } => {
            ApiErrorCode::NotFound
        }
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, message, correlation)
}

/// Bind the command to what the user previewed: the request must still be `requested` and
/// its digest must be the one presented.
async fn bound_request(
    runtime: &ApprovalRuntime,
    request_id: &str,
    presented_digest: &str,
    correlation: &str,
) -> Result<crate::policy::ApprovalRequestRecord, ApiError> {
    let request = runtime
        .store()
        .load_approval_request(request_id)
        .await
        .map_err(|error| policy_error(error, correlation))?;
    match request.status {
        ApprovalRequestStatus::Requested => {}
        ApprovalRequestStatus::Superseded => {
            return Err(ApiError::new(
                ApiErrorCode::ApprovalSuperseded,
                format!("approval {request_id} was superseded: the effect's parameters changed after the preview"),
                correlation,
            ));
        }
        ApprovalRequestStatus::Expired => {
            return Err(ApiError::new(
                ApiErrorCode::ApprovalExpired,
                format!("approval {request_id} expired"),
                correlation,
            ));
        }
        other => {
            return Err(ApiError::new(
                ApiErrorCode::ConflictState,
                format!("approval {request_id} is already {}", other.as_str()),
                correlation,
            ));
        }
    }
    if request.expires_at <= Utc::now() {
        return Err(ApiError::new(
            ApiErrorCode::ApprovalExpired,
            format!("approval {request_id} expired"),
            correlation,
        ));
    }
    let presented: Digest = presented_digest.parse().map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("params_digest: {error}"),
            correlation,
        )
    })?;
    if presented != request.params_digest {
        return Err(ApiError::new(
            ApiErrorCode::ApprovalSuperseded,
            "the previewed params_digest does not match the request: the payload changed after the preview",
            correlation,
        ));
    }
    Ok(request)
}

/// `ApproveEffect {approval_request_id, params_digest}`: grant the signed receipt, release
/// the parked run, and run its next turn (which consumes the receipt and dispatches).
pub async fn approve_effect(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
    next_turn: impl FnOnce(
        &ApiState,
        RuntimeIdentity,
    ) -> Result<
        crate::runtime::state_machine::RuntimeEngine,
        crate::runtime::state_machine::RuntimeError,
    >,
) -> Result<Value, ApiError> {
    let Some(user_id) = user_id else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "ApproveEffect needs an acting user",
            correlation,
        ));
    };
    let request_id = required_str(params, "approval_request_id", correlation)?;
    let presented = required_str(params, "params_digest", correlation)?;
    let Some(signer) = ApprovalSigner::from_env() else {
        return Err(ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "the approval signing key (QUANSIO_APPROVAL_SIGNING_KEY) is not configured",
            correlation,
        ));
    };
    let runtime = ApprovalRuntime::new(state.pool().clone(), identity.clone())
        .map_err(|error| policy_error(error, correlation))?;
    let request = bound_request(&runtime, request_id, presented, correlation).await?;
    // The decision is the approver's to make: DOMAIN.md §2's approve_effect family,
    // decided by the server from the user's membership in the request's workspace.
    decider(state, identity, user_id, &request.workspace_id, correlation).await?;
    let run_id = CanonicalId::parse_typed(&request.run_id, Prefix::Run)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let run = runtime
        .runtime()
        .store()
        .load_run(&run_id)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let receipt = runtime
        .grant_and_resume(request_id, user_id, run.generation, &signer, Utc::now())
        .await
        .map_err(|error| policy_error(error, correlation))?;
    let mut result = json!({
        "approval_request_id": request_id,
        "receipt_id": receipt.id.to_string(),
        "effect_id": receipt.effect_id,
        "params_digest": receipt.params_digest.to_string(),
        "run_id": request.run_id,
        "granted_at": receipt.granted_at,
        "expires_at": receipt.expires_at,
    });
    // The released run continues in this request: the resumed turn consumes the receipt.
    let engine = next_turn(state, identity.clone())
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let outcome = engine
        .run_turn(
            &run_id,
            run.generation,
            TurnInput::new(RunTriggerKind::Wake).with_reference(request_id.to_string()),
        )
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    result["turn"] = super::commands::turn_json(&outcome);
    result["reply_message_id"] = json!(
        reply_for(
            state,
            identity,
            &request.run_id,
            &run.agent_thread_id.to_string(),
            &outcome,
            correlation
        )
        .await?
    );
    Ok(result)
}

/// Refuse a decision by a user who is not an approver (or admin) of the workspace.
async fn decider(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: &str,
    workspace_id: &str,
    correlation: &str,
) -> Result<(), ApiError> {
    let scope = TenantScope {
        tenant_id: identity.tenant_id.clone(),
        user_id: Some(user_id.to_string()),
        session_id: None,
        principal: None,
    };
    super::rbac::Authorization::check(
        state.pool(),
        &scope,
        Some(workspace_id),
        crate::policy::rbac::ActionFamily::ApproveEffect,
        correlation,
    )
    .await
    .map(|_| ())
}

/// The resumed turn's assistant text becomes the agent's durable message in the run's thread.
async fn reply_for(
    state: &ApiState,
    identity: &RuntimeIdentity,
    run_id: &str,
    agent_id: &str,
    outcome: &crate::runtime::state_machine::TurnOutcome,
    correlation: &str,
) -> Result<Option<String>, ApiError> {
    let Some(thread_id) =
        super::commands::thread_of_run(state, &identity.tenant_id, run_id).await?
    else {
        return Ok(None);
    };
    let store = crate::control::conversation::ConversationStore::new(
        state.pool().clone(),
        identity.clone(),
    )
    .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    super::commands::post_agent_reply(&store, &thread_id, agent_id, run_id, outcome, correlation)
        .await
}

/// `DenyEffect {approval_request_id}`: deny the request, record the effect as `DENIED`,
/// release the run and let its next turn see the refusal.
pub async fn deny_effect(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
    next_turn: impl FnOnce(
        &ApiState,
        RuntimeIdentity,
    ) -> Result<
        crate::runtime::state_machine::RuntimeEngine,
        crate::runtime::state_machine::RuntimeError,
    >,
) -> Result<Value, ApiError> {
    let Some(user_id) = user_id else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "DenyEffect needs an acting user",
            correlation,
        ));
    };
    let request_id = required_str(params, "approval_request_id", correlation)?;
    let mut acting = identity.clone();
    acting.actor = quansio_events::Actor::user(user_id);
    let runtime = ApprovalRuntime::new(state.pool().clone(), acting)
        .map_err(|error| policy_error(error, correlation))?;
    let request = runtime
        .store()
        .load_approval_request(request_id)
        .await
        .map_err(|error| policy_error(error, correlation))?;
    decider(state, identity, user_id, &request.workspace_id, correlation).await?;
    if request.status != ApprovalRequestStatus::Requested {
        return Err(ApiError::new(
            ApiErrorCode::ConflictState,
            format!(
                "approval {request_id} is already {}",
                request.status.as_str()
            ),
            correlation,
        ));
    }
    runtime
        .store()
        .deny_approval(request_id)
        .await
        .map_err(|error| policy_error(error, correlation))?;
    let ledger = EffectLedger::new(state.pool().clone(), identity.clone())
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let effect = ledger
        .deny(&request.effect_id)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let run_id = CanonicalId::parse_typed(&request.run_id, Prefix::Run)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let run = runtime
        .runtime()
        .store()
        .load_run(&run_id)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let generation: Generation = run.generation;
    runtime
        .runtime()
        .resolve_wait(
            &run_id,
            generation,
            crate::runtime::state_machine::WaitResolution::Approval {
                approval_id: request_id.to_string(),
            },
        )
        .await
        .map_err(|error| {
            ApiError::new(ApiErrorCode::ConflictState, error.to_string(), correlation)
        })?;
    let engine = next_turn(state, identity.clone())
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let outcome = engine
        .run_turn(
            &run_id,
            generation,
            TurnInput::new(RunTriggerKind::Wake).with_reference(request_id.to_string()),
        )
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let reply = reply_for(
        state,
        identity,
        &request.run_id,
        &run.agent_thread_id.to_string(),
        &outcome,
        correlation,
    )
    .await?;
    Ok(json!({
        "approval_request_id": request_id,
        "effect_id": effect.id,
        "effect_status": effect.status.as_str(),
        "run_id": request.run_id,
        "turn": super::commands::turn_json(&outcome),
        "reply_message_id": reply,
    }))
}
