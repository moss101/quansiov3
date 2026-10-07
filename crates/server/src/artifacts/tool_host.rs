//! The server-hosted artifact tools (`artifact.create`, `artifact.update`; CAP-003 on CORE-007's
//! store, DOMAIN.md §7.5, §10.1).
//!
//! A run makes a document, spreadsheet, presentation, code or data artifact by proposing one of
//! these governed tools: the runtime validates the arguments against the registry, reserves the
//! `content.upload` effect and dispatches here. The host is where the authority checks what it
//! is about to store:
//!
//! * the source is carried in the call (`content`, UTF-8, or `content_base64`) and must hash to
//!   the declared `content_digest` — the digest the effect's idempotency key is derived from;
//! * the source must pass [`super::formats::check`] for its kind — a malformed, empty or
//!   chat-transcript source is refused with a typed, non-retryable failure the model sees, and
//!   no version is created;
//! * the version is added through [`ArtifactStore`], content-addressed and immutable, produced
//!   by the run and its step, and granted to the acting user so the person who asked can read
//!   and edit it. An update adds a version on top of the current one (`parent_version_id`).
//!
//! [`ServerToolHost`] routes the artifact tools here and every other tool to the host it wraps.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use quansio_core::Digest;
use serde_json::{json, Value};
use sqlx::PgPool;

use super::formats::{self, default_media_type};
use super::outputs::{is_output_tool, tool_owned, OutputToolHost};
use super::{
    ArtifactError, ArtifactGrant, ArtifactId, ArtifactKind, ArtifactStore, GrantLevel, NewArtifact,
    NewVersion, ObjectStore, Origin, OriginKind, ProducedBy, Retention,
};
use crate::runtime::turn_loop::{HostDispatch, HostFailure, HostOutcome, ToolHostPort};

/// The task that owns the server-hosted artifact tools (`dispatch::ARTIFACT_HOST_OWNER`).
pub const ARTIFACT_TOOL_OWNER: &str = "CORE-007";
/// The largest source a tool call may carry.
pub const MAX_TOOL_SOURCE_BYTES: usize = 5 * 1024 * 1024;

/// Executes `artifact.create` and `artifact.update`.
#[derive(Clone)]
pub struct ArtifactToolHost {
    pool: PgPool,
    objects: Arc<dyn ObjectStore>,
}

impl std::fmt::Debug for ArtifactToolHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactToolHost").finish_non_exhaustive()
    }
}

fn refusal(code: &str, rule: &str, detail: impl Into<String>) -> HostOutcome {
    HostOutcome::Failure {
        retryable: false,
        error: json!({ "code": code, "rule": rule, "message": detail.into() }),
        evidence_ids: Vec::new(),
    }
}

fn parse_kind(value: &str) -> Option<ArtifactKind> {
    serde_json::from_value(Value::String(value.to_string())).ok()
}

/// The source bytes a call carries, bound to its declared digest.
fn source(args: &Value) -> Result<Vec<u8>, HostOutcome> {
    let text = args.get("content").and_then(Value::as_str);
    let encoded = args.get("content_base64").and_then(Value::as_str);
    let bytes = match (text, encoded) {
        (Some(text), None) => text.as_bytes().to_vec(),
        (None, Some(encoded)) => STANDARD.decode(encoded.trim()).map_err(|error| {
            refusal(
                "VALIDATION_SCHEMA",
                "artifact.content",
                format!("content_base64: {error}"),
            )
        })?,
        _ => {
            return Err(refusal(
                "VALIDATION_SCHEMA",
                "artifact.content",
                "give the source as exactly one of content (UTF-8) or content_base64",
            ))
        }
    };
    if bytes.len() > MAX_TOOL_SOURCE_BYTES {
        return Err(refusal(
            "VALIDATION_BOUNDS",
            "artifact.content",
            format!("a source is at most {MAX_TOOL_SOURCE_BYTES} bytes"),
        ));
    }
    let declared = args
        .get("content_digest")
        .and_then(Value::as_str)
        .unwrap_or("");
    let actual = Digest::of(&bytes);
    if declared != actual.as_str() {
        return Err(refusal(
            "VALIDATION_SCHEMA",
            "artifact.content_digest",
            format!("content_digest {declared:?} is not the digest of the source ({actual})"),
        ));
    }
    Ok(bytes)
}

fn store_failure(error: &ArtifactError) -> HostOutcome {
    let code = match error {
        ArtifactError::AccessDenied { .. } => "SCOPE_FORBIDDEN",
        ArtifactError::NotFound { .. } => "NOT_FOUND",
        ArtifactError::Deleted(_) => "CONFLICT_STATE",
        ArtifactError::DigestMismatch { .. } => "VALIDATION_SCHEMA",
        _ => "INTERNAL",
    };
    HostOutcome::Failure {
        retryable: code == "INTERNAL",
        error: json!({ "code": code, "rule": "artifact.store", "message": error.to_string() }),
        evidence_ids: Vec::new(),
    }
}

impl ArtifactToolHost {
    /// A host over the database and the artifact object store.
    #[must_use]
    pub fn new(pool: PgPool, objects: Arc<dyn ObjectStore>) -> Self {
        Self { pool, objects }
    }

    /// The principal the call acts as: the person the run acts for, or the run itself.
    fn principal(dispatch: &HostDispatch) -> String {
        dispatch
            .acting_user_id
            .clone()
            .unwrap_or_else(|| format!("run:{}", dispatch.run_id))
    }

    async fn create(&self, dispatch: &HostDispatch) -> Result<HostOutcome, HostFailure> {
        let args = &dispatch.args;
        let Some(kind) = args
            .get("kind")
            .and_then(Value::as_str)
            .and_then(parse_kind)
        else {
            return Ok(refusal(
                "VALIDATION_SCHEMA",
                formats::RULE_KIND,
                "kind is one of document, spreadsheet, presentation, code, data, image, audio, \
                 video, archive, other",
            ));
        };
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if title.is_empty() || title.chars().count() > 200 {
            return Ok(refusal(
                "VALIDATION_BOUNDS",
                "artifact.title",
                "title must be 1..=200 characters",
            ));
        }
        let role = args
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let bytes = match source(args) {
            Ok(bytes) => bytes,
            Err(outcome) => return Ok(outcome),
        };
        let media_type = args
            .get("media_type")
            .and_then(Value::as_str)
            .map_or_else(|| default_media_type(kind).to_string(), str::to_string);
        if let Some(reason) = tool_owned::refuses_create(role, &media_type) {
            return Ok(refusal("VALIDATION_SCHEMA", "artifact.tool_owned", reason));
        }
        let check = formats::check(kind, &media_type, &bytes);
        if !check.ok {
            return Ok(refusal(
                "VALIDATION_SCHEMA",
                &check.rule,
                format!(
                    "the {} source is not valid, so no artifact was created: {}",
                    kind.as_str(),
                    check.detail
                ),
            ));
        }
        let principal = Self::principal(dispatch);
        let store = ArtifactStore::new(self.objects.as_ref());
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|error| HostFailure::Unavailable {
                owner: ARTIFACT_TOOL_OWNER,
                detail: error.to_string(),
            })?;
        let artifact = match store
            .create(
                &mut conn,
                &dispatch.tenant_id,
                &principal,
                &NewArtifact {
                    workspace_id: dispatch.workspace_id.clone(),
                    kind,
                    title: title.to_string(),
                    role: Some(role.to_string()),
                    origin: Origin {
                        kind: OriginKind::Run,
                        reference: dispatch.run_id.clone(),
                    },
                    grants: vec![ArtifactGrant {
                        principal: principal.clone(),
                        level: GrantLevel::Write,
                    }],
                    retention: Retention::default(),
                },
            )
            .await
        {
            Ok(artifact) => artifact,
            Err(error) => return Ok(store_failure(&error)),
        };
        self.add_version(
            dispatch,
            &mut conn,
            &principal,
            &artifact.id,
            bytes,
            media_type,
        )
        .await
        .map(|outcome| with_artifact(outcome, &artifact.id))
    }

    async fn update(&self, dispatch: &HostDispatch) -> Result<HostOutcome, HostFailure> {
        let args = &dispatch.args;
        let Some(artifact_id) = args
            .get("artifact_id")
            .and_then(Value::as_str)
            .and_then(|id| ArtifactId::parse(id).ok())
        else {
            return Ok(refusal(
                "VALIDATION_SCHEMA",
                "artifact.id",
                "artifact_id is not an artifact id",
            ));
        };
        let bytes = match source(args) {
            Ok(bytes) => bytes,
            Err(outcome) => return Ok(outcome),
        };
        let principal = Self::principal(dispatch);
        let store = ArtifactStore::new(self.objects.as_ref());
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|error| HostFailure::Unavailable {
                owner: ARTIFACT_TOOL_OWNER,
                detail: error.to_string(),
            })?;
        let artifact = match store
            .load(&mut conn, &dispatch.tenant_id, &principal, &artifact_id)
            .await
        {
            Ok(artifact) => artifact,
            Err(error) => return Ok(store_failure(&error)),
        };
        if artifact.workspace_id != dispatch.workspace_id {
            return Ok(refusal(
                "SCOPE_FORBIDDEN",
                "artifact.workspace",
                format!(
                    "{} is not an artifact of this run's workspace",
                    artifact_id.to_canonical()
                ),
            ));
        }
        let current = match artifact.current_version_id.as_ref() {
            Some(version) => sqlx::query_scalar::<_, String>(
                "SELECT media_type FROM artifact_versions WHERE id = $1 AND tenant_id = $2",
            )
            .bind(version.to_canonical())
            .bind(&dispatch.tenant_id)
            .fetch_optional(&mut *conn)
            .await
            .ok()
            .flatten(),
            None => None,
        }
        .unwrap_or_else(|| default_media_type(artifact.kind).to_string());
        let media_type = args
            .get("media_type")
            .and_then(Value::as_str)
            .map_or_else(|| current.clone(), str::to_string);
        if let Some(reason) =
            tool_owned::refuses_update(artifact.role.as_deref(), &current, &media_type)
        {
            return Ok(refusal("VALIDATION_SCHEMA", "artifact.tool_owned", reason));
        }
        let check = formats::check(artifact.kind, &media_type, &bytes);
        if !check.ok {
            return Ok(refusal(
                "VALIDATION_SCHEMA",
                &check.rule,
                format!(
                    "the edited {} source is not valid, so no version was added: {}",
                    artifact.kind.as_str(),
                    check.detail
                ),
            ));
        }
        self.add_version(
            dispatch,
            &mut conn,
            &principal,
            &artifact.id,
            bytes,
            media_type,
        )
        .await
    }

    async fn add_version(
        &self,
        dispatch: &HostDispatch,
        conn: &mut sqlx::PgConnection,
        principal: &str,
        artifact_id: &ArtifactId,
        bytes: Vec<u8>,
        media_type: String,
    ) -> Result<HostOutcome, HostFailure> {
        let store = ArtifactStore::new(self.objects.as_ref());
        let stored = match store
            .add_version(
                conn,
                &dispatch.tenant_id,
                principal,
                artifact_id,
                &NewVersion {
                    bytes,
                    media_type,
                    expected_digest: None,
                    produced_by: ProducedBy {
                        run_id: Some(dispatch.run_id.clone()),
                        step_id: Some(dispatch.step_id.clone()),
                        user_id: dispatch.acting_user_id.clone(),
                    },
                    quality_record: None,
                },
            )
            .await
        {
            Ok(stored) => stored,
            Err(error) => return Ok(store_failure(&error)),
        };
        let version_id = stored.version.id.to_canonical();
        Ok(HostOutcome::Success {
            output: json!({
                "artifact_id": artifact_id.to_canonical(),
                "version_id": version_id,
                "version": stored.version.seq,
                "content_digest": stored.version.content_digest.to_string(),
                "replayed": stored.replayed,
            }),
            evidence_ids: Vec::new(),
            remote_ref: Some(version_id),
        })
    }
}

fn with_artifact(outcome: HostOutcome, artifact_id: &ArtifactId) -> HostOutcome {
    match outcome {
        HostOutcome::Success {
            mut output,
            evidence_ids,
            remote_ref,
        } => {
            output["artifact_id"] = Value::String(artifact_id.to_canonical());
            HostOutcome::Success {
                output,
                evidence_ids,
                remote_ref,
            }
        }
        other => other,
    }
}

#[async_trait]
impl ToolHostPort for ArtifactToolHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        match dispatch.tool.as_str() {
            "artifact.create" => self.create(&dispatch).await,
            "artifact.update" => self.update(&dispatch).await,
            other => Err(HostFailure::Refused {
                detail: format!("{other} is not an artifact tool"),
            }),
        }
    }
}

/// The deployable's tool host: the artifact tools run here on the server; every other tool goes
/// to the host this wraps.
#[derive(Clone)]
pub struct ServerToolHost {
    artifacts: ArtifactToolHost,
    outputs: Option<OutputToolHost>,
    other: Arc<dyn ToolHostPort>,
}

impl std::fmt::Debug for ServerToolHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerToolHost").finish_non_exhaustive()
    }
}

impl ServerToolHost {
    /// Route artifact tools to `artifacts`, the rest to `other`.
    #[must_use]
    pub fn new(artifacts: ArtifactToolHost, other: Arc<dyn ToolHostPort>) -> Self {
        Self {
            artifacts,
            outputs: None,
            other,
        }
    }

    /// Also host the output tools (ADP-006): decks, diagrams, printed documents, video,
    /// transcription and the web-dev preview.
    #[must_use]
    pub fn with_outputs(mut self, outputs: OutputToolHost) -> Self {
        self.outputs = Some(outputs);
        self
    }
}

#[async_trait]
impl ToolHostPort for ServerToolHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        if matches!(
            dispatch.tool.as_str(),
            "artifact.create" | "artifact.update"
        ) {
            self.artifacts.execute(dispatch).await
        } else if let Some(outputs) = self
            .outputs
            .as_ref()
            .filter(|_| is_output_tool(&dispatch.tool))
        {
            outputs.execute(dispatch).await
        } else {
            self.other.execute(dispatch).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_bound_to_its_digest_and_carried_one_way() {
        let body = "# Plan\n\nShip.\n";
        let digest = Digest::of(body.as_bytes()).to_string();
        assert_eq!(
            source(&json!({ "content": body, "content_digest": digest })).ok(),
            Some(body.as_bytes().to_vec())
        );
        let encoded = STANDARD.encode(body);
        assert!(source(&json!({ "content_base64": encoded, "content_digest": digest })).is_ok());
        assert!(source(&json!({ "content": body, "content_digest": "0".repeat(64) })).is_err());
        assert!(source(
            &json!({ "content": body, "content_base64": encoded, "content_digest": digest })
        )
        .is_err());
        assert!(source(&json!({ "content_digest": digest })).is_err());
    }
}
