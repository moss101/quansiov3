//! What every output tool host shares (ADP-006): the call context, typed refusals, and the two
//! operations that touch the artifact authority — reading a source and committing an output with
//! its quality record.
//!
//! Every write goes through one transaction with the tenant context set inside it, so a version, its
//! artifact row and its grants either all land or none do, and row-level security sees the tenant on
//! every statement.

use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::PgPool;

use super::config::OutputsConfig;
use super::ports::RenderPort;
use super::quality::{Input, QualityRecord};
use crate::artifacts::CapturedBy;
use crate::artifacts::{
    Artifact, ArtifactError, ArtifactGrant, ArtifactId, ArtifactKind, ArtifactStore,
    ArtifactVersion, ArtifactVersionId, EvidenceContent, EvidenceKind, EvidenceStore, GrantLevel,
    NewArtifact, NewEvidence, NewVersion, ObjectStore, Origin, OriginKind, ProducedBy, Retention,
};
use crate::control::schema;
use crate::runtime::turn_loop::{HostDispatch, HostOutcome};

/// What the output tool hosts are built over.
#[derive(Clone)]
pub struct Env {
    /// The database.
    pub pool: PgPool,
    /// The object store.
    pub objects: Arc<dyn ObjectStore>,
    /// `config/outputs.yaml`.
    pub config: Arc<OutputsConfig>,
    /// The worker-side layout, when one is composed.
    pub render: Option<Arc<dyn RenderPort>>,
}

/// One call.
pub struct Ctx<'a> {
    /// What the hosts are built over.
    pub env: &'a Env,
    /// The dispatch being executed.
    pub dispatch: &'a HostDispatch,
    /// The principal the call acts as: the person the run acts for, or the run itself.
    pub principal: String,
}

impl<'a> Ctx<'a> {
    /// The context for a dispatch.
    #[must_use]
    pub fn new(env: &'a Env, dispatch: &'a HostDispatch) -> Self {
        let principal = dispatch
            .acting_user_id
            .clone()
            .unwrap_or_else(|| format!("run:{}", dispatch.run_id));
        Self {
            env,
            dispatch,
            principal,
        }
    }

    /// A string argument.
    #[must_use]
    pub fn arg_str(&self, name: &str) -> Option<&'a str> {
        self.dispatch.args.get(name).and_then(Value::as_str)
    }
}

/// A typed refusal the model sees (nothing was produced, a retry will not help).
#[must_use]
pub fn refuse(code: &str, rule: &str, message: impl Into<String>) -> HostOutcome {
    refuse_with(code, rule, message, None)
}

/// A typed refusal with machine-readable `details`.
#[must_use]
pub fn refuse_with(
    code: &str,
    rule: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> HostOutcome {
    let mut error = json!({ "code": code, "rule": rule, "message": message.into() });
    if let Some(details) = details {
        error["details"] = details;
    }
    HostOutcome::Failure {
        retryable: false,
        error,
        evidence_ids: Vec::new(),
    }
}

/// A worker, binary or provider that is not composed: nothing was produced and nothing is simulated.
#[must_use]
pub fn unavailable(adapter: &str, detail: impl Into<String>, env: Option<&str>) -> HostOutcome {
    let mut details = json!({ "adapter": adapter });
    if let Some(env) = env {
        details["env"] = Value::String(env.to_string());
    }
    refuse_with(
        "ADAPTER_UNAVAILABLE",
        &format!("output.{adapter}.unavailable"),
        detail,
        Some(details),
    )
}

/// A failure of the store, mapped the way the artifact tools map it.
#[must_use]
pub fn store_failure(error: &ArtifactError) -> HostOutcome {
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

/// A database failure.
#[must_use]
pub fn database_failure(error: &impl std::fmt::Display) -> HostOutcome {
    HostOutcome::Failure {
        retryable: true,
        error: json!({ "code": "INTERNAL", "rule": "output.database", "message": error.to_string() }),
        evidence_ids: Vec::new(),
    }
}

/// A success.
#[must_use]
pub fn success(
    output: Value,
    evidence_ids: Vec<String>,
    remote_ref: Option<String>,
) -> HostOutcome {
    HostOutcome::Success {
        output,
        evidence_ids,
        remote_ref,
    }
}

/// The stable URI of an artifact: it names the artifact, not a version.
#[must_use]
pub fn artifact_uri(artifact_id: &str) -> String {
    format!("quansio://artifact/{artifact_id}")
}

/// A source read from the store.
pub struct Loaded {
    /// The artifact.
    pub artifact: Artifact,
    /// Its current version.
    pub version: ArtifactVersion,
    /// That version's bytes, digest-verified.
    pub bytes: Vec<u8>,
}

impl Loaded {
    /// This version as the input of a quality record.
    #[must_use]
    pub fn input(&self) -> Input {
        Input {
            artifact_id: self.artifact.id.to_canonical(),
            version_id: self.version.id.to_canonical(),
            content_digest: self.version.content_digest.to_string(),
        }
    }
}

/// Read an artifact's current version for the call's principal, checking it belongs to the run's
/// workspace and has the kind (and media type) the tool expects.
///
/// # Errors
/// A refusal outcome, ready to return.
pub async fn read_source(
    ctx: &Ctx<'_>,
    artifact_id: &str,
    kind: ArtifactKind,
    media_type: Option<&str>,
) -> Result<Loaded, HostOutcome> {
    let id = ArtifactId::parse(artifact_id).map_err(|_| {
        refuse(
            "VALIDATION_SCHEMA",
            "artifact.id",
            format!("{artifact_id:?} is not an artifact id"),
        )
    })?;
    let store = ArtifactStore::new(ctx.env.objects.as_ref());
    let mut tx = begin(ctx).await?;
    let artifact = store
        .load(&mut tx, &ctx.dispatch.tenant_id, &ctx.principal, &id)
        .await
        .map_err(|error| store_failure(&error))?;
    if artifact.workspace_id != ctx.dispatch.workspace_id {
        return Err(refuse(
            "SCOPE_FORBIDDEN",
            "artifact.workspace",
            format!("{artifact_id} is not an artifact of this run's workspace"),
        ));
    }
    if artifact.kind != kind {
        return Err(refuse(
            "VALIDATION_SCHEMA",
            "artifact.kind",
            format!(
                "{artifact_id} is a {} artifact; this tool needs a {}",
                artifact.kind.as_str(),
                kind.as_str()
            ),
        ));
    }
    let Some(version_id) = artifact.current_version_id else {
        return Err(refuse(
            "CONFLICT_STATE",
            "artifact.version",
            format!("{artifact_id} has no version yet"),
        ));
    };
    let version = store
        .load_version(
            &mut tx,
            &ctx.dispatch.tenant_id,
            &ctx.principal,
            &version_id,
        )
        .await
        .map_err(|error| store_failure(&error))?;
    if let Some(wanted) = media_type {
        if version.media_type != wanted {
            return Err(refuse(
                "VALIDATION_SCHEMA",
                "artifact.media_type",
                format!(
                    "{artifact_id} holds {}; this tool needs {wanted}",
                    version.media_type
                ),
            ));
        }
    }
    let bytes = store
        .fetch_bytes(
            &mut tx,
            &ctx.dispatch.tenant_id,
            &ctx.principal,
            &version.id,
        )
        .await
        .map_err(|error| store_failure(&error))?;
    tx.commit()
        .await
        .map_err(|error| database_failure(&error))?;
    Ok(Loaded {
        artifact,
        version,
        bytes,
    })
}

/// Begin a transaction with the call's tenant context set inside it.
///
/// # Errors
/// A refusal outcome.
pub async fn begin(
    ctx: &Ctx<'_>,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, HostOutcome> {
    let mut tx = ctx
        .env
        .pool
        .begin()
        .await
        .map_err(|error| database_failure(&error))?;
    schema::set_tenant_context(&mut tx, &ctx.dispatch.tenant_id)
        .await
        .map_err(|error| database_failure(&error))?;
    Ok(tx)
}

/// What to store.
pub struct OutputSpec {
    /// The role that ties the artifact to its source (`deck_pdf:<art_…>`).
    pub role: String,
    /// The kind a new artifact is made with.
    pub kind: ArtifactKind,
    /// The title a new artifact is made with.
    pub title: String,
    /// The media type of the bytes.
    pub media_type: String,
    /// The bytes.
    pub bytes: Vec<u8>,
    /// The quality record to write with the version.
    pub record: QualityRecord,
}

/// What was stored.
pub struct Committed {
    /// The artifact.
    pub artifact_id: ArtifactId,
    /// The version.
    pub version: ArtifactVersion,
    /// Whether identical bytes were already stored for the artifact.
    pub replayed: bool,
}

/// Add a version to the artifact that holds this role in the workspace, creating the artifact the
/// first time, and write the quality record with it.
///
/// # Errors
/// A refusal outcome.
pub async fn commit_output(ctx: &Ctx<'_>, spec: OutputSpec) -> Result<Committed, HostOutcome> {
    let store = ArtifactStore::new(ctx.env.objects.as_ref());
    let tenant = &ctx.dispatch.tenant_id;
    let mut tx = begin(ctx).await?;
    let existing = store
        .find_by_role(
            &mut tx,
            tenant,
            &ctx.principal,
            &ctx.dispatch.workspace_id,
            &spec.role,
        )
        .await
        .map_err(|error| store_failure(&error))?;
    let artifact_id = match existing {
        Some(artifact) if artifact.kind == spec.kind => artifact.id,
        Some(artifact) => {
            return Err(refuse(
                "CONFLICT_STATE",
                "artifact.kind",
                format!(
                    "the artifact for role {} is a {} artifact, not a {}",
                    spec.role,
                    artifact.kind.as_str(),
                    spec.kind.as_str()
                ),
            ))
        }
        None => {
            store
                .create(
                    &mut tx,
                    tenant,
                    &ctx.principal,
                    &NewArtifact {
                        workspace_id: ctx.dispatch.workspace_id.clone(),
                        kind: spec.kind,
                        title: spec.title.clone(),
                        role: Some(spec.role.clone()),
                        origin: Origin {
                            kind: OriginKind::Run,
                            reference: ctx.dispatch.run_id.clone(),
                        },
                        grants: vec![ArtifactGrant {
                            principal: ctx.principal.clone(),
                            level: GrantLevel::Write,
                        }],
                        retention: Retention::default(),
                    },
                )
                .await
                .map_err(|error| store_failure(&error))?
                .id
        }
    };
    let stored = store
        .add_version(
            &mut tx,
            tenant,
            &ctx.principal,
            &artifact_id,
            &NewVersion {
                bytes: spec.bytes,
                media_type: spec.media_type,
                expected_digest: None,
                produced_by: ProducedBy {
                    run_id: Some(ctx.dispatch.run_id.clone()),
                    step_id: Some(ctx.dispatch.step_id.clone()),
                    user_id: ctx.dispatch.acting_user_id.clone(),
                },
                quality_record: Some(spec.record.to_json()),
            },
        )
        .await
        .map_err(|error| store_failure(&error))?;
    tx.commit()
        .await
        .map_err(|error| database_failure(&error))?;
    Ok(Committed {
        artifact_id,
        version: stored.version,
        replayed: stored.replayed,
    })
}

/// Find the artifact that holds a role and its current version's metadata, if there is one.
///
/// # Errors
/// A refusal outcome.
pub async fn find_output(
    ctx: &Ctx<'_>,
    role: &str,
) -> Result<Option<(Artifact, ArtifactVersion)>, HostOutcome> {
    let store = ArtifactStore::new(ctx.env.objects.as_ref());
    let tenant = &ctx.dispatch.tenant_id;
    let mut tx = begin(ctx).await?;
    let found = store
        .find_by_role(
            &mut tx,
            tenant,
            &ctx.principal,
            &ctx.dispatch.workspace_id,
            role,
        )
        .await
        .map_err(|error| store_failure(&error))?;
    let Some(artifact) = found else {
        return Ok(None);
    };
    let Some(version_id) = artifact.current_version_id else {
        return Ok(None);
    };
    let version = store
        .load_version(&mut tx, tenant, &ctx.principal, &version_id)
        .await
        .map_err(|error| store_failure(&error))?;
    tx.commit()
        .await
        .map_err(|error| database_failure(&error))?;
    Ok(Some((artifact, version)))
}

/// Read the bytes of a version of an artifact the principal can read.
///
/// # Errors
/// A refusal outcome.
pub async fn read_version_bytes(
    ctx: &Ctx<'_>,
    version_id: &ArtifactVersionId,
) -> Result<Vec<u8>, HostOutcome> {
    let store = ArtifactStore::new(ctx.env.objects.as_ref());
    let mut tx = begin(ctx).await?;
    let bytes = store
        .fetch_bytes(&mut tx, &ctx.dispatch.tenant_id, &ctx.principal, version_id)
        .await
        .map_err(|error| store_failure(&error))?;
    tx.commit()
        .await
        .map_err(|error| database_failure(&error))?;
    Ok(bytes)
}

/// Record a screenshot as evidence of what a check measured.
///
/// # Errors
/// A refusal outcome.
pub async fn record_screenshot(
    ctx: &Ctx<'_>,
    png: Vec<u8>,
    summary: String,
    adapter: &str,
) -> Result<String, HostOutcome> {
    let store = EvidenceStore::new(ctx.env.objects.as_ref());
    let mut tx = begin(ctx).await?;
    let evidence = store
        .record(
            &mut tx,
            &ctx.dispatch.tenant_id,
            &NewEvidence {
                workspace_id: ctx.dispatch.workspace_id.clone(),
                run_id: Some(ctx.dispatch.run_id.clone()),
                step_id: Some(ctx.dispatch.step_id.clone()),
                effect_id: Some(ctx.dispatch.effect_id.clone()),
                kind: EvidenceKind::Screenshot,
                content: EvidenceContent::Bytes {
                    bytes: png,
                    media_type: "image/png".to_string(),
                },
                inline_summary: Some(summary),
                captured_at: None,
                captured_by: CapturedBy {
                    kind: "adapter".to_string(),
                    id: adapter.to_string(),
                },
                redaction_applied: false,
            },
        )
        .await
        .map_err(|error| store_failure(&error))?;
    tx.commit()
        .await
        .map_err(|error| database_failure(&error))?;
    Ok(evidence.id.to_canonical())
}
