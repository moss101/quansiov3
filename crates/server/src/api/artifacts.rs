//! Artifact, version and evidence projections (APP-004 upload, APP-008 workspace).
//!
//! Every read is a projection over CORE-007's tables and its object store; the one write
//! is the catalogued `CreateArtifact` command, which goes through `ArtifactStore`. Bytes
//! are served with `nosniff` and an attachment disposition: the API never asks a client
//! to render them, and the desktop never does (DOMAIN.md §10.1: preview never executes
//! active content with host privileges).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use quansio_indexer::{SourceChange, SourceDocument};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::artifacts::{
    ArtifactError, ArtifactGrant, ArtifactKind, ArtifactStore, ArtifactVersionId, EvidenceId,
    EvidenceStore, GrantLevel, NewArtifact, NewVersion, ObjectKey, ObjectStore, ObjectStoreError,
    Origin, OriginKind, ProducedBy, Retention,
};
use crate::control::schema;

/// Largest upload accepted from the composer (bytes, after base64 decoding).
pub const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Largest object the search index reads text from; bigger ones index by title only.
pub const MAX_INDEXED_TEXT_BYTES: usize = 4 * 1024 * 1024;

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

fn artifact_error(error: ArtifactError, correlation: &str) -> ApiError {
    let code = match &error {
        ArtifactError::AccessDenied { .. } => ApiErrorCode::CapabilityDenied,
        ArtifactError::NotFound { .. } | ArtifactError::Deleted(_) => ApiErrorCode::NotFound,
        ArtifactError::DigestMismatch { .. } => ApiErrorCode::Internal,
        _ => ApiErrorCode::ValidationBounds,
    };
    ApiError::new(code, error.to_string(), correlation)
}

/// Whether a media type is text the index (and the desktop's preview) can show as text.
#[must_use]
pub fn is_text_media(media_type: &str) -> bool {
    let media = media_type.split(';').next().unwrap_or("").trim();
    media.starts_with("text/")
        || matches!(
            media,
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/xhtml+xml"
                | "image/svg+xml"
                | "application/x-yaml"
                | "application/yaml"
                | "application/toml"
                | "application/sql"
        )
}

/// Params of the `CreateArtifact` command (DOMAIN.md §14, "upload init/complete" in one
/// bounded step): a base64-encoded upload from the composer, or a new version of an
/// existing artifact when `artifact_id` is given.
#[derive(Debug, Deserialize)]
pub struct UploadRequest {
    workspace_id: String,
    title: String,
    #[serde(default = "default_media_type")]
    media_type: String,
    bytes_base64: String,
    #[serde(default)]
    thread_id: Option<String>,
    /// Add a version to this artifact instead of creating one (needs a write grant).
    #[serde(default)]
    artifact_id: Option<String>,
    /// Kind override; otherwise derived from the media type.
    #[serde(default)]
    kind: Option<String>,
}

fn default_media_type() -> String {
    "application/octet-stream".to_string()
}

fn parse_kind(kind: &str) -> Option<ArtifactKind> {
    serde_json::from_value(Value::String(kind.to_string())).ok()
}

fn kind_for(media_type: &str) -> ArtifactKind {
    let media = media_type.split(';').next().unwrap_or("").trim();
    match media {
        m if m.starts_with("image/") => ArtifactKind::Image,
        m if m.starts_with("audio/") => ArtifactKind::Audio,
        m if m.starts_with("video/") => ArtifactKind::Video,
        // A CSV/TSV file is a spreadsheet source (DOMAIN.md §10.1, CAP-003).
        "text/csv" | "text/tab-separated-values" => ArtifactKind::Spreadsheet,
        "application/json" | "application/x-ndjson" => ArtifactKind::Data,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.ms-excel"
        | "application/vnd.oasis.opendocument.spreadsheet" => ArtifactKind::Spreadsheet,
        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/vnd.ms-powerpoint"
        | "application/vnd.oasis.opendocument.presentation" => ArtifactKind::Presentation,
        "application/zip"
        | "application/gzip"
        | "application/x-tar"
        | "application/x-7z-compressed" => ArtifactKind::Archive,
        "application/javascript"
        | "application/x-javascript"
        | "text/x-python"
        | "text/x-rust"
        | "application/sql"
        | "text/x-shellscript" => ArtifactKind::Code,
        m if m.starts_with("text/")
            || m == "application/pdf"
            || m == "application/xml"
            || m.starts_with("application/vnd.openxmlformats-officedocument.wordprocessingml")
            || m == "application/msword"
            || m == "application/vnd.oasis.opendocument.text" =>
        {
            ArtifactKind::Document
        }
        _ => ArtifactKind::Other,
    }
}

/// `CreateArtifact`: upload → Artifact + version (CORE-007) so a message can attach it, or
/// a new version of an existing artifact.
///
/// # Errors
/// `AUTH_REQUIRED` without an acting user, `TARGET_UNAVAILABLE` when no object store is
/// configured, `VALIDATION_*` for a bad title, encoding or size, `CAPABILITY_DENIED`
/// without a write grant on an existing artifact.
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
    let kind = match request.kind.as_deref() {
        Some(kind) => parse_kind(kind).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("{kind:?} is not an artifact kind"),
                correlation,
            )
        })?,
        None => kind_for(&request.media_type),
    };
    let mut conn = state.pool().acquire().await.map_err(projection_error)?;
    let store = ArtifactStore::new(objects.as_ref());
    let artifact = match request.artifact_id.as_deref() {
        Some(existing) => {
            let id = crate::artifacts::ArtifactId::parse(existing).map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    error.to_string(),
                    correlation,
                )
            })?;
            let artifact = store
                .load(&mut conn, tenant_id, &user_id, &id)
                .await
                .map_err(|error| artifact_error(error, correlation))?;
            if artifact.workspace_id != request.workspace_id {
                return Err(ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    format!(
                        "{existing} is not an artifact of workspace {}",
                        request.workspace_id
                    ),
                    correlation,
                ));
            }
            artifact
        }
        None => store
            .create(
                &mut conn,
                tenant_id,
                &user_id,
                &NewArtifact {
                    workspace_id: request.workspace_id.clone(),
                    kind,
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
            .map_err(|error| artifact_error(error, correlation))?,
    };
    let stored = store
        .add_version(
            &mut conn,
            tenant_id,
            &user_id,
            &artifact.id,
            &NewVersion {
                bytes: bytes.clone(),
                media_type: request.media_type.clone(),
                expected_digest: None,
                produced_by: ProducedBy {
                    run_id: None,
                    step_id: None,
                    user_id: Some(user_id.clone()),
                },
            },
        )
        .await
        .map_err(|error| artifact_error(error, correlation))?;
    // The derived search index learns about the new text right away; a failure there is
    // logged, not surfaced: the index is rebuildable and the upload already happened.
    if let Some(index) = state.search_index() {
        let body = if is_text_media(&request.media_type) && bytes.len() <= MAX_INDEXED_TEXT_BYTES {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            String::new()
        };
        let document = SourceDocument::artifact(
            tenant_id,
            artifact.workspace_id.clone(),
            artifact.id.to_canonical(),
            stored.version.id.to_canonical(),
            artifact.title.clone(),
            body,
            stored.version.media_type.clone(),
            stored.version.content_digest.to_string(),
        );
        if let Err(error) = index.apply(SourceChange::upsert(document)) {
            eprintln!(
                "quansio-server: search index apply failed for {}: {error}",
                artifact.id.to_canonical()
            );
        }
    }
    Ok(json!({
        "artifact_id": artifact.id.to_canonical(),
        "version_id": stored.version.id.to_canonical(),
        "seq": stored.version.seq,
        "workspace_id": artifact.workspace_id,
        "title": artifact.title,
        "kind": artifact.kind,
        "media_type": stored.version.media_type,
        "size_bytes": stored.version.size_bytes,
        "content_digest": stored.version.content_digest.to_string(),
        "replayed": stored.replayed,
    }))
}

/// `?workspace_id=` plus the browser's typed filters.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    workspace_id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /v1/artifacts?workspace_id=[&kind=&run_id=]`: the workspace's artifacts, newest first,
/// with their current version and where they came from. Artifacts outlive their runs: a
/// closed, failed or cancelled run's artifacts are listed like any other.
pub async fn list_artifacts(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    if let Some(kind) = query.kind.as_deref() {
        parse_kind(kind).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("{kind:?} is not an artifact kind"),
                "projection",
            )
        })?;
    }
    let limit = query.limit.unwrap_or(200).clamp(1, 500);
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(
        "SELECT a.id, a.kind, a.title, a.role, a.origin, a.current_version_id, a.created_at, a.updated_at, \
                v.seq, v.media_type, v.size_bytes, v.content_digest, \
                v.produced_by_run_id, v.produced_by_step_id, v.produced_by_user_id, \
                (SELECT count(*) FROM artifact_versions x WHERE x.artifact_id = a.id AND x.tenant_id = a.tenant_id) AS version_count \
         FROM artifacts a LEFT JOIN artifact_versions v ON v.id = a.current_version_id \
         WHERE a.tenant_id = $1 AND a.workspace_id = $2 AND a.deleted_at IS NULL \
           AND ($3::text IS NULL OR a.kind = $3) \
           AND ($4::text IS NULL OR v.produced_by_run_id = $4 OR EXISTS ( \
                SELECT 1 FROM artifact_versions y WHERE y.artifact_id = a.id AND y.tenant_id = a.tenant_id AND y.produced_by_run_id = $4)) \
         ORDER BY a.updated_at DESC, a.created_at DESC LIMIT $5",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .bind(&query.kind)
    .bind(&query.run_id)
    .bind(limit)
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
        "current_seq": row.get::<Option<i32>, _>("seq"),
        "version_count": row.get::<i64, _>("version_count"),
        "media_type": row.get::<Option<String>, _>("media_type"),
        "size_bytes": row.get::<Option<i64>, _>("size_bytes"),
        "content_digest": row.get::<Option<String>, _>("content_digest"),
        "produced_by": {
            "run_id": row.get::<Option<String>, _>("produced_by_run_id"),
            "step_id": row.get::<Option<String>, _>("produced_by_step_id"),
            "user_id": row.get::<Option<String>, _>("produced_by_user_id"),
        },
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
    })).collect::<Vec<_>>() })))
}

/// `GET /v1/artifacts/{id}/versions`: every version with who produced it and the evidence
/// its producing step captured (the source/evidence links of the workspace).
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
    let artifact = sqlx::query(
        "SELECT id, workspace_id, kind, title, role, origin, current_version_id, deleted_at, created_at \
         FROM artifacts WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(&artifact_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(projection_error)?
    .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound, format!("{artifact_id} is not an artifact of this tenant"), "projection"))?;
    let rows = sqlx::query(
        "SELECT v.id, v.seq, v.content_digest, v.size_bytes, v.media_type, v.produced_by_run_id, v.produced_by_step_id, \
                v.produced_by_user_id, v.parent_version_id, v.format_check, v.created_at, \
                COALESCE((SELECT jsonb_agg(e.id ORDER BY e.captured_at) FROM evidence e \
                          WHERE e.tenant_id = v.tenant_id AND v.produced_by_step_id IS NOT NULL AND e.step_id = v.produced_by_step_id), '[]'::jsonb) AS evidence_ids \
         FROM artifact_versions v WHERE v.tenant_id = $1 AND v.artifact_id = $2 ORDER BY v.seq",
    )
    .bind(&scope.tenant_id)
    .bind(&artifact_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({
        "artifact": {
            "id": artifact.get::<String, _>("id"),
            "workspace_id": artifact.get::<String, _>("workspace_id"),
            "kind": artifact.get::<String, _>("kind"),
            "title": artifact.get::<String, _>("title"),
            "role": artifact.get::<Option<String>, _>("role"),
            "origin": artifact.get::<Value, _>("origin"),
            "current_version_id": artifact.get::<Option<String>, _>("current_version_id"),
            "deleted_at": artifact.get::<Option<chrono::DateTime<chrono::Utc>>, _>("deleted_at"),
            "created_at": artifact.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        },
        "versions": rows.iter().map(|row| json!({
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
            "evidence_ids": row.get::<Value, _>("evidence_ids"),
            "parent_version_id": row.get::<Option<String>, _>("parent_version_id"),
            "format_check": row.get::<Option<Value>, _>("format_check"),
            "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        })).collect::<Vec<_>>(),
    })))
}

/// `GET /v1/artifacts/{id}/versions/{vid}/content`: the version's bytes, digest-verified by
/// CORE-007 on the way out, for a principal with a read grant. Served as an attachment
/// with `nosniff` so no client is invited to render it.
pub async fn artifact_content(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path((artifact_id, version_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let Some(user_id) = scope.user_id.clone() else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "reading artifact bytes needs an acting user",
            "projection",
        ));
    };
    let Some(objects) = state.objects().cloned() else {
        return Err(ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "no object store is configured on this deployment",
            "projection",
        ));
    };
    let version = ArtifactVersionId::parse(&version_id).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            error.to_string(),
            "projection",
        )
    })?;
    let mut conn = state.pool().acquire().await.map_err(projection_error)?;
    let store = ArtifactStore::new(objects.as_ref());
    let (bytes, media_type, title) = fetch_version(
        &store,
        &mut conn,
        &scope.tenant_id,
        &user_id,
        &artifact_id,
        &version,
    )
    .await?;
    let filename = sanitize_filename(&title);
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, safe_content_type(&media_type))
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header("x-content-type-options", "nosniff")
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(Body::from(bytes))
        .map_err(projection_error)?;
    response.headers_mut().insert(
        "x-quansio-version",
        HeaderValue::from_str(&version_id).map_err(projection_error)?,
    );
    Ok(response)
}

async fn fetch_version(
    store: &ArtifactStore<'_>,
    conn: &mut sqlx::PgConnection,
    tenant_id: &str,
    user_id: &str,
    artifact_id: &str,
    version: &ArtifactVersionId,
) -> Result<(Vec<u8>, String, String), ApiError> {
    schema::set_tenant_context_conn(conn, tenant_id)
        .await
        .map_err(projection_error)?;
    let row = sqlx::query(
        "SELECT v.media_type, a.title, a.id AS artifact_id FROM artifact_versions v JOIN artifacts a ON a.id = v.artifact_id \
         WHERE v.tenant_id = $1 AND v.id = $2",
    )
    .bind(tenant_id)
    .bind(version.to_canonical())
    .fetch_optional(&mut *conn)
    .await
    .map_err(projection_error)?
    .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound, format!("{} is not a version of this tenant", version.to_canonical()), "projection"))?;
    if row.get::<String, _>("artifact_id") != artifact_id {
        return Err(ApiError::new(
            ApiErrorCode::NotFound,
            format!(
                "{} is not a version of {artifact_id}",
                version.to_canonical()
            ),
            "projection",
        ));
    }
    let bytes = store
        .fetch_bytes(conn, tenant_id, user_id, version)
        .await
        .map_err(|error| artifact_error(error, "projection"))?;
    Ok((
        bytes,
        row.get::<String, _>("media_type"),
        row.get::<String, _>("title"),
    ))
}

/// Active document types are re-labelled so a browser that ignores `nosniff` still has no
/// document to render; the desktop shows them as text regardless.
fn safe_content_type(media_type: &str) -> String {
    let media = media_type.split(';').next().unwrap_or("").trim();
    match media {
        "text/html"
        | "application/xhtml+xml"
        | "image/svg+xml"
        | "application/javascript"
        | "application/x-javascript"
        | "text/javascript"
        | "application/xml"
        | "text/xml" => "text/plain; charset=utf-8".to_string(),
        "" => "application/octet-stream".to_string(),
        other => other.to_string(),
    }
}

fn sanitize_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "artifact".to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}

/// `GET /v1/evidence/{id}`: one evidence row (the link target of a version's evidence ids).
pub async fn evidence(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(evidence_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let id = EvidenceId::parse(&evidence_id).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            error.to_string(),
            "projection",
        )
    })?;
    // Loading an evidence row reads no bytes, so a deployment without an object store can
    // still serve the link target.
    let objects: Arc<dyn ObjectStore> = match state.objects() {
        Some(objects) => objects.clone(),
        None => Arc::new(NoObjects),
    };
    let mut conn = state.pool().acquire().await.map_err(projection_error)?;
    let evidence = EvidenceStore::new(objects.as_ref())
        .load(&mut conn, &scope.tenant_id, &id)
        .await
        .map_err(|error| artifact_error(error, "projection"))?;
    Ok(Json(json!({
        "id": evidence.id.to_canonical(),
        "workspace_id": evidence.workspace_id,
        "run_id": evidence.run_id,
        "step_id": evidence.step_id,
        "effect_id": evidence.effect_id,
        "kind": evidence.kind,
        "content_digest": evidence.content_digest.to_string(),
        "has_bytes": evidence.object_key.is_some(),
        "inline_summary": evidence.inline_summary,
        "captured_at": evidence.captured_at,
        "captured_by": evidence.captured_by,
        "redaction_applied": evidence.redaction_applied,
    })))
}

/// An object store that holds nothing: for metadata-only reads on a deployment without one.
struct NoObjects;

#[async_trait::async_trait]
impl ObjectStore for NoObjects {
    async fn put(
        &self,
        _key: &ObjectKey,
        _bytes: &[u8],
        _content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::Transport(
            "no object store is configured".to_string(),
        ))
    }
    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::NotFound(key.as_str().to_string()))
    }
    async fn exists(&self, _key: &ObjectKey) -> Result<bool, ObjectStoreError> {
        Ok(false)
    }
    async fn delete(&self, _key: &ObjectKey) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}
